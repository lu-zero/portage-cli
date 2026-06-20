//! The solver abstraction both bridges implement.
//!
//! [`Solver`] covers the post-construction surface a consumer needs: feed in
//! installed packages and knobs, then run [`Solver::resolve_targets`] which
//! returns an owned [`Plan`] carrying the selected packages, labelled graph,
//! install order, and advisory output. Construction is intentionally **not**
//! part of the trait: each bridge takes its own `PackageRepository` adapter and
//! options via a concrete `new`, so the consumer picks the bridge at the call
//! site (e.g. `em --solver=resolvo`) and then talks to it through
//! `Box<dyn Solver>` (or the concrete type).
//!
//! Knobs that only some bridges support (cross-compilation host/sysroot sets)
//! stay bridge-specific extension methods on the concrete type; the trait
//! models the common native path that both bridges implement.

use crate::{InstalledPackage, Plan, SolveError, TargetSpec};

/// A Portage dependency solver.
///
/// Both `portage-atom-pubgrub` and `portage-atom-resolvo` implement this so a
/// plan can be produced — and cross-checked — by two independent algorithms
/// behind one interface.
///
/// # Lifecycle
///
/// 1. Construct the concrete bridge (each bridge's own `new`), passing the
///    [`crate::PackageRepository`] adapter and any bridge-specific options.
/// 2. Register installed packages via [`Solver::add_installed`] and set knobs.
/// 3. Call [`Solver::resolve_targets`] with the resolve targets — a single
///    joint solve over a synthetic root — and own the returned [`Plan`].
///
/// The trait is object-safe, so a consumer can hold `Box<dyn Solver>` and swap
/// implementations behind one driver (the basis for `--solver` selection and
/// A/B comparison).
pub trait Solver {
    /// Register an installed package (called before `resolve_targets`).
    fn add_installed(&mut self, pkg: InstalledPackage);

    /// Whether to pull build-host dependencies (`--with-bdeps`). Default
    /// implementation is a no-op for bridges that do not model it.
    fn set_with_bdeps(&mut self, _on: bool) {}

    /// Whether `:*` deps should bump to the newest slot (`--deep` / native
    /// `--emptytree`). Default no-op.
    fn set_prefer_newest_slot(&mut self, _on: bool) {}

    /// Whether to expand the full deep closure (native `--emptytree`).
    /// Default no-op.
    fn set_rebuild_tree(&mut self, _on: bool) {}

    /// Run the resolve over all targets in one joint solve.
    ///
    /// On success returns an owned [`Plan`] — the selected packages, labelled
    /// graph, install order, dropped deps, ceded USE decisions, derived
    /// USE-flag requirements, and post-solve violations. The solver retains no
    /// borrowed view of it, so the consumer can hold the `Plan` across a
    /// re-solve (as the autounmask fixpoint does).
    fn resolve_targets(&mut self, targets: &[TargetSpec]) -> Result<Plan, SolveError>;
}
