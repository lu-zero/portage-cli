# `REQUIRED_USE` does not mask (PMS 7.3.4)

Status: 🔴 not started. Related: [[pms-compliance]], Level A/C in
`docs/design/architecture.md`.

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
