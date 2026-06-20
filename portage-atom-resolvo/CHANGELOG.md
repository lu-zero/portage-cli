# Changelog

## 0.7.1

### Other

- Bump `resolvo` to 0.11. The upstream `ArenaId` trait became `DenseIndex`
  (`from_usize`/`to_usize` → `from_index`/`to_index`), and `Interner` gained the
  `NameId`/`SolvableId` associated types. `portage-atom-resolvo` now uses
  resolvo's dense solvable-ID layout (a ~12% solve speedup upstream). The
  crate's own public API is unchanged; no MSRV change (resolvo 0.11 MSRV is
  1.85.1, below the workspace floor of 1.92).

## 0.7.0

### Breaking changes

- Depend on `portage-atom` 0.10; the major bump is propagated to dependents.

### Features

- Solver-decided USE flags via `virtual/USE_<flag>` solvables and conditions.
- Blocker handling through inverted version constraints.
- Improved autounmask `package.use` writing.

### Documentation

- Document all public items and enable `#![warn(missing_docs)]`.

### Other

- Point `repository` at the workspace and add `keywords`/`categories`.
- Raise MSRV to 1.92.
