# Changelog

## 0.4.0

### Breaking changes

- Depends on `portage-atom` 0.12.
- `MergeRoot::Base` for the toolchain sysroot; `MergeRoot::Target`'s discriminant changed.
- `resolve_effective_use` takes eight parameters instead of six.
- `Solver::add_sysroot_installed` is removed: no solver consulted it.

### Features

- `UseLayer::wildcards` lists the `<group>_*` USE_EXPAND wildcards a layer enables.

### Bug fixes

- `package.use` applies over `make.defaults` (PMS 5.2.10), and a profile's `package.use` does not override
  `make.conf` `USE`.
- Only an explicitly assigned `USE_EXPAND` variable clears its group, and group membership overrides a
  flag's own `+IUSE` default.

## 0.3.0

### Features

- `UseLayer` — parse profile / conf / env USE strings once
  (`empty` / `parse` / `is_empty` / `len` / `has_clear_all`); share a frozen
  `Arc` base map with a small overlay for package-scoped folds.

### Performance

- Frozen `Arc` pre-env base plus a small overlay avoids cloning full USE maps
  on the hot path; empty secondary probes are skipped.

### Other

- Ship a `LICENSE` file; exclude local `Cargo.lock` from the crates.io tarball.

## 0.2.0

### Breaking changes

- Rename `apply_package_use` → `resolve_effective_use`. The fold is
  layer-ordered (profile → make.conf → package.use → env) and matches Portage
  on `USE=-*` wildcard reset: conf-level reset does not wipe later
  `package.use`; env-level reset does.
- Cross-compilation knobs enter the `Solver` trait: `set_cross_active`,
  `set_root_deps_rdeps`, `add_host_installed`, `add_sysroot_installed`, plus
  `MergeRoot` on plan entries.
- `set_prefer_update` for emerge-style `-uD` in-slot upgrades.
- Installed blockers fold into `InstalledPackage`.

### Features

- First crates.io-oriented release of the solver-agnostic vocabulary and
  `Solver` trait used by `portage-atom-pubgrub` (resolvo remains a best-effort
  parallel stack).

### Performance

- Skip cloning `UseConfig` on no-match; restrict `use.mask` scans to a
  package's own IUSE.

### Other

- Inherit `rust-version` and `repository` from the workspace package metadata.

## 0.1.0

- Initial extraction of shared solver vocabulary from the PubGrub bridge.
