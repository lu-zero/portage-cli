# Blocker enforcement (Tier-2 → Tier-1)

STATUS: **Step 1 (classification) DONE, 2026-08-01.** Blockers (`!foo`/`!!foo`)
are now classified as auto-removable / genuine conflict / non-actionable and
reported with richer advisory text (`>>> would unmerge:` preview, the same
fatal "cannot be installed at the same time" message real emerge prints for a
hard conflict). **No removal happens yet** — Step 2 (actual unmerge execution)
remains SLATED LAST per the user's 2026-06-20 decision.

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
- `conflicts::classify_blockers` turns each hit into a `BlockerVerdict`
  (auto-removable / still-needed / planned-coexistence / pre-existing), and
  `output::report_blockers` prints it — a post-solve advisory only (`!!!
  Blocker conflict(s) detected`, plus a `>>> would unmerge:` preview and the
  emerge-style fatal message for a real hard conflict). **No removal
  happens.**
- `DepgraphOutcome` is still install-only (`plan: Vec<PlannedMerge>`) — no
  removal set yet; that's Step 2.
- `em depclean` already exists and owns unmerge *execution* machinery to reuse
  when (and only when) the destructive step is built.

## Reference case

`sys-apps/systemd[resolvconf]` declares `!net-dns/openresolv`; openresolv is
installed and nothing else needs it → emerge schedules openresolv for **removal**;
em keeps it. (Full 4-edge `blocks B` report parity already reached — see
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

## Step 2 — actual enforcement (SLATED LAST — destructive automation)

Only after everything else. Thread a removal set into the plan and perform the
unmerge in the real (non-pretend) merge path, reusing `em depclean`'s execution.
Blast radius is large and it removes installed packages, so the Step-1 safety
classification must be rock-solid and well-tested first, and it likely wants its
own opt-in/confirmation. Do not start this until the cheaper gaps
(properties/restrict, package.env, wrapper/shim) are done.
