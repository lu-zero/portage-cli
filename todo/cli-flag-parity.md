# Short-flag parity with real `emerge`

STATUS: **all short-flag-parity items landed** — `-C`/`-B`/`-c`/`-1`/`-uD`/
`-N`/`-U`/`-P`/`-W`/`-F` (all 2026-07-21 or earlier), plus **`-r`/`--resume`
landed 2026-07-22**. See [[PENDING]] 2026-07-18 queue rows 5–6.

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

## Landed later (resolver flags)

- **`-u`/`--update` + `-D`/`--deep` in-slot upgrades** — 2026-07-18
  ([[deep-in-slot-upgrades]]): `prefer_update` on `update && deep`; host-
  satisfied BDEPEND retained so build tools upgrade. Slot bumps remain
  `prefer_newest_slot` ([[deep-slot-bump]]). Shallow `-p`/`-up` stay Favor.
- **`-N`/`--newuse` and `-U`/`--changed-use`** — 2026-07-18 ([[newuse]]):
  USE/IUSE drift → `InstalledPolicy::Rebuild`, same-CPV `[R]` (or upgrade
  with `-uD`). `-U` ignores pure IUSE add/drop.

## Remaining gaps (not started)

- **`-f`/`--fetchonly`** — ✅ done 2026-07-18: plan as usual, then download
  distfiles (or remote binpkgs under `-g`) only — no build/install/env-update/
  world write. `PhaseGroup::FetchOnly` + `act_on_package` short-circuit.
- **`-P`/`--prune`** — ✅ done 2026-07-21. Companion to `-C`/`--unmerge`
  (not `--depclean`'s dependency-graph machinery, in the end): matches atoms
  the same way `-C` does (`match_installed_atoms`, shared by both), then
  drops each matched `Cpn`'s single highest version
  (`drop_highest_version_per_cpn`) so only older versions are removal
  candidates — real emerge's own "removes all but the highest installed
  version" rule. Shares the whole preview/`--pretend`/`--ask`/preserve-libs/
  removal/env-update sequence with `-C` via a new `remove_matched_packages`
  core (`emerge.rs`) instead of duplicating it. No dependency graph, same
  caveat as `-C`. Requires at least one atom (no bare "prune everything"
  form, matching `-C`'s own requirement).
- **`-W`/`--deselect`** — ✅ done 2026-07-21. World-file-only removal, no
  unmerge, no dependency graph, no shell/build setup at all —
  `maint::world::remove_atoms` (new, mirrors `add_atoms`'s replace-by-`Cpn`
  matching) takes plain atoms or `@set` names and drops matching world-file
  lines; a token matching nothing is a silent no-op (matches real emerge).
  `-W` was free at the top level (no flag collision, unlike the module's
  earlier guess about `-w`/`--work-dir` — that's `em ebuild`'s own
  subcommand-scoped flag, a different clap::Args group, no clash).
- **`-F`/`--fetch-all-uri`** — ✅ done 2026-07-21. Unlike every other merge
  flag so far, this one changes behavior *inside* the fetch phase itself
  (which `SRC_URI` entries count), not just which phases run — so it needed
  to reach `run_fetch` (`ebuild.rs`), the deepest point in the call chain
  (`MergeFlags::fetch_all_uri` → `act_on_package` → `build_and_merge` →
  `PhaseGroup::FetchOnly { all_uri }` → `run_inner` → `run_one_phase` →
  `run_fetch`). New `DistfileResolver::resolve_all` (`portage-distfiles`)
  descends every `UseConditional` branch unconditionally instead of gating
  on live USE. Treated identically to `-f` at every "did we actually
  install anything" gate (pkgdir-writable preflight, env-update, done/fail
  messaging) — real `-F` never builds either.
- **`-r`/`--resume`** — ✅ done 2026-07-22. Studied real portage's own
  mechanism (`portage/util/mtimedb.py`, `_emerge/actions.py`,
  `_emerge/Scheduler.py`) rather than guessing, then deliberately
  simplified: portage persists a fully-resolved, pinned `mergelist`
  (`[pkg_type, pkg_root, cpv, action]`), pruned entry-by-entry as packages
  complete. `em` instead persists just enough to **replay the invocation**
  — the raw atoms (pre-`@set`-expansion) and the effective merge/depgraph
  flags (`maint::resume::ResumeState`, `<root>/var/cache/edb/em-resume.json`
  — same directory real portage's own `mtimedb` lives in, distinct
  filename/shape, never touches portage's actual file) — and re-resolves
  fresh on `-r`. Already-merged packages are skipped for free by the
  existing VDB-presence check in `merge::merge_sequential`/`merge_parallel`,
  so there's no pinned-list staleness to manage, and a changed repo between
  runs self-heals instead of needing portage's own `resume_depgraph`
  re-validation machinery.

  One level of backup, matching portage's `resume`/`resume_backup` pair:
  starting a *new* (non-`-r`) top-level merge while a `resume` entry is
  still pending backs it up first (`maint::resume::save`'s `is_resume`
  flag), so running an unrelated command doesn't silently discard an
  interrupted job; `-r` consults `resume` first, falling back to (and
  promoting) `resume_backup` otherwise. `em maint cleanresume` — a CLI stub
  that already existed (`MaintCommand::Cleanresume`,
  `dispatch.rs`) waiting for exactly this — reports occupied slots and,
  with `--fix`, clears both (real portage's own `emaint cleanresume`
  equivalent).

  **Scope decision: no `--skipfirst`.** Real emerge's `--skipfirst` needs
  the first-failed cpv reported back structurally from `run_merge_plan`
  (currently a formatted `bail!`, no caller-visible failure list) —
  deferred as a separate, smaller follow-up if ever needed. A stuck package
  can already be skipped manually via `-X`/`--exclude` on the resumed
  invocation (`em -r -X stuck/atom`), which covers the practical use case.

  **Found and fixed live-testing this**: `-X`/`--exclude` was itself a
  dead flag — parsed and merged between flag sources
  (`crossdev::merge_merge_flags_fields`) but never actually read anywhere
  in resolution (same class of bug as the earlier `-K` fix). Fixed by
  adding `exclude: &'a [String]` to `DepgraphOpts` and filtering `order`
  (`query/depgraph/mod.rs`) against the parsed exclude atoms right after
  it's finalized — before *any* consumer (the `-p`/`--tree`/`--json`
  preview, and the final `PlannedMerge` merge-loop plan) is built from it,
  so every display and the actual merge agree, not just the merge itself.
  Deliberately a post-solve filter, not integrated into the pubgrub solve:
  if an excluded package is a genuine hard dependency of something else
  still in the plan, that other package's own preflight/build fails with a
  clear missing-dependency error instead of the solver reporting the
  conflict up front the way real portage's own backtracking can. Live
  sandbox-verified: `-p -X` no longer shows the excluded package, `-r -X`
  correctly merges only the non-excluded remainder and clears resume.

  The current invocation's own merge/depgraph flags win over the persisted
  ones where set (`em -r --keep-going`), reusing the exact same
  "args-over-base" precedence `crossdev::merge_merge_flags_with` already
  used for subcommand-vs-global flags — factored into pure
  `merge_merge_flags_fields`/`merge_depgraph_flags_fields` helpers so both
  call sites share one implementation.

None of these were asked for beyond the `-C`/`-B` pair; listed here so a
future "what's next" survey doesn't have to re-derive the man-page diff from
scratch.
