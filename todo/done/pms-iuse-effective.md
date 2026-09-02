# `IUSE_EFFECTIVE` (PMS 11.1.1)

Status: ✅ constructor, `in_iuse`, VDB, ForceMask filter, `use*` table 12.20.
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

`iuse_effective()` builds the set (EAPI 5+ injection). Merge sets
`IUSE_EFFECTIVE`; `in_iuse` reads it; VDB writes it. ForceMask's global
filter uses that set. `use`/`usev`/`usex`/`use_enable`/`use_with` die on
EAPI ≥ 4 when the flag is outside the set (skipped if `IUSE_EFFECTIVE` is
unset — metadata generation).

## How to attack

1. Build the set from profile env + the ebuild's `IUSE` (unit tests with a
   synthetic `IUSE_IMPLICIT` / `USE_EXPAND_VALUES_ELIBC`).
2. Set `IUSE_EFFECTIVE` on the merge shell; `in_iuse` reads it.
3. `use` errors (EAPI ≥ 4) when the flag is outside the set.
4. Write the VDB field at register time.
5. Point `ForceMask`'s IUSE filter at this set, not raw `IUSE`.
