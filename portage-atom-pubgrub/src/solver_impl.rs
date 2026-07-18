//! [`portage_solver::Solver`] implementation for the PubGrub bridge.
//!
//! The bridge already solves natively via
//! [`PortageDependencyProvider::resolve_targets`] (which internally computes the
//! upgrade fixpoint, USE-flag requirements, ceded USE decisions and dropped
//! deps). This shim drives that path and translates its solver-specific result
//! types into the solver-agnostic [`Plan`] — no behaviour change, a boundary
//! conversion only.
//!
//! `MergeRoot` and `DepClass` are now the shared `portage-solver` types
//! (re-exported by this crate), so they need no translation here; only the
//! richer node/edge/requirement types (keyed on `PortagePackage`) are mapped to
//! their plain `Cpn`/[`SelectedPackage`] counterparts.

use portage_solver::{
    CededFlag, DepEdge, DroppedDep, InstalledPackage as SolverInstalled, Plan, SelectedPackage,
    SolveError, Solver, TargetSpec, UseFlagRequirement, Violation,
};
use pubgrub::PubGrubError;

use crate::package::PortagePackage;
use crate::version_set::PortageVersionSet;
use crate::{
    DepEdge as PgDepEdge, Error as PgError, InstalledPackage, InstalledPolicy,
    PortageDependencyProvider,
};

impl Solver for PortageDependencyProvider {
    fn add_installed(&mut self, pkg: SolverInstalled) {
        let package = to_portage_package(pkg.cpn, pkg.slot);
        if !pkg.blockers.is_empty() {
            self.add_installed_blockers(&package, &pkg.blockers);
        }
        let pg = InstalledPackage {
            package,
            version: pkg.version,
            policy: map_installed_policy(pkg.policy),
            active_use: pkg.active_use,
            iuse: pkg.iuse,
        };
        PortageDependencyProvider::add_installed(self, pg);
    }

    fn set_with_bdeps(&mut self, on: bool) {
        PortageDependencyProvider::set_with_bdeps(self, on);
    }

    fn set_prefer_newest_slot(&mut self, on: bool) {
        PortageDependencyProvider::set_prefer_newest_slot(self, on);
    }

    fn set_prefer_update(&mut self, on: bool) {
        PortageDependencyProvider::set_prefer_update(self, on);
    }

    fn set_rebuild_tree(&mut self, on: bool) {
        PortageDependencyProvider::set_rebuild_tree(self, on);
    }

    fn set_cross_active(&mut self, active: bool) {
        PortageDependencyProvider::set_cross_active(self, active);
    }

    fn set_root_deps_rdeps(&mut self, active: bool) {
        PortageDependencyProvider::set_root_deps_rdeps(self, active);
    }

    fn add_host_installed(&mut self, pkg: SolverInstalled) {
        PortageDependencyProvider::add_host_installed(
            self,
            to_portage_package(pkg.cpn, pkg.slot),
            pkg.version,
            pkg.active_use,
            pkg.iuse,
        );
    }

    fn add_sysroot_installed(&mut self, pkg: SolverInstalled) {
        PortageDependencyProvider::add_sysroot_installed(
            self,
            to_portage_package(pkg.cpn, pkg.slot),
            pkg.version,
        );
    }

    fn resolve_targets(&mut self, targets: &[TargetSpec]) -> Result<Plan, SolveError> {
        let pg_targets: Vec<(PortagePackage, PortageVersionSet)> = targets
            .iter()
            .flat_map(|spec| self.to_portage_targets(spec))
            .collect();

        // UFCS to disambiguate from this trait method of the same name.
        let solution =
            PortageDependencyProvider::resolve_targets(self, pg_targets).map_err(map_error)?;

        // Advisory violations: blockers + repo constraints (USE-dep surfacing is
        // the consumer's separate autounmask path, not a violation here).
        let mut violations: Vec<Violation> = self
            .check_blockers(&solution)
            .into_iter()
            .map(map_violation)
            .collect();
        violations.extend(
            self.check_repo_constraints(&solution)
                .into_iter()
                .map(map_violation),
        );

        Ok(Plan {
            selected: solution
                .iter()
                .filter_map(|(p, v)| to_selected(p, v))
                .collect(),
            graph: self
                .dependency_graph(&solution)
                .iter()
                .filter_map(map_dep_edge)
                .collect(),
            install_order: PortageDependencyProvider::install_order(self, &solution)
                .into_iter()
                .filter_map(|(p, v)| to_selected(&p, &v))
                .collect(),
            dropped_deps: self
                .dropped_deps()
                .iter()
                .filter(|d| !d.package.is_virtual())
                .map(|d| DroppedDep {
                    cpn: *d.package.cpn(),
                })
                .collect(),
            ceded_flags: self
                .solved_use_decisions()
                .into_iter()
                .map(|c| CededFlag {
                    cpn: c.cpn,
                    flag: c.flag,
                    value: c.value,
                    flipped: c.flipped,
                })
                .collect(),
            use_flag_requirements: self
                .use_flag_requirements()
                .iter()
                .filter(|r| !r.package.is_virtual())
                .map(|r| UseFlagRequirement {
                    cpn: *r.package.cpn(),
                    version: r.version.clone(),
                    upgrade_to: r.upgrade_to.clone(),
                    required_enabled: r.required_enabled.clone(),
                    required_disabled: r.required_disabled.clone(),
                    required_by: r.required_by.clone(),
                })
                .collect(),
            violations,
        })
    }
}

/// A PubGrub package identity from a CPN + optional slot. Targets/installs fed
/// through the trait are always native (target-root); cross host/sysroot sets
/// are added via the bridge's own concrete methods, not the trait.
fn to_portage_package(
    cpn: portage_atom::Cpn,
    slot: Option<portage_atom::interner::Interned<portage_atom::interner::DefaultInterner>>,
) -> PortagePackage {
    match slot {
        Some(slot) => PortagePackage::slotted(cpn, slot),
        None => PortagePackage::unslotted(cpn),
    }
}

impl PortageDependencyProvider {
    /// Expand a solver [`TargetSpec`] into the PubGrub `(package, version-set)`
    /// targets to feed `resolve_targets`. A slot-pinned spec maps to that one
    /// slotted node; a slotless spec expands to every real slot node the
    /// provider holds for the CPN (matching how the bench feeds
    /// `packages_for_cpn`), since the solver keys nodes by `(cpn, slot)` and has
    /// no slotless "any slot" node. Consumers that need a single slot pin it in
    /// the `TargetSpec` (the CLI does, via keyword/mask-aware slot selection).
    fn to_portage_targets(&self, spec: &TargetSpec) -> Vec<(PortagePackage, PortageVersionSet)> {
        let vs = match (spec.op, &spec.version) {
            (Some(op), Some(v)) => PortageVersionSet::from_operator(op, spec.glob, v.clone()),
            _ => PortageVersionSet::any(),
        };
        match spec.slot {
            Some(slot) => vec![(PortagePackage::slotted(spec.cpn, slot), vs)],
            None => {
                let nodes = self.packages_for_cpn(&spec.cpn);
                if nodes.is_empty() {
                    // No node for the CPN: hand the solver an unslotted target so
                    // it reports the absence (NoVersions) rather than silently
                    // dropping the request.
                    vec![(PortagePackage::unslotted(spec.cpn), vs)]
                } else {
                    nodes.into_iter().map(|n| (n, vs.clone())).collect()
                }
            }
        }
    }
}

/// Map a PubGrub solution entry to a [`SelectedPackage`], dropping the
/// solver-internal virtual nodes. `merge_root` is the shared type (no mapping).
fn to_selected(pkg: &PortagePackage, ver: &portage_atom::Version) -> Option<SelectedPackage> {
    if pkg.is_virtual() {
        return None;
    }
    Some(SelectedPackage {
        cpn: *pkg.cpn(),
        version: ver.clone(),
        slot: pkg.slot(),
        merge_root: pkg.merge_root(),
    })
}

/// Translate a PubGrub `DepEdge` (keyed on `(PortagePackage, Version)`) into the
/// plain form keyed on [`SelectedPackage`]. `None` if either endpoint is a
/// virtual node (defensive: graph edges are real-only).
fn map_dep_edge(edge: &PgDepEdge) -> Option<DepEdge> {
    Some(DepEdge {
        from: to_selected(&edge.from.0, &edge.from.1)?,
        to: to_selected(&edge.to.0, &edge.to.1)?,
        class: edge.class,
        via_use_flag: edge.via_use_flag,
    })
}

fn map_installed_policy(policy: portage_solver::InstalledPolicy) -> InstalledPolicy {
    match policy {
        portage_solver::InstalledPolicy::Favor => InstalledPolicy::Favor,
        portage_solver::InstalledPolicy::Lock => InstalledPolicy::Lock,
        portage_solver::InstalledPolicy::Rebuild => InstalledPolicy::Rebuild,
    }
}

/// Map a PubGrub advisory [`PgError`] into the solver-agnostic [`Violation`].
fn map_violation(error: PgError) -> Violation {
    match error {
        PgError::BlockerConflict {
            pkg,
            blocker,
            strength,
        } => Violation::Blocker {
            pkg,
            blocker,
            strength,
        },
        PgError::UseDepConflict(a, b) => Violation::UseDep(a, b),
        PgError::RepoConstraintConflict(a, b) => Violation::Repo(a, b),
    }
}

fn map_error(error: PubGrubError<PortageDependencyProvider>) -> SolveError {
    match error {
        PubGrubError::NoSolution(tree) => SolveError::NoSolution(crate::format_no_solution(tree)),
        other => SolveError::Provider(format!("{other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::{InMemoryRepository, PackageDeps};
    use portage_atom::interner::Interned;
    use portage_atom::{Cpn, Cpv, DepEntry};

    fn deps(depend: &str, rdepend: &str) -> PackageDeps {
        PackageDeps {
            depend: (DepEntry::parse(depend).unwrap()).into(),
            rdepend: (DepEntry::parse(rdepend).unwrap()).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        }
    }

    /// Drive a resolve entirely through the `portage_solver::Solver` trait and
    /// confirm the owned `Plan` is correct: both packages selected, the dep
    /// before its dependent in install order — proving the boundary translation
    /// (TargetSpec → pubgrub, solution → SelectedPackage) round-trips.
    #[test]
    fn solver_trait_round_trips_a_simple_plan() {
        let slot0 = Some(Interned::intern("0"));
        let mut repo = InMemoryRepository::new();
        repo.add_version(
            Cpv::parse("dev-libs/openssl-3.0.0").unwrap(),
            slot0,
            None,
            deps("", ""),
        );
        repo.add_version(
            Cpv::parse("dev-lang/rust-1.75.0").unwrap(),
            slot0,
            None,
            deps(">=dev-libs/openssl-3.0.0", ">=dev-libs/openssl-3.0.0"),
        );
        let mut provider = PortageDependencyProvider::new(repo);

        let target = TargetSpec::any_in(Cpn::parse("dev-lang/rust").unwrap(), None);
        let plan = Solver::resolve_targets(&mut provider, &[target]).unwrap();

        let selected: Vec<&str> = plan
            .selected
            .iter()
            .map(|p| p.cpn.package.as_str())
            .collect();
        assert!(selected.contains(&"rust"), "rust selected: {selected:?}");
        assert!(
            selected.contains(&"openssl"),
            "openssl (dep) pulled in: {selected:?}"
        );

        let order: Vec<&str> = plan
            .install_order
            .iter()
            .map(|p| p.cpn.package.as_str())
            .collect();
        let oi = order.iter().position(|n| *n == "openssl");
        let ri = order.iter().position(|n| *n == "rust");
        assert!(
            matches!((oi, ri), (Some(o), Some(r)) if o < r),
            "openssl installs before rust: {order:?}"
        );
    }

    /// Cross config + resolve driven entirely through the `Solver` trait:
    /// `set_cross_active` + `set_root_deps_rdeps` make a target package's
    /// build-only `DEPEND` drop from the sysroot plan while its `RDEPEND` stays.
    /// Proves the cross knobs are usable via the trait, not just the concrete API.
    #[test]
    fn solver_trait_cross_rdeps_drops_target_depend() {
        let slot0 = Some(Interned::intern("0"));
        let mut repo = InMemoryRepository::new();
        repo.add_version(
            Cpv::parse("dev-build/buildtool-1.0").unwrap(),
            slot0,
            None,
            deps("", ""),
        );
        repo.add_version(
            Cpv::parse("sys-libs/runlib-1.0").unwrap(),
            slot0,
            None,
            deps("", ""),
        );
        // leaf: DEPEND on the build tool, RDEPEND on the runtime lib.
        repo.add_version(
            Cpv::parse("app-misc/leaf-1.0").unwrap(),
            slot0,
            None,
            deps("dev-build/buildtool", "sys-libs/runlib"),
        );
        let mut provider = PortageDependencyProvider::new(repo);
        Solver::set_cross_active(&mut provider, true);
        provider.set_is_cross_arch(true);
        Solver::set_root_deps_rdeps(&mut provider, true);

        let target = TargetSpec::any_in(Cpn::parse("app-misc/leaf").unwrap(), None);
        let plan = Solver::resolve_targets(&mut provider, &[target]).unwrap();

        let selected: Vec<&str> = plan
            .selected
            .iter()
            .map(|p| p.cpn.package.as_str())
            .collect();
        assert!(selected.contains(&"leaf"), "leaf resolves: {selected:?}");
        assert!(
            selected.contains(&"runlib"),
            "rdeps keeps RDEPEND: {selected:?}"
        );
        assert!(
            !selected.contains(&"buildtool"),
            "rdeps drops target DEPEND: {selected:?}"
        );
    }
}
