//! The solver abstraction both bridges implement.
//!
//! [`Solver`] covers the post-construction surface a consumer needs: feed in
//! installed packages and knobs, then run [`Solver::resolve_targets`], which
//! returns the owned [`Plan`] (selected packages, labelled graph, install order)
//! together with the solver's advisory output (dropped deps, ceded USE
//! decisions, violations).
//!
//! Construction is intentionally **not** part of the trait: each bridge takes
//! its own `PackageRepository` adapter and options via a concrete `new`, so the
//! consumer picks the bridge at the call site (e.g. `em --solver=resolvo`) and
//! then talks to it through `Box<dyn Solver>` (or the concrete type). Knobs
//! that only some bridges support (cross-compilation host/sysroot sets) stay
//! bridge-specific extension methods on the concrete type; the trait models the
//! common native path that both bridges implement.

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
///    joint solve over a synthetic root — and read the returned [`Plan`].
pub trait Solver {
    /// Register an installed package (called before `resolve_targets`).
    fn add_installed(&mut self, pkg: InstalledPackage);

    /// Whether to pull build-host dependencies (`--with-bdeps`). Default
    /// implementation is a no-op for bridges that do not model it.
    fn set_with_bdeps(&mut self, _on: bool) {}

    /// Whether `:*` deps should bump to the newest slot (`--deep` / native
    /// `--emptytree`). Default no-op.
    fn set_prefer_newest_slot(&mut self, _on: bool) {}

    /// Prefer newest accepted in-slot versions for packages in the solve
    /// (emerge `-uD`: typically `update && deep`, not emptytree). When off,
    /// installed packages that satisfy a constraint stay installed (`Favor`).
    /// Default no-op.
    fn set_prefer_update(&mut self, _on: bool) {}

    /// Whether to expand the full deep closure (native `--emptytree`).
    /// Default no-op.
    fn set_rebuild_tree(&mut self, _on: bool) {}

    /// Enable dual-root cross planning (crossdev `{target}-emerge`): the solver
    /// keys nodes by `(package, merge_root)` so build-host deps (`BDEPEND`/
    /// `IDEPEND`) route to `BROOT` while runtime deps install into the target
    /// sysroot. Default no-op — a native-only backend ignores it and produces a
    /// single-root (host) plan, which is the correct native result.
    fn set_cross_active(&mut self, _active: bool) {}

    /// crossdev `--root-deps=rdeps`: discard a target package's `DEPEND` from the
    /// sysroot graph (build deps resolve against the host toolchain, not the
    /// target ROOT). Only meaningful under [`set_cross_active`](Self::set_cross_active). Default no-op.
    fn set_root_deps_rdeps(&mut self, _active: bool) {}

    /// Register a package present on the build host (`BROOT`, `/`) so a
    /// host-satisfied `BDEPEND`/`IDEPEND` edge is not rebuilt into the plan.
    /// Only meaningful under [`set_cross_active`](Self::set_cross_active). Default no-op.
    /// ([`InstalledPackage::policy`] is ignored for host/sysroot registrations.)
    fn add_host_installed(&mut self, _pkg: InstalledPackage) {}

    /// Register a package present in the cross sysroot (`ESYSROOT`) so a
    /// sysroot-satisfied `DEPEND` is not rebuilt. Only meaningful under
    /// [`set_cross_active`](Self::set_cross_active). Default no-op.
    fn add_sysroot_installed(&mut self, _pkg: InstalledPackage) {}

    /// Run the resolve. All targets are solved together in one joint solve over
    /// a synthetic root, returning the owned [`Plan`] (selected packages,
    /// labelled graph, install order) and the advisory output (dropped deps,
    /// ceded USE decisions, USE-flag requirements, violations). On error the
    /// solver state is unchanged.
    fn resolve_targets(&mut self, targets: &[TargetSpec]) -> Result<Plan, SolveError>;
}
