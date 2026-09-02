# Blocker enforcement (Tier-2 → Tier-1)

STATUS: **DONE, 2026-08-20.** Step 1 classification (2026-08-01) plus PMS 8.3.2
enforcement: hard conflicts fail with exit 1; `WouldUnmerge` victims are
unmerged on a real merge (strong `!!` before the merge loop, weak `!` after),
reusing `execute_unmerge_batch`. `-p` still exits 0. Related: [[pms-compliance]].

PMS 8.3.2: the blocked package must not be installed. A weak block may be
ignored only if that package is uninstalled later. A **strong** block must
not be ignored.

Planned by a Fable agent (grounded directly against real portage's
`_emerge/depgraph.py::_validate_blockers` and this repo's actual code, not just
this doc's prior prose — see the plan's "ground truth" corrections below),
then implemented across 4 commits:

- `aeaa203` — `portage-atom-pubgrub`: `BlockerHit`/`BlockerVictim` +
  `check_blockers_detailed`, keeping victim identity instead of collapsing to
  an advisory string. `check_blockers` becomes a thin compat wrapper.
- `c88c81e` — `portage-resolve`: `removal_obstacles` (simulates unmerging a
  set of candidates, jointly, with a fixpoint for mutually-dependent pairs).
  Generalized `collect_violations`'s tree walk into `walk_unsatisfied_deps<T>`
  so `find_conflicts` and `removal_obstacles` share one `AllOf`/`AnyOf`
  implementation instead of two that could drift. Also fixed a real gap: the
  old inline blame lookup silently dropped a violation when the dep's cpn had
  zero remaining `present` entries — exactly the shape a removal simulation
  hits.
- `3acdc23` — `portage-resolve`: `classify_blockers` → `BlockerVerdict`
  (`WouldUnmerge`/`StillNeeded`/`PlannedCoexistence`/`PreExisting`), with
  `UnmergeOrder` encoding the real weak/strong asymmetry (both are equally
  auto-removed; strong orders its unmerge *before* the blocking merge, weak
  *after* — not "weak is safer," the folk version). A reciprocal hit (an
  installed owner's blocker against a planned victim) makes the *owner* the
  removal candidate, not the immovable victim.
- `2bb3252` — `portage-cli`: wired into the `-p` advisory site
  (`query/depgraph/mod.rs`/`output.rs`), replacing the old flat
  `check_blockers` call. `PreExisting` verdicts are silently dropped, matching
  real portage's own suppression ("the damage is already done").
- `5e3987a`/`bd2a165`/`64feadb` (2026-08-20, same day) — the display was then
  matched to real emerge's actual formatting, not just this doc's ad-hoc
  wording: `bd2a165` gives hard conflicts the real `[blocks B     ]` bracket
  row (`blocker_bracket_line`, verified against
  `_emerge/resolver/output.py::_blockers`); `64feadb` replaces the
  `>>> would unmerge:` line described below with real emerge's
  `[uninstall    ]` row (`"uninstall".ljust(13)`, `uninstall_row` in
  `output.rs`) — the "resolved: ..." explanatory line stays underneath it.
  `output.rs` still prints the uninstall row in the advisories block rather
  than interleaving it into the merge list itself as a real node in
  dependency order (noted as a known, deliberate simplification in
  `uninstall_row`'s doc comment).

Corrections the plan found that this doc's older prose got wrong or left
unstated: real portage auto-resolves *both* weak and strong blockers by the
same orphan-ness test (the asymmetry is same-slot tolerance + unmerge
ordering, not "weak safer"); a reciprocal hit's removal candidate is the
*owner*, not the victim; installed-vs-installed conflicts are suppressed by
real portage, not reported.

Live-verified: full workspace test suite green, `em -p sys-devel/gcc` and a
full `em -p @world` both run clean through the new path on this host's real
tree (no crash; no real blocker pair exists here currently to exercise the
WouldUnmerge/StillNeeded text itself, but the classifier has 7 dedicated unit
tests plus the canonical `systemd[resolvconf]`/openresolv two-edge case).

**Real-blocker-pair live verification, closing the gap above:
`test-scripts/test-blockers-iuse-effective-sandbox.sh`** (landed alongside
this feature, 2026-08-20). A synthetic `test-pms` overlay (`victim`/
`attacker-weak`/`attacker-strong`/`needs-victim`/`fails-after`) exercises
weak auto-unmerge, strong auto-unmerge, the still-needed hard-conflict path,
`-p`'s no-side-effect preview, `-B`/`-f` never unmerging, and the F4
regression (a pending `AfterBlocker` unmerge surviving an unrelated later
failure) end to end through a real merge/VDB round trip, unprivileged
(hakoniwa sandbox, no `sudo`). Rerun after any change to `conflicts.rs`/
`emerge.rs`'s unmerge machinery — it caught a real regression in the
2026-08-30 interleaving work below within one run.

**Interleaved execution position (2026-08-30) — the residual gap
`uninstall_row`'s own doc comment flagged ("full interleaving is tracked
separately, not attempted here") is closed on the *execution* side.**
Previously `emerge.rs::unmerge_blocker_victims` ran every strong (`!!`)
victim in one batch before the *entire* merge loop, and every weak (`!`)
victim in one batch after the *entire* loop — coarser than PMS 8.3.2 actually
requires (before/after the specific owner's own merge). Fixed by computing
each unmerge's real position relative to `plan`'s own topological order:
`merge::owner_plan_indices`/`merge::partition_positioned_unmerges` (an
unmerge whose owner(s) can't be located in `plan` — should not happen in
practice — still falls back to the old before/after-the-whole-plan batch,
unchanged); `merge::splice_points` for `--jobs 1` (splices the unmerge into
`merge_sequential`'s own array walk at the owner's index); `merge::
extend_blockers_with_unmerges` for `--jobs N>1` (adds the unmerge as a real
node in the `Scheduler`'s precedence graph — an owner depends on its strong
unmerge, an unmerge depends on all its weak owners — so it interleaves
correctly with concurrent, unrelated packages instead of stalling the whole
run). `-p`'s preview text placement is unchanged (display-only, still
tracked separately — see `uninstall_row`'s doc comment). Unit-tested
(`merge::unmerge_scheduling_tests`, 6 cases); live-verified both the
`--jobs 1` sandbox script above (still all green) and `--jobs 4` manually
(strong: unmerge ran concurrently with two unrelated packages while the
blocking owner correctly waited for it; weak: the unmerge correctly waited
for its owner to finish before running, even with other packages still
mid-build) — real inputs found a genuine regression in the first pass
(`-B`/`-f` had stopped being exempt from the new positioned path) that the
`-B` sandbox check caught immediately, fixed before landing.

## Current state (already in place)

- `PortageDependencyProvider::check_blockers_detailed` (portage-atom-pubgrub
  `validate.rs`) detects every blocker — forward (a planned package blocks
  something present) and reciprocal (an installed package blocks the plan, via
  `conflicts::installed_blocker_atoms`) — distinguishes weak `!` vs strong `!!`
  (`Blocker::{Weak,Strong}`), evaluates blocker USE-deps correctly, and keeps
  full victim identity (`BlockerHit`/`BlockerVictim`). The old `check_blockers`
  is now a thin compat wrapper over it.
- `conflicts::classify_blockers` turns each hit into a `BlockerVerdict`.
  `is_hard_conflict` fails `-p`/merge with exit 1. `planned_unmerges` is
  the removal set on `DepgraphOutcome`; a real merge runs it via
  `execute_unmerge_batch` (same path as `-C`/`depclean`).
- `-f`/`-B` skip unmerge (they never install). Already-gone CPVs are skipped
  (resume). Hard conflicts are not unmerged — they fail first.

## Reference case

`sys-apps/systemd[resolvconf]` declares `!net-dns/openresolv`; openresolv is
installed and nothing else needs it → emerge schedules openresolv for **removal**;
em unmerges it after the blocking merge (weak `!`). (Full 4-edge `blocks B`
report parity already reached — see
`todo/broad-basket-gaps.md`.)

## Step 1 — non-destructive classification (DONE, 2026-08-01)

Upgrade the advisory to *classify* each blocker hit, reusing the reverse-dep
machinery in `conflicts.rs` (installed deps vs final plan):

- **auto-removable** — a planned/retained package blocks an installed package
  that nothing in the final plan (or any retained installed package) still
  depends on → this is what emerge auto-removes.
- **unresolvable conflict** — the blocked package is still needed → genuine
  conflict; keep reporting, do not pretend it's fixable.

Weak/strong rule: strong `!!` must remove (else hard conflict); weak `!`
auto-removes only when safe. Render as richer advisory text and/or a
`>>> would unmerge: <cpv>` preview line for emerge `-p` parity. **No plan change,
no removal — purely analysis.** (This is "option 1 ≈ option 3" from the scoping
discussion: a removal-set display and richer advisory wording are the same work.)

## Step 2 — actual enforcement (DONE, 2026-08-20)

`planned_unmerges` on the depgraph; `unmerge_blocker_victims` before/after
the merge loop — superseded 2026-08-31 (`de53f23`), which positions each
`PlannedUnmerge` against the plan's own topological order via its owners
instead of batching them around the whole run. Confirmation is the existing `--ask` merge prompt (the
`[uninstall    ]` row, `64feadb`, already printed). No extra opt-in flag.
