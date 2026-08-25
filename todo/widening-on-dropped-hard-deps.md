# Widen when hard deps are dropped by acceptance filtering

Status: 🔴 not started. Design recap agreed with Luca 2026-08-25
("sounds interesting we might have to tweak a bit what we can emit as
unmask"). Found while regenerating the pilot-i586-em llvm accepts from
scratch — full story in [[i586-full-run-findings.md]] § design gap.

## The gap

Two autounmask writer paths compose badly from an empty grant set:

1. **Widened selections** (solve fails → phase 2): slot-scoped,
   live-bounded grants — the good shape (`<llvm-core/llvm-23.1.0.9999:23
   **`).
2. **DroppedDep advisories** (solve SUCCEEDS but a hard dep had zero
   accepted candidates and was dropped at provider construction):
   portage-parity exact pins for *every* filtered version — e.g. all six
   clang/lld 23.x/24.x releases including `.9999`.

Since dropped deps don't fail the solve, phase 2 never runs and
real-run persistence writes path 2's everything-grant — exactly how
stale exact-pin sets regenerate. Real portage diverges upstream of this:
it *fails* the depgraph on a mandatory dep with no candidates and
recomputes with the collected changes applied (a complete plan); our
widened phase-2 is that recompute, just not wired to this shape.

## Chosen direction (option A): escalate to widened on eligible drops

After a strict solve succeeds, inspect `provider.dropped_deps()`: if any
entry is widening-eligible, flip the widened Cell and re-run
`attempt_solve`; adopt the widened outcome. Eligible =
same predicate as `find_autounmask_candidates` (skip virtuals, skip deps
with `||` alternatives) **plus** `data.versions` contains the cpn —
distinguishing filtered-out (widenable) from genuinely absent (not;
keep today's advisories for those).

Edges settled in the recap:

- Adopt-widened-always-once per round: widened `versions_for` ⊇ strict,
  so its drops are a subset — monotonic improvement; Cell guard prevents
  loops; each `--complete-graph` repair round may escalate once again.
- Unfixable drops (e.g. slot-mismatch narrowing) survive in the widened
  outcome's own dropped list and keep their exact-pin advisories.
- Reporting/persistence/exit-code rules unchanged (widened selections →
  bounded grants, never block; remaining drops → exact pins, exit 1).
- Visible behavior change (flagged to Luca): degraded plans now include
  the masked picks as real `[N]` rows instead of partial-plan-plus-
  warnings — the portage-parity direction.
- Luca's caveat: expect to tweak what we emit as unmask along the way
  (the widened scan currently only emits keyword/mask/license reasons for
  *selected* tagged cpvs; a dropped-then-widened dep may need the same
  treatment shaped differently).

## Implementation sketch

- Predicate home: `portage-resolve` beside `find_autounmask_candidates`
  (takes `(&[DroppedDep], &RepoData)` → bool / eligible subset);
  unit-testable against synthetic RepoData.
- Escalation site: inside `solve_round`'s Ok arm in
  `query/depgraph/mod.rs`, next to the existing failure-path retry.
- Regression probe: wipe all llvm accepts in pilot-i586-em, real merge →
  complete plan, bounded slot grants for clang/lld (not six exact pins),
  follow-up `-p` EXIT 0.

## Alternatives rejected (recap)

- Smarter exact pins only (pick best version): leaves plans incomplete,
  paths divergent.
- Make filtered hard deps fatal: closest to portage's error, but breaks
  graceful degradation for @world-style resolves.
