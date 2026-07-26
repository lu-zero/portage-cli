# em is blind to ebuilds with no md5-cache entry

STATUS: **found 2026-07-26, independently confirmed; nothing implemented.**
Highest value/effort item currently open — it silently removes real versions
from every resolve, including world members with available upgrades.

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

## Shape of the fix

Portage's rule: the cache is an *optimisation*, not the source of truth. When
an ebuild has no valid cache entry (missing, or `_eclasses_`/mtime stale), it
sources the ebuild to regenerate metadata.

Open questions before implementing:

- **Where.** `load_repos` is on the hot path (the co-solve fixpoint rebuilds the
  provider up to ~8×), and sourcing ebuilds is orders of magnitude slower than
  reading cache. Sourcing *only* the uncached minority (44-63 of ~40k) is
  probably affordable, but it must happen once, outside the fixpoint.
- **Or refuse to guess.** A cheaper first step: keep the version out of the
  solve but *report* it — "N versions skipped, no metadata cache" — so the
  blindness stops being silent. Strictly worse than sourcing, but a large
  improvement over today and much cheaper. Could ship first.
- **Staleness, not just absence.** This note only measured *missing* entries.
  A *stale* entry (ebuild newer than its cache) is the same class of bug and
  probably more common; check whether `CacheEntry` validation compares mtimes
  or `_eclasses_` at all today.
- `em regen` exists (see [[regen-jobs-guidance]]) — establish whether running
  it fixes these locally, which would tell us whether this is "the user's cache
  is stale" or "em cannot handle a legitimately cacheless ebuild". Both need
  handling, but the priority differs.

## Verification

- `em -p '=sys-fs/btrfs-progs-7.1'` resolves instead of erroring.
- `em -puD @world` gains `dev-python/sphinx` and drops the docutils conflict
  without any repair-loop work.
- Row counts vs `emerge -pu --exclude app-containers/incus @world` (182, rc=0)
  and `-puD` (305, rc=0) — read the measurement trap in
  [[selective-resolution]] first.
- Resolve timing must not regress materially; `em -p @world` is ~1 s today.
