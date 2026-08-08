# Changelog

## 0.8.0

### Features

- `check_blockers_detailed` keeps blocker victim identity (`BlockerHit` /
  `BlockerVictim`).
- `PortageDependencyProvider::set_selective_no_update`: when set, a satisfying
  installed version is preferred over newest (selective / `--noreplace` path;
  consumers that want `-u` leave it off).
- Re-export `UseLayer` from the USE stack rewrite in `portage-solver`.
- Humanize the synthetic root as “the requested targets” in no-solution reports
  (`format_no_solution` / `format_solve_error`).

### Bug fixes

- Soft RDEPEND order repair after soft-cycle walk; lock pass-1-forward edges so
  repair cannot invert hard dep order.

### Other

- Depend on `portage-solver` 0.3 and `portage-atom` 0.11.
- Packaging hygiene: exclude workspace-only files from the crates.io tarball.

## 0.7.0

### Breaking changes

- Re-exports `portage-solver` 0.2: `apply_package_use` →
  `resolve_effective_use` (layer-ordered USE fold, Portage-aligned `USE=-*`).
- Depends on `portage-solver` 0.2 and `portage-atom` 0.11.

### Features

- `Solver::set_prefer_update` for `-uD` in-slot upgrades (newest accepted
  in-range version instead of early `Favor` return).
- `--newuse` / `-N` and `--changed-use` / `-U` USE-drift rebuild detection.
- Treat `package.provided` as present on the build host and in preflight.
- Cross-compilation dual-root planning via the `Solver` trait
  (`set_cross_active`, host/sysroot installed sets, `root_deps_rdeps`).

### Bug fixes

- Correct inverted `!flag?` USE-dep semantics (PMS 8.2.6.4).
- Multi-slot USE-deps and branch-scoped blockers.
- Stop `install_order` SCC tie-break from sweeping non-cyclic hard deps.
- Layer-ordered USE fold (replace ad-hoc `wildcard_reset`).
- `-uD` keeps host-satisfied build deps; ROOT-aware planning parity.
- Stop `branch_installed_ver` recursing into REQUIRED_USE `UseDecision` nodes.

### Performance

- Arc-wrap dependency trees (`DepList`); intern Choice/SlotChoice less;
  hoist loop-invariant slot-map / sysroot work.

### Other

- Re-export `portage-solver`'s `RequiredUse` instead of duplicating it.

## 0.6.0

### Breaking changes

- `apply_package_use` now takes pre-parsed `&[(Dep, Vec<UseOverride>)]` instead
  of `&[(Dep, Vec<String>)]`. Flags are parsed (`+flag`/`flag` → on, `-flag` →
  off) and interned once at config-read time, so the per-version apply path does
  no string work. New public `UseOverride { flag, enable }` with `parse`.

## 0.5.0

This release covers the large body of work accumulated since 0.4.x; the public
API changed in several breaking ways (verified with `cargo semver-checks`).

### Breaking changes

- `PortageDependencyProvider::new` now takes a single `repo` argument; USE
  configuration and `package.use` are resolved by the caller (the
  `PackageRepository::desired_use` trait) rather than passed in.
- `UseConfig::solver_decide` gained a `prefer` argument, and `UseFlagState`
  gained a `SolverDecided { prefer }` variant, for Level-C `REQUIRED_USE`
  auto-satisfaction.
- `add_installed_blockers` now takes `&PortagePackage` and `&[Dep]`.
- Additional `PortagePackage` / dependency-class shapes and a new repository
  trait method; downstream impls may need updating.

### Features

- Level-C `REQUIRED_USE` auto-satisfaction (opt-in `--autosolve-use`):
  encode `a? (…)`, `||`, `^^`, `??`, and nested ceded-guard chains over
  `UseDecision` virtual nodes with preference-biased selection.
- `--deep`/emptytree: bump `:*` any-slot deps to the newest slot.
- Cross-package `[flag]` USE-dep co-solve.
- Installed-package blocker registration and reporting.
- `||` provider preference is now version-aware: when every branch of a
  provider group is installed, keep the branch reaching the newest installed
  version (matching emerge's `dep_zapdeps`), e.g. source `rust` over `rust-bin`.

### Documentation

- Document all public items and enable `#![warn(missing_docs)]`.

### Other

- Depend on `portage-atom` 0.10.
- Raise MSRV to 1.92.
