# pubgrub version-choice heuristic — `@system` silently holds back versions

Found 2026-08-17 comparing `emerge -vp @system` against `em -vp @system` on a
live host. Two related solver bugs surfaced from the same comparison; this
note covers the harder one (the false-positive blocker was a separate root
cause and is already fixed, `2802c1b`).

## The bug

`em -vp @system` was missing `sys-apps/acl`/`virtual/acl` upgrades that real
emerge includes, and silently picked `net-misc/rsync-3.4.4` instead of the
true newest `3.5.0` — no error, no warning, just a quietly worse (older)
solution than the true optimum.

## Root cause (confirmed by two independent investigations + live repro)

`portage-atom-pubgrub` implements pubgrub's `DependencyProvider` trait.
`prioritize` (`provider/solve.rs:28-39`) is an unmodified copy of pubgrub's
own minimum-remaining-values default — decide the package with the *fewest*
in-range versions first — with no Portage-specific tiering. pubgrub's own
docs are explicit that `prioritize` **is** the "who wins a conflict" policy:
ties are broken in favor of the higher-priority package, and lower-priority
decisions get backtracked away first.

`virtual/acl` has only 2 in-range versions (`0-r2`, `2.4.0`) vs `net-misc/
rsync`'s 3+, so pubgrub decides `virtual/acl` *first* — before any dependent
(rsync) has been examined — and `choose_version`'s `InstalledPolicy::Favor`
branch (`solve.rs:89-111`) commits it to the already-installed `0-r2`
because `range` is still `any` at that point. `rsync`'s `acl? (
>=virtual/acl-2.4.0 )` constraint arrives later as an incompatibility that's
**already satisfied** by the acl decision — plain unit propagation just
derives "not rsync-3.5.0". No conflict is ever raised, so there's no clause
to learn from and nothing for backtracking to fix; the returned solution is
fully sound, just not the greedy-newest one Portage's top-down depgraph
produces.

Real Portage's `depgraph._add_pkg_deps` traverses parents-before-children,
so an installed dep's version is always *derived* from an already-selected
parent. pubgrub traverses in priority order, which here is inverted.

### Minimal reproduction (both on this host, `target/release/em`)

- `em -p sys-apps/sed net-misc/rsync` → **wrong** (rsync 3.4.4; `virtual/acl`
  decided 3rd, at `0-r2`, before rsync)
- `em -p app-arch/tar net-misc/rsync` → wrong (same shape)
- `em -p sys-apps/shadow net-misc/rsync` → wrong (same shape)
- `em -p sys-apps/util-linux net-misc/rsync` → **correct** (rsync 3.5.0,
  acl+virtual/acl upgrade — rsync happens to get decided before
  `virtual/acl` here)
- `em -p net-misc/rsync` alone → correct

This `sed`+`rsync` (fails) / `util-linux`+`rsync` (passes) pair is the
smallest discriminating fixture and should anchor the regression test —
every existing `portage-atom-pubgrub` test fixture is small enough that MRV
happens to land in the right order by accident, so none of them catch this
class of bug today.

### Incidental finding (fold into the same fix)

`prioritize` is also missing pubgrub's own `version_count == 0 →
(u32::MAX, Reverse(0))` early-return guard (present in pubgrub's
`OfflineDependencyProvider::prioritize`, absent here). Without it, a
package with an empty in-range set gets prioritized *lowest* instead of
highest, so the solver burns time on unrelated decisions before hitting the
dead end, and `NoVersions` error attribution degrades. Independent bug,
cheap to fix, must sit above whatever new tier gets added.

## Phased plan (Opus review, 2026-08-17)

Sequencing per the review: **A′ now + D alongside → benchmark → A if it
generalizes → C only if a stronger objective is ever actually needed → skip
B.**

- [x] **Phase 1 (A′) — root-target priority tier.** Add a tier to
  `prioritize()` so packages named directly on the command line (`@system`
  members, `@world`, explicit atoms — i.e. `root_targets`, `mod.rs:258`)
  are always decided before their dependencies, plus the missing
  `version_count == 0` guard folded into the same change.
  `Priority = (conflict_count, is_root_target, Reverse(version_count))`
  roughly 5 lines in `provider/solve.rs:28-39`. Encodes a real emerge
  guarantee (an argument atom gets the best visible version) rather than a
  heuristic guess. Lowest risk of any option here — start here.
  **Done**: `Priority = (u32, bool, Reverse<usize>)`, `prioritize()` in
  `provider/solve.rs` — folded in the missing `version_count == 0` guard
  too. 161/161 `portage-atom-pubgrub` tests pass unchanged, including every
  OR-group/SlotChoice test the plan flagged as at risk.
- [x] **Phase 2 (D) — post-solve detection, land alongside Phase 1.**
  After solving, flag any root target that landed below its newest visible
  version and name what blocked it (walk the rejected newer version's dep
  edges against the solution). Zero solver risk; this is also the
  regression guard for Phases 1/4, so it should exist regardless of which
  direction wins.
  **Done**: `HeldBackTarget` + `check_held_back_targets` in `validate.rs`
  (re-exported from the crate root), wired into `depgraph/mod.rs` next to
  the repo-constraint advisory and printed via
  `output::report_held_back_targets` — same non-fatal-advisory pattern as
  the existing blocker/repo-constraint reports, never mutates the plan.
- [x] **Regression test.** Add the `sed`+`rsync` / `util-linux`+`rsync`
  two-target fixture to `provider/tests.rs` (near
  `installed_favored_picks_installed_version` :230,
  `installed_favored_upgrades_when_required` :479,
  `or_group_prefers_installed_alternative` :583) — two root targets, one
  with an under-constrained installed dep that has *fewer* in-range
  versions than the other target, whose newest version carries a `>=`
  bound on that dep.
  **Done**: `root_target_priority_avoids_premature_dependency_commitment`
  (generic `libacl`/`leader`/`syncer` fixture, same shape as the real
  `virtual/acl`/`rsync` bug). Verified it actually discriminates: fails
  (picks `syncer-3.4.0`) with Phase 1 reverted, passes with it in.
- [x] **Live verify.** Rebuild release, re-run `em -vp @system` against
  `emerge -vp @system` on this host, confirm `sys-apps/acl`/`virtual/acl`
  now upgrade and `net-misc/rsync` lands on `3.5.0`. Also re-check the
  three isolated repro commands above.
  **Done**: `em -vp @system` now upgrades `sys-apps/acl`/`virtual/acl` and
  picks `net-misc/rsync-3.5.0`, matching `emerge -vp @system` exactly on
  those. Total package count is 53 vs emerge's 52 — the one remaining diff
  is the pre-existing, separately-tracked `acct-user/root-0-r3` addition
  (unrelated, not touched here). No blocker errors, no held-back-target
  advisory (nothing left to flag). All three isolated repro commands
  (`sed`/`tar`/`shadow` + `rsync`) now pick `rsync-3.5.0` + acl upgrade.
- [ ] **Phase 3 — benchmark gate before going further.**
  `benchmarks/bench-em-vs-emerge.sh`, `benchmarks/benches` (`pubgrub_resolve`,
  `pubgrub_resolve_conflicts`), and `test-scripts/regression-matrix.sh`'s
  quick mode (the `-p` solver/ordering guard) must hold clean/flat before
  considering Phase 4. Watch for backtracking-count/time regressions on
  `@world`-sized graphs — MRV is demoted to a lower tier component, which
  can change search shape even when the final answer stays correct.
- [ ] **Phase 4 (A) — full follower tier, only if Phase 3 is clean.**
  Generalize the tier to *every* "follower" package (anything
  `choose_version` would resolve to the VDB version verbatim under
  `InstalledPolicy::Favor` — same predicate as `solve.rs:105-110`), not
  just root targets. Fixes the deep-dep-vs-deep-dep case Phase 1 alone
  doesn't cover. Higher risk: touches `choose_version`'s OR-group/
  `SlotChoice` heuristics (`or_group_prefers_installed_alternative`,
  `or_group_no_preference_when_both_installed`,
  `rebuild_tree_slot_star_prefers_installed_newest_slot`) indirectly by
  changing what `range` looks like when those run — should be order-stable
  since they key off `self.installed` not `range`, but verify, don't
  assume.
- [ ] **Phase 5 (C) — backstop, only if a stronger objective is later
  needed.** Extend the existing post-solve re-solve fixpoint
  (`resolve_targets`, `mod.rs:981-1026`, currently capped at
  `MAX_RESOLVE_ITERS = 4` and only pinning USE-dep-violation upgrades from
  `post_solve.rs`) to also pin any root target found below its newest
  visible version and re-solve. Closest to Portage's real iterative
  depgraph. Needs hardening first: today one failed pin discards the whole
  retry round (`Err(_) => break`), so it should pin one package per round
  (or bisect) rather than several at once, and `MAX_RESOLVE_ITERS` stops
  being just an anti-oscillation guard and becomes a correctness ceiling —
  think about what that implies before relying on it.
- **Explicitly not pursuing:** depth/topological-order prioritization (was
  candidate B) — reviewed and rejected: worst case for MRV performance on
  `@world`, depth over the *static* USE-conditional graph is fuzzy/
  approximate (Gentoo dependency cycles are common), and it only buys the
  deep-vs-deep case that Phase 4 already covers more cheaply. Also
  rejected: modeling "prefer installed" as a retractable soft constraint —
  pubgrub 0.4 has no soft clauses/assumption literals, and a synthetic
  `KeepInstalled` node has the identical premature-commitment problem
  unless given lowest priority, at which point it's just Phase 1/4 with
  extra nodes.

## Critical files

- `portage-atom-pubgrub/src/provider/solve.rs` — `prioritize` :28-39;
  `choose_version`'s `upgrade_pins` :63-67 and `InstalledPolicy::Favor`
  :89-111
- `portage-atom-pubgrub/src/provider/mod.rs` — `resolve_targets` re-solve
  fixpoint :981-1026; `root_targets` :258, `installed` :202,
  `upgrade_pins` :253
- `portage-atom-pubgrub/src/provider/post_solve.rs` — pin source for
  Phase 5, `compute_use_flag_requirements`/`upgrade_to` :183-265
- `portage-atom-pubgrub/src/provider/tests.rs` — regression test home
- `portage-cli/src/query/depgraph/mod.rs` :670 — where root-target
  classification (`selective_no_update` etc.) is set, i.e. which packages
  become "leaders" for Phase 1/4

## Related

Blocker false-positive from the same `@system` comparison (already fixed):
`fix(solver): trust VDB USE/IUSE for a candidate at its installed version`,
`2802c1b`. Root cause was unrelated (a stale tree-metadata lookup for a
revbumped installed package, not a version-choice ordering issue) — see
that commit for detail.
