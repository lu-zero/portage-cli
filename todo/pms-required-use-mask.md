# `REQUIRED_USE` does not mask (PMS 7.3.4)

Status: ✅ landed 2026-08-23. Related: [[pms-compliance]], Level A/C in
`docs/design/architecture.md`.

## Resolution

Picked the second option below (refuse the merge, keep `-p` advisory).
`query/depgraph/mod.rs`: the `find_violations` block now returns a
`required_use_unsatisfied` bool (any violation left after Level-C ceding)
alongside `hard_conflict`/`unmerges`, folded into `DepgraphOutcome::exit_code`
the same way a PMS 8.3.2 hard blocker already was. `-p` still prints the
`!!! The following REQUIRED_USE flag constraints are unsatisfied:` block
(unchanged), but now exits non-zero for it, and `emerge.rs`'s existing
`exit_code != 0 → ConfigChangesNeeded` bail-out (already shared with the
hard-blocker/USE-change cases) stops a real merge before it qmerges anything.
No new error path needed — reused the existing sentinel.

Live-verified (A/B against pre-fix `em`, forcing a violation since none of
the default-USE ebuilds in the local tree trip it):
`USE="-bison -byacc" em -p app-alternatives/yacc` — advisory block unchanged,
exit code `0` (old) → `1` (new). Full workspace `cargo nextest run -p
portage-cli`: 500 passed / 5 skipped, `cargo clippy -p portage-cli -D
warnings` clean, `cargo fmt --check` clean.

No dedicated regression test added — this codebase drives depgraph-level
`exit_code` scenarios via live/`regression-matrix.sh` checks rather than a
fixture-repo unit harness (none exists for `depgraph()` today); the live A/B
above is the same style of evidence used for the file's own prior findings.

## PMS

If the assertions are not met, the package manager must treat the version
as masked. No phase functions. Flags used here must be in IUSE_EFFECTIVE.

## What `em` does

Level A (default): evaluate after the solve, print an advisory, keep the
version. That matches emerge's "fix your USE flags" prompt, not the letter
of PMS. Level C (`--autosolve-use`) can repair intra-package flags.

## How to attack

Decide, don't silently promote:

- Strict PMS: drop unsatisfied versions in `versions_for` (they become
  masked). Breaks emerge `-p` parity on every REQUIRED_USE miss.
- Keep Level A for `-p`, refuse the merge (non-zero exit, no qmerge) when
  any planned package is unsatisfied — closer to "must not install".

The second is the likely product choice. Confirm before coding; this is a
policy fork, not a missing `if`.
