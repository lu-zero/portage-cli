# Changelog

## 0.2.1

### Bug fixes

- Empty or missing image directory is valid when writing a GPKG (virtuals /
  empty `ED`); create the path before `tar -C` so `--buildpkg` does not fail.

### Other

- Bump `sokgi` to 0.3: use `Dialect::parse` and `FlagSet::is_machine_dependent`
  for the board `build_env_key` path. Keep the broader `-m*` filter (Policy B)
  rather than sokgi's narrower `abi_key()` allowlist, so ISA toggles like
  `-mavx2` / `-mrvv-vector-bits=` still split the cache.
- Bump `md5` to 0.8 (`Context::finalize`); pin `blake2` to `0.11.0-rc.6`
  (0.11 still pre-release). `rand` stays on 0.8 while `pgp` 0.20 requires it.
- Ship a `LICENSE` file; exclude local `Cargo.lock` from the crates.io tarball.

## 0.2.0

### Features

- GPG signing and verification for GPKG containers via the `pgp` crate.
- `build_env_key`: canonicalize ABI/ISA compiler flags with `sokgi` so `-k` /
  binhost reuse does not false-miss on flag ordering or identical
  `CFLAGS`/`CXXFLAGS` pairs.
- Prune groups leftover multi-`BUILD_ID` containers by
  `(CPV, CHOST, build_env_key)`.
- Record `RUSTFLAGS` in VDB / GPKG / Packages producer chain.
- Maint listing surfaces CHOST, build-env key, and CFLAGS.

### Bug fixes

- Verify no longer panics on SIZE-less index rows.
- Harden Packages header/entry parsing boundary.
- Close wrong-arch reuse gaps in the build-env key gate.
- Hash only ABI/ISA flags (not the full free-form flag soup) for reuse identity.

### Other

- Shared gpkg member locate/list and local/remote index reuse-gate scaffolding.
- Inherit `rust-version` and `repository` from the workspace package metadata.

## 0.1.0

- GPKG read/write (GLEP 78), Packages index parse/write, PKGDIR scan/regen,
  and basic maint verify/list/prune.
