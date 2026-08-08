# Changelog

## 0.2.1

### Other

- Ship a `LICENSE` file; exclude local `Cargo.lock` from the crates.io tarball.

## 0.2.0

### Features

- Richer VDB metadata field set (closer to Portage's on-disk keys).
- ELF link metadata scan (`NEEDED` / `REQUIRES` / `PROVIDES`) and ebuild copy
  into the VDB entry.
- Record `RUSTFLAGS` through the producer path used by binpkg / Packages.
- Field cache for flat-file reads (invalidated per entry); poison-tolerant under
  concurrent workers.

### Bug fixes

- Parse `CONTENTS` paths that contain spaces.
- Loud, non-silent failures when worker env setup fails.

### Other

- Inherit `rust-version` and `repository` from the workspace package metadata.

## 0.1.0

- Initial VDB reader/writer: open `/var/db/pkg`, package iteration, ownership
  lookup, collision detection, register/unregister, `camino` paths, lazy
  iterators.
