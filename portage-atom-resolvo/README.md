# portage-atom-resolvo

[![LICENSE](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE-MIT)
[![Build Status](https://github.com/lu-zero/portage-atom-resolvo/workflows/CI/badge.svg)](https://github.com/lu-zero/portage-atom-resolvo/actions?query=workflow:CI)
[![codecov](https://codecov.io/gh/lu-zero/portage-atom-resolvo/graph/badge.svg?token=T4QDNX4HYK)](https://codecov.io/gh/lu-zero/portage-atom-resolvo)
[![Crates.io](https://img.shields.io/crates/v/portage-atom-resolvo.svg)](https://crates.io/crates/portage-atom-resolvo)
[![dependency status](https://deps.rs/repo/github/lu-zero/portage-atom-resolvo/status.svg)](https://deps.rs/repo/github/lu-zero/portage-atom-resolvo)
[![docs.rs](https://docs.rs/portage-atom-resolvo/badge.svg)](https://docs.rs/portage-atom-resolvo)

Bridge between [portage-atom](https://crates.io/crates/portage-atom) and the
[resolvo](https://crates.io/crates/resolvo) SAT-based dependency solver.

> **Warning**: This codebase was largely AI-generated (slop-coded) and has not
> yet been thoroughly audited. It may contain bugs, incomplete PMS coverage, or
> surprising edge-case behaviour. Use at your own risk and please report issues.

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
portage-atom-resolvo = "0.8"
```

## Quick start

```bash
cargo run --example resolve
cargo run --example resolve_conflict
```

See [`examples/resolve.rs`](examples/resolve.rs) for a complete walkthrough
that builds an in-memory repository, declares transitive / any-of / slotted /
USE-conditional dependencies, and prints the solved package set.

See [`examples/resolve_conflicts.rs`](examples/resolve_conflicts.rs) for the
initial error reporting layout.

## Feature checklist

### Working

- [x] Version matching - all 7 PMS 8.3.1 operators (`<` `<=` `=` `>=` `>` `~` `=*`)
- [x] Transitive dependency resolution via resolvo's CDCL SAT solver
- [x] Newest-first version preference (`sort_candidates` descending)
- [x] `|| ( a b )` any-of groups -> `Requirement::Union`
- [x] USE-conditional deps (`use? ( ... )`, `!use? ( ... )`) - eagerly evaluated or solver-decided via `UseConfig`
- [x] Blockers (`!atom`, `!!atom`) -> resolvo `constrains`, with weak/strong distinction tracked via `blocker_type()`
- [x] Multi-slot coexistence (`python:3.11` + `python:3.12` in same solution)
- [x] Unslotted deps resolve across all known slots (union)
- [x] Slot-specific deps (`:3.12`) target only that slot's candidates
- [x] Slot operator `:*` (accept any slot)
- [x] Slot operator `:=` tracking - `is_rebuild_trigger()` flags deps that need rebuilds on slot/subslot changes
- [x] Sub-slot matching - `:SLOT/SUBSLOT` constraints checked in `filter_candidates`
- [x] Strong vs weak blocker distinction - `blocker_type()` returns `Blocker::Weak` or `Blocker::Strong`
- [x] Repository constraint (`::gentoo`) - `PackageMetadata::repo` + `VersionConstraint::repo` filtering in `filter_candidates`
- [x] USE dep constraints on atoms (`[ssl,-debug]`) - all 6 PMS 8.3.4 variants, conditional forms resolved eagerly against `UseConfig`
- [x] `DEPEND` / `RDEPEND` / `BDEPEND` / `PDEPEND` / `IDEPEND` separation - `PackageDeps` struct with per-class fields, all treated as requirements
- [x] Arena-based interning with dedup for names and version sets
- [x] `InMemoryRepository` for testing
- [x] Public API: `intern_requirement()` -> `Problem` -> `Solver::solve()`
- [x] Circular dependency handling via `PDEPEND` - `dependency_graph()` returns dep-class–labeled edges, `install_order()` uses Kahn's toposort with PDEPEND relaxation
- [x] Installed-package database - `InstalledSet` + `with_installed()` constructor; `Candidates::favored` (soft preference) and `Candidates::locked` (hard constraint) per name

### Not yet implemented
- [ ] Better human-readable conflict/error reporting

## Architecture

```
lib.rs               re-exports
version_match.rs     version_matches(candidate, op, constraint) -> bool
pool.rs              PortagePool arena (resolvo IDs <-> portage-atom types)
repository.rs        PackageRepository trait + InMemoryRepository
provider.rs          Interner + DependencyProvider impl
```

## Running checks

```bash
cargo test                        # 75 tests
cargo clippy -- -D warnings       # clean
cargo fmt --check                 # formatted
cargo doc --no-deps               # no warnings
```

## Related Projects

- [PMS](https://projects.gentoo.org/pms/) - Package Manager Specification
- [Portage](https://wiki.gentoo.org/wiki/Portage) - Reference Gentoo package manager
- [pkgcraft](https://crates.io/crates/pkgcraft) - Full-featured Gentoo package manager library

## License

[MIT](LICENSE-MIT)

## Author

Luca Barbato <lu_zero@gentoo.org>
