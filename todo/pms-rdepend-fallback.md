# `RDEPEND` defaults to `DEPEND` (PMS 7.3.7)

Status: 🔴 not started. Related: [[pms-compliance]]. Low impact on a
modern tree (almost everything is EAPI ≥ 7).

## PMS

Table 7.4: EAPI 0–3, if `RDEPEND` is unset it becomes `DEPEND`, after
eclass accumulation (10.2).

## What `em` does

`___eapi_has_RDEPEND_DEPEND_fallback` exists for eclasses. `collect_env`
reads `RDEPEND` as-is; empty stays empty.

## How to attack

After source, if EAPI ≤ 3 and `RDEPEND` is unset, copy `DEPEND`. Distinguish
unset vs `RDEPEND=""`. Test with a synthetic EAPI 0 ebuild that only sets
`DEPEND`.
