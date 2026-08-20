# Profile stack leftovers (PMS 5.2.1 / table 5.1 / 5.3)

Status: 🔴 not started. No live canary on stock gentoo.
Related: [[pms-compliance]], [[pms-env-unset]], [[use-stable-in-defaults]].

## Duplicate parents (5.2.1)

PMS: depth-first, left to right, **source a parent every time it appears**.
`collect_stack` uses a visited set; `stack_diamond_inheritance` locks
"once". A second visit would re-apply that node's incremental files
(including `-*`). Cycles are undefined; skipping those is allowed.

Attack: drop the visited skip for non-cycle repeats, keep a recursion
stack for cycles. Synthetic diamond test first — current test encodes the
diverge.

## EAPI 9 missing per-dir `eapi` (table 5.1)

PMS: default is EAPI 0 for profile EAPIs 0–8; for EAPI 9 it is the
top-level `profiles/eapi`. `Profile::open` always uses `Eapi::Zero`.
`Repository::profiles_eapi` exists and is unused as the fallback.

Attack: pass the repo's `profiles/eapi` into `Profile::open` when the
per-dir file is absent.

## `package.provided` (table 5.3)

Optional EAPI 0–6, **No** for 7–9. `em` still loads it. Ignore the file
when the providing profile directory's EAPI is ≥ 7.
