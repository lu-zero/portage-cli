# Parser too-lenient vs PMS 3.x / 8.x

Status: 🔴 not started. Related: [[pms-compliance]].

PMS 3.1: a PM should indicate or reject invalid names. We parse more than
the charset/start/end rules allow. Modern gentoo does not ship these
strings; this is QA, not a live depgraph bug.

## Concrete

- **3.1.2:** `Cpn::parse("cat/pkg-1.0")` succeeds. PN must not end in
  hyphen + version syntax. `test_package_name_cannot_end_with_hyphen_version`
  currently locks acceptance. `parse_package` comments also claim PMS
  requires an alphanumeric first character (that is 3.1.4 for USE);
  `_cron-failure` is PMS-legal.
- **3.1.5:** `::repo` uses `parse_ident_base` with no start/end check.
- **3.1.3:** `Slot::parse` (SLOT metadata) skips the charset `SlotDep` uses.
- **8.3 / tables 8.7–8.9:** no EAPI gate on USE-deps, 4-style defaults,
  `:=`, or `!!` in EAPI 0–1. PMS: warn or error.
- **8.3.3:** `:=` in PDEPEND / `||` is not rejected.
- **8.2:** empty `|| ( )` parses (`repeat(0..)`); grammar is one-or-more.

## How to attack

Tighten `parse` paths with tests that currently expect `Ok`. Do not change
`Cpn::new` / builders (documented as unvalidated). EAPI gates belong at
cache-load / ebuild-source time, where `Eapi` is known — portage-atom
stays EAPI-agnostic.
