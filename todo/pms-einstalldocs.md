# `einstalldocs` vs algorithm 12.3

Status: ✅ landed 2026-08-29.

## PMS

Algorithm 12.3: default file set is `README*` `ChangeLog` `AUTHORS` `NEWS`
`TODO` `CHANGES` `THANKS` `BUGS` `FAQ` `CREDITS` `CHANGELOG`. `HTML_DOCS`
uses `docinto /usr/share/doc/${PF}/html` and restores dest. Algorithm 12.4:
the fallback list only runs when `DOCS` is **unset** — a declared-but-empty
`DOCS` installs nothing.

## What this todo originally claimed (stale, already wrong)

Said the default list had an extra `*` on several names and was missing
`BUGS`/`FAQ`/`CREDITS`, and that `HTML_DOCS` had no dest save/restore.
Neither was true by the time this was picked back up:
`portage-repo/src/build/commands/phase_defaults.rs`'s `DEFAULT_DOC_FILES`
already matches the PMS list exactly, and `EinstalldocsCommand` already
saves/restores `DOCDESTTREE` around the `HTML_DOCS` step. Whatever fixed
those predates this file being revisited; closing that half as already
correct rather than re-doing it.

## The real remaining bug: empty vs unset

`var_shape` (same file) collapsed "unset" and "declared-but-empty" to one
`VarShape::Empty` variant, and `EinstalldocsCommand` matched on that shape
alone to decide whether to run the fallback list — so `DOCS=""` or
`DOCS=()` ran the README*/AUTHORS/… fallback exactly like an unset `DOCS`,
contrary to Algorithm 12.4. `EapiSrcInstall4Command` (EAPI 4-5's
`src_install` default) already had this right via its own `declared =
shell.env().get("DOCS").is_some()` gate.

**Fix**: `EinstalldocsCommand` now uses the same `declared` gate before
calling `install_docs_var` (which itself no-ops on an empty declared var
via `var_shape`), instead of matching on `var_shape` directly.

## Verified

- `build::shell::tests::einstalldocs_empty_docs_installs_nothing`: `DOCS=""`
  with a real `README`/`AUTHORS` sitting in `${S}` — asserts nothing lands
  under `${D}/usr/share/doc/${PF}`.
- `build::shell::tests::einstalldocs_unset_docs_uses_fallback_list`:
  companion case, `DOCS` genuinely unset — asserts the fallback list does
  install `README`/`AUTHORS`.
- Pre-existing `einstalldocs_expands_a_glob_pattern_in_a_scalar_docs` still
  passes unchanged.
