# Changelog

## 0.4.1

### Other

- Packaging hygiene: exclude workspace-only files from the crates.io tarball.

## 0.4.0

### Other

- Inherit `rust-version` and `repository` from the workspace package metadata.

## 0.3.1

### Features

- Expose interned keys without re-interning.

### Documentation

- Document all public items and enable `#![warn(missing_docs)]`.

### Other

- Point `repository` at the workspace; raise MSRV to 1.92.
