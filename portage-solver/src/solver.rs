//! The solver abstraction both bridges implement.
//!
//! [`Solver`] covers the post-construction surface a consumer needs: feed in
//! installed packages and knobs, run [`Solver::resolve_targets`], then read the
//! plan (selected packages, labelled graph, install order) and the solver's
//! advisory output (dropped deps, ceded USE decisions, violations).
//!
//! Construction is intentionally **not** part of the trait: each bridge takes
//! its own `PackageRepository` adapter and options via a concrete `new`, so the
//! consumer picks the bridge at the call site (e.g. `em --solver=resolvo`) and
//! then talks to it through `Box<dyn Solver>` (or the concrete type). Knobs
//! that only some bridges support (cross-compilation host/sysroot sets) stay
//! bridge-specific extension methods on the concrete type; the trait models the
//! common native path that both bridges implement.

use crate::{
    CededFlag, DepEdge, DroppedDep, InstalledPackage, SelectedPackage, SolveError, TargetSpec,
    UseFlagRequirement, Violation,
};

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
///    joint solve over a synthetic root.
/// 4. Read the plan via [`Solver::selected`], [`Solver::dependency_graph`],
///    [`Solver::install_order`], and the advisory accessors.
///
/// All accessors return data materialised by the last successful
/// `resolve_targets` call and are valid until the next call mutates the solver.
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

    /// Run the resolve. All targets are solved together in one joint solve.
    /// On success the plan accessors are populated; on error they retain their
    /// previous state.
    fn resolve_targets(&mut self, targets: &[TargetSpec]) -> Result<(), SolveError>;

    /// The selected packages (real packages only; virtual/decision nodes are
    /// stripped), in no guaranteed order.
    fn selected(&self) -> &[SelectedPackage];

    /// The labelled dependency graph of the last solution (edges where both
    /// endpoints are selected).
    fn dependency_graph(&self) -> &[DepEdge];

    /// The selected packages in topological install order: a dependency is
    /// merged before the package that needs it. Cycles are broken on soft
    /// (RDEPEND) edges, falling back to a deterministic tie-break on genuine
    /// build-time cycles.
    fn install_order(&self) -> Vec<SelectedPackage>;

    /// Dependencies the solver had to drop (no satisfying candidate in the
    /// reachable closure). Reported for diagnostics.
    fn dropped_deps(&self) -> &[DroppedDep];

    /// USE flags the solver was ceded (Level-C `REQUIRED_USE`) and the values
    /// it picked. Empty when nothing was ceded.
    fn solved_use_decisions(&self) -> &[CededFlag];

    /// Per-target USE-flag requirements the solve derived (the "needed" set),
    /// surfaced as autounmask `package.use` suggestions.
    fn use_flag_requirements(&self) -> &[UseFlagRequirement];

    /// Post-solve advisory violations (blockers, USE-deps, `::repo`), reported
    /// after the plan as portage does. Empty when the solution is clean.
    fn violations(&self) -> &[Violation];
}
