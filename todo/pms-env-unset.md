# `ENV_UNSET` (PMS 5.3.1 / 5.3.2)

Status: 🔴 not started. Related: [[pms-compliance]], [[pms-profile-stack]].

## PMS

Table 5.7: EAPI 7–9. Incremental in `make.defaults` like `USE`. After the
profile stack is applied, the PM unsets every listed name in the ebuild
environment (5.3.2).

## What `em` does

`___eapi_has_ENV_UNSET` exists. `ENV_UNSET` is not in `INCREMENTAL_VARS`
and is never applied around `source_ebuild` / `run_phase`.

## How to attack

1. Add `ENV_UNSET` to `INCREMENTAL_VARS`.
2. After sourcing the profile + conf, `unset` each token before ebuild
   phases (and metadata regen, so cache does not leak those vars).
3. Synthetic profile test: parent sets `FOO=1` + `ENV_UNSET="FOO"`, child
   adds `BAR`; both gone in the ebuild env.
