//! Solver-agnostic vocabulary and [`Solver`] trait for Portage dependency
//! resolution.
//!
//! `portage-solver` is the shared layer between the two solver bridges
//! ([`portage-atom-pubgrub`], [`portage-atom-resolvo`]). It defines:
//!
//! - **Facts vocabulary** — [`PackageRepository`], [`VersionFacts`],
//!   [`PackageDeps`], [`DepClass`], [`RequiredUse`]: what a consumer feeds a
//!   solver.
//! - **USE policy vocabulary** — [`UseConfig`], [`UseFlagState`],
//!   [`IUseDefault`], [`apply_package_use`]: the per-package resolved policy
//!   the consumer computes and the solver consumes (the solver never resolves
//!   policy).
//! - **Solution/plan vocabulary** — [`SelectedPackage`], [`DepEdge`],
//!   [`InstalledPackage`], [`TargetSpec`], [`Violation`]: what a solver
//!   produces, in plain Portage terms (`Cpn`, `Version`, slot) rather than
//!   solver-internal IDs.
//! - **The [`Solver`] trait** — the single abstraction both bridges implement,
//!   so a plan can be produced — and cross-checked — by two independent
//!   algorithms behind one interface.
//!
//! This crate depends only on [`portage_atom`] (and `thiserror`); it knows
//! nothing of pubgrub or resolvo. The canonical model is the richer
//! `portage-atom-pubgrub` API, so that bridge's eventual [`Solver`] impl is a
//! thin translation; `portage-atom-resolvo` implements a best-effort subset.
//!
//! [`portage-atom-pubgrub`]: https://crates.io/crates/portage-atom-pubgrub
//! [`portage-atom-resolvo`]: https://crates.io/crates/portage-atom-resolvo
#![warn(missing_docs)]

mod facts;
mod required_use;
mod solution;
mod solver;
mod use_config;

pub use facts::{
    DepClass, IUseDefault, PackageDeps, PackageRepository, VersionFacts, rank_slots_by_version,
};
pub use required_use::RequiredUse;
pub use solution::{
    CededFlag, DepEdge, DroppedDep, InstalledPackage, InstalledPolicy, MergeRoot, SelectedPackage,
    SolveError, TargetSpec, UseFlagRequirement, Violation,
};
pub use solver::Solver;
pub use use_config::{UseConfig, UseFlagState, UseOverride, apply_package_use, atom_matches_cpv};

// Re-export the shared atom vocabulary so consumers can `use portage_solver::`
// for everything they need without a second crate import.
pub use portage_atom::interner;
pub use portage_atom::interner::{DefaultInterner, Interned};
pub use portage_atom::{Cpn, Cpv, Dep, DepEntry, Operator, Version};
