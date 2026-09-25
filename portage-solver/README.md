# portage-solver

[![LICENSE](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Crates.io](https://img.shields.io/crates/v/portage-solver.svg)](https://crates.io/crates/portage-solver)
[![docs.rs](https://docs.rs/portage-solver/badge.svg)](https://docs.rs/portage-solver)

Solver-agnostic vocabulary and [`Solver`] trait for Gentoo Portage dependency
resolution.

The vocabulary layer
[`portage-atom-pubgrub`](https://crates.io/crates/portage-atom-pubgrub)
resolves through.
[`portage-atom-resolvo`](https://crates.io/crates/portage-atom-resolvo) is a
separate comparison stack that keeps its own vocabulary and does not implement
`Solver`.

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
- **`Solver` trait** — the post-construction surface `portage-atom-pubgrub`
  implements, for cross-checking plans across backends

Depends only on [`portage-atom`](https://crates.io/crates/portage-atom); no
pubgrub or resolvo.

[`Solver`]: https://docs.rs/portage-solver/latest/portage_solver/trait.Solver.html

## License

MIT