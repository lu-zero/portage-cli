# Short-flag parity with real `emerge`

2026-07-17: quick survey of `man emerge`'s short options against `em`'s own
`Cli`/`MergeFlags`/`DepgraphFlags` struct source (more reliable than a
possibly-truncated `--help` dump). Landed the two obvious gaps; the rest are
noted below, not started.

## Landed

- **`-C`/`--unmerge`** — remove installed packages directly, no dependency
  checking at all (distinct from `depclean`'s automatic orphan cleanup).
  `emerge.rs::unmerge_atoms`, shares its removal core with the existing
  in-place-replace path via `ebuild::unmerge_package`.
- **`-B`/`--buildpkgonly`** — build a binary package straight from the image,
  never merge/install it. Computes CONTENTS/metadata by walking the image and
  registering into a scratch VDB dir (`ebuild::build_binpkg_standalone`),
  matching real portage's own `EbuildBuild.py` model (never calls `merge()`
  for `-B` either). Live/root/VDB genuinely untouched, verified in the
  crossdev-stages sandbox.

Committed as two separate commits: `-C` in `a32c217`, `-B` in `d5d4eb5`
(both also fixed by a Fable review pass to respect `--ask`, which the
first draft missed for `-C`).

- **`-c`/`--depclean`** — reverse-dependency/orphan-detection machinery:
  `depclean.rs`, reads real portage's `_calc_depclean` rather than
  guessing. Reuses `SetResolver::resolve("world")`, `DepEntry::
  evaluate_use`/`Dep::matches_cpv` for the installed-graph walk, and
  `ebuild::unmerge_standalone` (+ preserve-libs) for removal. Documented
  simplifications: no USE-dep bracket verification, no virtual/provider
  indirection, `||` groups walk every branch (over-keep biased, not
  under-keep). Live-verified (`fa1a012`; the preceding `36de084` refactor
  made preserve-libs scan the VDB once per batch instead of once per
  removed package, since depclean's cleanlist can be much bigger than
  `-C`'s typical 1-3 packages).
  - **Found live-verifying this, fixed same session (`9c850f6`)**:
    ordinary `em <atom>` merges never wrote to `var/lib/portage/world`
    at all, which would have made depclean dangerously over-eager
    against a real (non-empty-world) system. Fixed via
    `maint::world::add_atoms` (replace-in-place by Cpn, matching real
    emerge's `_world_atom`), called from `emerge_atoms_inner` after a
    successful non-pretend merge, gated on the same skip-set real
    portage uses. This is also what made `-1`/`--oneshot` do something
    for the first time — it was a parsed-but-unread flag before this
    (same class of bug as the `-K` fix), with nothing to skip until
    world-writing existed.

- **`-1`/`--oneshot`** — landed alongside the world-write fix above
  (`9c850f6`): skips adding the merged atom to world, matching real
  emerge exactly (its only effect).

## Remaining gaps (not started)

- **`-P`/`--prune`** — companion to `--depclean` (remove all but the best
  version of a match, ignoring deps entirely). `depclean.rs`'s machinery
  (`compute_cleanlist`'s protected-set handling, `removal_order`) covers
  most of what this needs now — likely a smaller follow-up than depclean
  itself was, not investigated further yet.
- **`-r`/`--resume`** — replay the last aborted/skipped merge list. Needs
  `em` to persist a resumable plan somewhere between invocations; no such
  state exists today.
- **`-U`/`--changed-use`** — like `-N`/`--newuse` but only rebuilds on a USE
  *change* relative to what's installed, not "profile default changed
  underneath you". `em` has `-N`; check whether the distinction is worth a
  separate flag or whether `-N`'s existing logic already covers this case.
- **`-W`/`--deselect`** — remove atoms from the world file without
  unmerging. `em`'s own `-w` short flag is already taken (`em ebuild`'s
  `--work-dir` override), so this would need a different short form or
  long-only, same reasoning as `--tree` losing its short form to `--target`.
- **`-F`/`--fetch-all-uri`** — minor, fetch every SRC_URI including
  unused-by-current-USE ones. Low priority.

None of these were asked for beyond the `-C`/`-B` pair; listed here so a
future "what's next" survey doesn't have to re-derive the man-page diff from
scratch.
