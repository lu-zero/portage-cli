# Changelog

## 0.10.0

### Breaking changes

- `Error::Invalid{SrcUri,License,RequiredUse,Restrict}` now carry
  `ParseDiagnostic` instead of `String`. Use `Error::parse_diagnostic()` (or
  the variant payload) and render as a `miette::Diagnostic` in the application;
  the library no longer pre-renders colorized strings.

### Features

- Structured `ParseDiagnostic` (`miette::Diagnostic`) for `SRC_URI` / `LICENSE` /
  `REQUIRED_USE` / `RESTRICT` parse failures, with a cropped source window
  around the failing span.

### Bug fixes

- Accept `!` in `SRC_URI` tokens (needed by live trees such as pentoo).
- Never emit ANSI color from library diagnostic paths (CLI owns rendering).

### Other

- Depend on `miette` for structured diagnostics.
- Packaging hygiene: exclude workspace-only files from the crates.io tarball.

## 0.9.0

### Performance

- Stop round-tripping already-interned `IUSE` keys through the interner on the
  per-version hot path (`From<&IUse<_>> for Interned<_>`).

### Other

- Inherit `rust-version` and `repository` from the workspace package metadata.

## 0.8.0

### Breaking changes

- Depend on `portage-atom` 0.10 (which changed `DepEntry::evaluate_use`); the
  major bump is propagated to dependents.

### Features

- Evaluate `REQUIRED_USE` expressions.
- Collect `SRC_URI` distfile names for a given USE state.
- Apply `IUSE` defaults for merge-path parity.
- Expose interned `IUSE` keys without re-interning.

### Documentation

- Document all public items and enable `#![warn(missing_docs)]`.

### Other

- Raise MSRV to 1.92.
