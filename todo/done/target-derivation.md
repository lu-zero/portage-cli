# Target derivation: slot/version-qualified targets ignored (B1 + B2)

STATUS: **FIXED** 2026-06-19 (tactical fix; the full Layer A/B unification below
is still the eventual target). All six `python` forms now match `emerge -p`
(`python`/`:*`/`:3.14` → `3.14.6 [U]`; `:3.13`/`=python-3.13*` → `3.13.14 [U]`,
the latter previously crashed with no result). firefox / libreoffice parity and
the gcc+clang multi-target union are unchanged; full suite + clippy clean.

Two changes landed:
- **B2** — `target_package` (`repo.rs`) now resolves the target slot from the
  newest accepted version that `dep.matches_cpv` the atom (slot op + version op),
  instead of always the newest slot.
- **B1** — the display filter (`mod.rs`) keys the "explicit target is reinstalled
  even at best version" clause on the resolved target *slot* (`root_pkgs`, by
  cpn+slot) instead of the bare CPN (`root_cpns`), so a satisfied sibling slot is
  no longer re-listed as `[R]`.

Regression test: `target_package_honours_slot_and_version_qualifiers` (`repo.rs`).

Intended behaviour is documented in `docs/architecture.md` §"Target derivation:
argv → request". Supersedes the "B. `dev-lang/python` over-pull" section of
`todo/broad-basket-gaps.md`. The architectural decomposition below remains the
direction of travel (route CLI targets through `convert_deps` rather than the
bespoke `target_package`).

## Symptom

`em -p dev-lang/python` lists a spurious `dev-lang/python-3.13.12 [R]` next to the
real target `python-3.14.6 [U]`; emerge lists only 3.14.6. Worse, the slot
operator and version glob have **no effect at all** — em produces byte-identical
output for every form:

| target           | emerge          | em                            |
|------------------|-----------------|-------------------------------|
| `python`         | `3.14.6 [U]`    | `3.13.12 [R]` + `3.14.6 [U]`  |
| `python:*`       | `3.14.6 [U]`    | `3.13.12 [R]` + `3.14.6 [U]`  |
| `python:3.14`    | `3.14.6 [U]`    | `3.13.12 [R]` + `3.14.6 [U]`  |
| `python:3.13`    | `3.13.14 [U]`   | `3.13.12 [R]` + `3.14.6 [U]`  |
| `=python-3.14*`  | `3.14.6 [U]`    | `3.13.12 [R]` + `3.14.6 [U]`  |
| `=python-3.13*`  | `3.13.14 [U]`   | **(no result / error)**       |

Installed: python 3.12.13_p1 / 3.13.12 / 3.14.5. Repo has newer 3.13.14 and 3.14.6.

## Root causes — two, both in Layer A (input → request)

The CLI target path bypasses the canonical atom→constraint converter
(`convert_deps` / `convert_atom`, portage-atom-pubgrub) that ebuild dependencies
use, and hand-rolls a lossy conversion instead.

**B2 — slot/version identity dropped.** `target_package` (`repo.rs:655`) collects
all of a CPN's *accepted* slots and the multi-slot arm picks the **newest**,
*never reading `dep.slot_dep`*. The version-set is then built separately
(`mod.rs:224`) and bolted onto that newest-slot package. So:

- `python:3.13` / `=python-3.13*` resolve to the `python:3.14` package; the
  version-set `=3.13*` then can't be satisfied on any 3.14.x → unsatisfiable → no
  result (the `=python-3.13*` crash).
- `Dep::matches_cpv(cpv, slot)` (`dep.rs:117`, checks slot at `:121`) already
  exists and would do the slot+version filtering correctly.

**B1 — sibling slot listed as `[R]`.** python:3.13 is pulled as a transitive dep,
satisfied by installed 3.13.12 (Favor keeps it — correct), but the display filter
keeps it in the merge list via `root_cpns.contains(pkg.cpn())` (`mod.rs:481`).
`root_cpns` holds the bare CPN `dev-lang/python` (`mod.rs:211`), so it matches the
*sibling* 3.13 slot too. `action_tag` (`installed.rs:177`) then stamps `[R]` on
version-equality. The entry carries **no USE change** (`em -pv` shows `0 KiB`, no
flags) — it is not in `reinstall_cpns`; it is purely the CPN-vs-slot conflation of
the "explicit target is reinstalled even at best version" clause.

## Fix plan — route CLI targets through the canonical path

1. **Layer A (portage-cli):** resolve each target `Dep` to a precise package
   identity — honour `dep.slot_dep`; for a bare / `:*` target pick the newest
   *accepted* slot (= emerge's "best version of the matched set"); preserve the
   version-set. Use `Dep::matches_cpv` for the slot+version filter so eligibility
   filtering (keyword/mask/license) and identity selection stop being conflated
   inside `target_package`.
2. **Layer B (portage-atom-pubgrub):** model the request as `Root`'s dependencies
   converted via `convert_deps`, and carry a per-target **disposition**
   (root target → pull-best / reinstall) so the display filter keys on the
   *resolved target package* rather than the bare CPN. This removes `root_cpns`
   and dissolves B1.

After the fix B2 (qualifier honoured) and B1 (no sibling `[R]`) both resolve; all
six rows above should match emerge.

## Regression coverage to add

- `em -p python:3.13` → `3.13.14 [U]` only.
- `em -p =dev-lang/python-3.13*` → `3.13.14 [U]` only (no crash).
- `em -p python` / `python:*` / `python:3.14` → `3.14.6 [U]` only (no `3.13 [R]`).
- Multi-target union (`sys-devel/gcc llvm-core/clang`) unchanged (default + `-u`).

## Reference behaviour confirmed against emerge (2026-06-19)

Documented as intended in `docs/architecture.md`; recorded here for traceability:

- Multi-target = joint solve = union for independent targets (`-p` and `-up`). ✓ em matches.
- Explicit target pulls best in-slot version even without `-u` (`gcc` → 16.1.1 `[U]`). ✓ em matches.
- Ambiguous bare name: emerge **always errors** (ignores installed); em prefers
  installed under `-u`. Intentional divergence.
- Multi-target, one bad atom: emerge **aborts**; em drops-with-warning. Divergence
  (decide whether to keep).
