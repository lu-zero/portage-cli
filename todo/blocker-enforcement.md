# Blocker enforcement (Tier-2 → Tier-1)

STATUS: **detection done (report-only); the destructive auto-unmerge is SLATED
LAST.** Blockers (`!foo`/`!!foo`) are detected and reported but never acted on.
Promoting them to actual exclusion/replacement is the open Tier-2 → Tier-1 step.
Per user (2026-06-20): the automation (actually removing packages) is the *very
last* thing to build; the non-destructive analysis below is the only near-term
piece, and it is small.

## Current state (already in place)

- `PortageDependencyProvider::check_blockers` (portage-atom-pubgrub `validate.rs`)
  detects every blocker — forward (a planned package blocks something present)
  and reciprocal (an installed package blocks the plan, via
  `conflicts::installed_blocker_atoms`) — distinguishes weak `!` vs strong `!!`
  (`Blocker::{Weak,Strong}`), and evaluates blocker USE-deps correctly.
- Output is a post-solve advisory only (`!!! Blocker conflict(s) detected`),
  printed after the merge list. **No removal happens.**
- `DepgraphOutcome` is install-only (`plan: Vec<PlannedMerge>`) — no removal set.
- `em depclean` already exists and owns unmerge *execution* machinery to reuse
  when (and only when) the destructive step is built.

## Reference case

`sys-apps/systemd[resolvconf]` declares `!net-dns/openresolv`; openresolv is
installed and nothing else needs it → emerge schedules openresolv for **removal**;
em keeps it. (Full 4-edge `blocks B` report parity already reached — see
`todo/broad-basket-gaps.md`.)

## Step 1 — non-destructive classification (the only near-term piece)

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
