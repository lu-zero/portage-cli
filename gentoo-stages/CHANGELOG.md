# Changelog

## 0.6.1

### Other

- Packaging hygiene: exclude workspace-only files and local cache dirs from the
  crates.io tarball.

## 0.6.0

### Other

- Inherit `rust-version` and `repository` from the workspace package metadata.
- Clippy cleanups on infallible paths.

## 0.5.1

### Documentation

- Document all public items (`Error`, `Stage3`, `Client` builder) and enable
  `#![warn(missing_docs)]`.

### Other

- Point `repository` at the workspace; migrate to workspace dependencies; raise
  MSRV to 1.92.
