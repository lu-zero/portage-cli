# portage-solver

[![LICENSE](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Crates.io](https://img.shields.io/crates/v/portage-solver.svg)](https://crates.io/crates/portage-solver)
[![docs.rs](https://docs.rs/portage-solver/badge.svg)](https://docs.rs/portage-solver)

Solver-agnostic vocabulary and [`Solver`] trait for Gentoo Portage dependency
resolution.

Shared layer between the two solver bridges
[`portage-atom-pubgrub`](https://crates.io/crates/portage-atom-pubgrub) and
[`portage-atom-resolvo`](https://crates.io/crates/portage-atom-resolvo).

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
portage-solver = "0.3"
```

## Overview

- **Facts vocabulary** — `PackageRepository`, `VersionFacts`, `PackageDeps`
- **USE policy vocabulary** — `UseConfig`, `UseFlagState`, `UseLayer`,
  `resolve_effective_use`
- **Solution vocabulary** — `SelectedPackage`, `DepEdge`, `TargetSpec`
- **`Solver` trait** — single interface both bridges implement for cross-checking

Depends only on [`portage-atom`](https://crates.io/crates/portage-atom); no
pubgrub or resolvo.

[`Solver`]: https://docs.rs/portage-solver/latest/portage_solver/trait.Solver.html

## License

MIT