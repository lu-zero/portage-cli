# `IUSE_EFFECTIVE` (PMS 11.1.1)

Status: 🔴 not started. Highest-impact gap from the 2026-08-20 PMS pass.
Related: [[pms-compliance]], [[pms-empty-dep-groups]] (flags in `flag?`
groups must be in this set).

## PMS

EAPI 5+ (table 5.6, profile-defined IUSE injection). Conceptual, not exported
to the ebuild as a user variable; `in_iuse` / `use` query it.

IUSE_EFFECTIVE = IUSE_REFERENCEABLE =

- calculated `IUSE`
- profile `IUSE_IMPLICIT`
- `USE_EXPAND_VALUES_${v}` for `v ∈ USE_EXPAND_UNPREFIXED ∩ USE_EXPAND_IMPLICIT`
  (typically `ARCH`)
- `${lower_v}_${x}` for `x ∈ USE_EXPAND_VALUES_${v}` where
  `v ∈ USE_EXPAND ∩ USE_EXPAND_IMPLICIT`

`USE` is the enabled subset of that set. `use` on a name not in the set is
an error for EAPI ≥ 4 (table 12.20). `in_iuse` (12.3.12) tests membership.
The PM must save `IUSE_EFFECTIVE` when installing.

## What `em` does

`IUSE_IMPLICIT` / `USE_EXPAND_IMPLICIT` are incrementally stacked
(`INCREMENTAL_VARS`) and then unused. `in_iuse` scans `$IUSE`
(`portage-repo/src/build/commands/has.rs`). `use` scans `$USE` with no
legality check. VDB write stores `IUSE` + `USE` only
(`todo/PENDING.md` already notes the VDB follow-up).

`ForceMask::effective` also skips global `use.mask` tokens not in ebuild
`IUSE`, so an implicit flag can slip the Algorithm 5.1 walk.

## How to attack

1. Build the set from profile env + the ebuild's `IUSE` (unit tests with a
   synthetic `IUSE_IMPLICIT` / `USE_EXPAND_VALUES_ELIBC`).
2. Set `IUSE_EFFECTIVE` on the merge shell; `in_iuse` reads it.
3. `use` errors (EAPI ≥ 4) when the flag is outside the set.
4. Write the VDB field at register time.
5. Point `ForceMask`'s IUSE filter at this set, not raw `IUSE`.
