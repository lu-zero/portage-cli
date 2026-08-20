# Empty `||` / `^^` after USE strip (PMS table 8.6)

Status: 🟡 partial. `evaluate_use_groups` + REQUIRED_USE table 8.6 are
wired; solver `convert_choice_group` still treats an empty `||` branch as
vacuous success. Related: [[pms-compliance]].

## PMS

8.2.3 / 8.2.4: a use-conditional that is an immediate child of `||` / `^^`
and whose flag is inactive is not a member. The group may then be empty.

Table 8.6: empty `||` and `^^` **match** in EAPI 0–6, and **do not match**
in EAPI 7–9.

`??` empty is always matched (at most one). `REQUIRED_USE` uses the same
group kinds (7.3.4).

## What `em` does

`DepEntry::evaluate_use` drops an empty `AnyOf` / `ExactlyOneOf`
(`dep_entry.rs`), so the constraint vanishes — EAPI 0–6 behaviour on every
EAPI. `RequiredUseExpr::is_satisfied` treats empty `||` as satisfied and
empty `^^` as unsatisfied (count == 1). Convert of `||` uses
`PortageVersionSet::any()` even with no remaining children.

`|| ( ssl? ( dev-libs/openssl ) )` with `USE=-ssl` is therefore a no-op on
EAPI 8, not a hard unsatisfiable dep.

## How to attack

1. `Eapi::empty_any_of_matches()` → `*self < Eapi::Seven`.
2. `DepEntry::evaluate_use` keeps `AnyOf([])` / `ExactlyOneOf([])` when
   that is false (portage-atom takes a bool; it cannot depend on
   portage-metadata). Callers with a cache entry pass the EAPI.
3. Convert empty `||` / `^^` to an unsatisfiable requirement.
4. `RequiredUseExpr::is_satisfied` takes EAPI (or the same bool).
5. Tests: EAPI 6 drops, EAPI 7 keeps; REQUIRED_USE empty `||` fails on 7+.
