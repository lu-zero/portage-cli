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
the merge loop. Confirmation is the existing `--ask` merge prompt (the
`>>> would unmerge:` preview already printed). No extra opt-in flag.
