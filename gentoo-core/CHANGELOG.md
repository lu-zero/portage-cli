# Changelog

## 0.6.2

### Features

- `KnownArch::ALL`, every known architecture in declaration order.
- `UseToken`: one operation of a USE configuration layer, with the PMS profile grammar
  (`UseToken::profile`) and Portage's user grammar (`UseToken::user`, `UseToken::assignment`);
  `UseToken::as_flag` reads a plain flag back; `use_token::global_use` folds tokens with no
  package, as Portage's global USE.

## 0.6.1

### Other

- Packaging hygiene: exclude workspace-only files from the crates.io tarball.

## 0.6.0

### Bug fixes

- `Arch::current()` on macOS returns the Gentoo Prefix keyword arch
  (`arm64-macos`, `x64-macos`, …) so keyword acceptance matches Darwin
  profiles instead of the bare host CPU arch.

### Other

- Inherit `rust-version` and `repository` from the workspace package metadata.

## 0.5.1

### Documentation

- Document all public items (including every `KnownArch` variant) and enable
  `#![warn(missing_docs)]`.

### Other

- Point `repository` at the workspace; migrate to workspace dependencies; raise
  MSRV to 1.92.
