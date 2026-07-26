# em is blind to ebuilds with no md5-cache entry

STATUS: **fixed 2026-07-26 on branch `md5-cache-fallback`** (`d6a7b82`
visibility, `4489e8d` memoisation + staleness), not yet merged. Two follow-ups
below are still open.

## Symptom

```
$ ls /var/db/repos/gentoo/sys-fs/btrfs-progs/ | grep 7.1
btrfs-progs-7.1.ebuild                      # the ebuild exists

$ ls /var/db/repos/gentoo/metadata/md5-cache/sys-fs/ | grep btrfs-progs-7.1
                                            # no cache entry

$ em -p '=sys-fs/btrfs-progs-7.1'
error: =sys-fs/btrfs-progs-7.1: no ebuilds in ::gentoo or overlays

$ emerge -p '=sys-fs/btrfs-progs-7.1'
rc=0, resolves fine
```

em reports the version as **nonexistent**. Not masked, not filtered —
invisible. Portage sources the ebuild when the cache lacks an entry; em does
not.

## Scale

A partial scan (first 20k ebuilds) found **44** ebuilds with no md5-cache
entry; a full sweep reported **63**. Confirmed affected here:
`sys-fs/btrfs-progs-7.1`, `dev-python/cython-3.2.9`, `dev-python/btrfsutil-7.1`,
and a run of `virtual/dist-kernel-*`.

`sys-fs/btrfs-progs` is a **world member**, so this is not a corner case: em's
`@world` plan is missing a real available upgrade and never says why.

## Mechanism

The main repo is loaded md5-cache-only (`portage-repo/src/overlay.rs:5`).
`load_repos` (`portage-resolve/src/repo.rs:936`) builds `RepoData.versions`
purely from `cache_entries_parallel`, so a CPV with no cache entry never enters
the map at all. Every downstream consumer — `versions_for`, `slots_for`,
`target_package`, `filter_reasons_for` — is therefore structurally unable to
see it, which is why it surfaces as `NoEbuilds` rather than as a mask.

This is *why* the reporting is so misleading: `filter_reasons_for_atom` returns
empty (no candidate matched the atom), and the new classification correctly
concludes "no ebuilds". The classification is right; its input is wrong.

## Knock-on effects found so far

- **The docutils/sphinx "resolver gap" is this bug, not a resolver defect.**
  `emerge -puD @world` pulls `dev-python/sphinx-9.1.0-r1` because
  `sys-fs/btrfs-progs-7.1` has BDEPEND `|| ( ( python:3.14 sphinx[…]
  sphinx-rtd-theme[…] ) … )`. em never sees `btrfs-progs-7.1`, so it never
  walks that BDEPEND, so sphinx is never a graph node, so `docutils-0.23`
  breaks its `<` bound with nothing to repair. Fix the visibility and that
  conflict disappears with no chain-completion machinery at all. See
  [[slot-chain-completion]], which had wrongly unified the two.
- Any `em -p @world`/row-count comparison against emerge is measuring this too.
  Re-baseline the numbers in [[selective-resolution]] after fixing.

## What landed

Portage's rule is that the cache is an *optimisation*, not the source of truth:
an ebuild with no valid entry gets sourced. em now does the same, narrowed so
the cost stays off the hot path.

`overlay.rs` already had the whole chain (layered lookup → master symlink →
source → `put_secondary`); it was declared main-repo-exempt on the premise that
rsync always ships a complete cache. The per-ebuild body is now shared, and
`primary_entries` runs it over *suspects* only:

- no cache entry (the original bug — 63 CPVs here), or
- newer than the last sync, which catches a **present-but-stale** entry that the
  bulk read would otherwise trust blindly (142 of 32,615 ebuilds here).

Validation is by md5 of file contents (`_md5_` + `_eclasses_` digests), never
mtime; mtime only decides *which* files to digest.

Finding suspects costs a tree walk, so it runs concurrently with the bulk read
and its outcome is memoised in a sidecar keyed on a sync stamp (`timestamp.chk`
plus the repo dir's own mtime, so rsync and git trees both invalidate). The
sidecar lists only what the in-tree cache genuinely cannot serve — 64, not every
suspect at 165. Any inconsistency (stamp mismatch, unparseable line, secondary
missing a listed entry) falls through to a full rescan: it costs time, never
correctness.

Measured, `em -p @world` median of 12: **0.98s before, 1.01s after**, inside the
baseline's own 0.97-1.03 spread. First resolve after a sync pays 1.14s.

## Open follow-up: regenerate on sync

Better still, do the reindex **at sync time** rather than on the next resolve,
so only *locally modified* ebuilds are ever suspect afterwards and no ordinary
command pays the 1.14s cold hit. `em regen` already exists
([[regen-jobs-guidance]]), and the sync stamp this fix added is the natural
trigger.

Note `em maint sync` is currently `bail!("not implemented: emaint sync")`
(`portage-cli/src/dispatch.rs`), so there is no in-em sync to hook yet — this
either waits for that, or hangs off stamp-change detection (regenerate eagerly
when the stamp moves, instead of lazily on first use).

## Open follow-up: lazy validation (option D) — and the memory angle

The deeper asymmetry: **portage validates lazily, per-cpv, only for packages it
actually considers; em loads the whole tree eagerly into `RepoData`.** So any
per-ebuild work in em is a 32,615× operation where portage's is a few hundred.

Going lazy would cut more than time. `RepoData` currently holds a parsed
`CacheEntry` for **every** version in the tree for the whole resolve, when a
typical closure touches a few hundred CPNs. Loading (or retaining) only what the
solver reaches should cut resident memory substantially — worth measuring before
committing to it, since the closure is discovered incrementally and a lazy
provider changes the ingestion model that `new_for_targets_with_bdeps_and_slot_map`
(closure-seeded, see `portage-cli/src/query/depgraph/mod.rs`) is built around.

This is a redesign of the eager load, not a tweak — file it separately if it
gets picked up.

## Verification

- `em -p '=sys-fs/btrfs-progs-7.1'` resolves instead of erroring.
- `em -puD @world` gains `dev-python/sphinx` and drops the docutils conflict
  without any repair-loop work.
- Row counts vs `emerge -pu --exclude app-containers/incus @world` (182, rc=0)
  and `-puD` (305, rc=0) — read the measurement trap in
  [[selective-resolution]] first.
- Resolve timing must not regress materially; `em -p @world` is ~1 s today.
