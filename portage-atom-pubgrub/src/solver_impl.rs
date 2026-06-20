//! Adapter implementing [`portage_solver::Solver`] for the PubGrub bridge.
//!
//! The PubGrub bridge already solves natively via
//! [`PortageDependencyProvider::resolve_targets`], which internally computes
//! the upgrade fixpoint, USE-flag requirements, ceded USE decisions, and
//! dropped deps. This module is a thin shim that drives that existing path and
//! translates its solver-specific result types into the solver-agnostic
//! [`portage_solver::Plan`] — no behaviour change, just a boundary conversion.
//!
//! Translation (not type-identity) is deliberate for now: the shared
//! vocabulary in `portage-solver` and PubGrub's own types are structurally
//! identical but distinct. Folding them into one definition (re-export) is a
//! follow-up that both bridges adopt together when the resolvo impl lands.

use portage_solver::{
    CededFlag, DepClass, DepEdge, DroppedDep, InstalledPackage as SolverInstalled, MergeRoot, Plan,
    SelectedPackage, SolveError, Solver, TargetSpec, UseFlagRequirement, Violation,
};
use pubgrub::PubGrubError;

use crate::package::{MergeRoot as PgMergeRoot, PortagePackage};
use crate::version_set::PortageVersionSet;
use crate::{
    DepClass as PgDepClass, DepEdge as PgDepEdge, Error as PgError, InstalledPackage,
    InstalledPolicy as PgInstalledPolicy, PortageDependencyProvider,
};

impl Solver for PortageDependencyProvider {
    fn add_installed(&mut self, pkg: SolverInstalled) {
        let pg = InstalledPackage {
            package: to_portage_package(&pkg),
            version: pkg.version,
            policy: map_installed_policy(pkg.policy),
            active_use: pkg.active_use,
            iuse: pkg.iuse,
        };
        PortageDependencyProvider::add_installed(self, pg);
    }

    fn resolve_targets(&mut self, targets: &[TargetSpec]) -> Result<Plan, SolveError> {
        let pg_targets: Vec<(PortagePackage, PortageVersionSet)> =
            targets.iter().map(to_portage_target).collect();

        // UFCS to disambiguate from this trait method of the same name.
        let solution =
            PortageDependencyProvider::resolve_targets(self, pg_targets).map_err(map_error)?;

        // Violations: blockers + repo constraints (USE-dep surfacing is the
        // CLI's separate autounmask path, not a violation here).
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
                .filter_map(|d| {
                    if d.package.is_virtual() {
                        None
                    } else {
                        Some(DroppedDep {
                            cpn: *d.package.cpn(),
                        })
                    }
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
                .filter_map(|r| {
                    if r.package.is_virtual() {
                        return None;
                    }
                    Some(UseFlagRequirement {
                        cpn: *r.package.cpn(),
                        version: r.version.clone(),
                        upgrade_to: r.upgrade_to.clone(),
                        required_enabled: r.required_enabled.clone(),
                        required_disabled: r.required_disabled.clone(),
                        required_by: r.required_by.clone(),
                    })
                })
                .collect(),
            violations,
        })
    }
}

/// Build a PubGrub package identity from a solver-agnostic installed/target
/// package (CPN + optional slot). Targets/installs are always native
/// (target-root); cross-compilation host/sysroot packages are added via the
/// bridge's own concrete methods, not through the trait.
fn to_portage_package(pkg: &SolverInstalled) -> PortagePackage {
    match pkg.slot {
        Some(slot) => PortagePackage::slotted(pkg.cpn, slot),
        None => PortagePackage::unslotted(pkg.cpn),
    }
}

/// Convert a solver [`TargetSpec`] to a PubGrub `(package, version-set)` pair.
fn to_portage_target(spec: &TargetSpec) -> (PortagePackage, PortageVersionSet) {
    let package = match spec.slot {
        Some(slot) => PortagePackage::slotted(spec.cpn, slot),
        None => PortagePackage::unslotted(spec.cpn),
    };
    let vs = match (spec.op, &spec.version) {
        (Some(op), Some(v)) => PortageVersionSet::from_operator(op, spec.glob, v.clone()),
        _ => PortageVersionSet::any(),
    };
    (package, vs)
}

/// Map a PubGrub solution entry to a solver-agnostic [`SelectedPackage`],
/// dropping solver-internal virtual nodes.
fn to_selected(pkg: &PortagePackage, ver: &portage_atom::Version) -> Option<SelectedPackage> {
    if pkg.is_virtual() {
        return None;
    }
    Some(SelectedPackage {
        cpn: *pkg.cpn(),
        version: ver.clone(),
        slot: pkg.slot(),
        merge_root: map_merge_root(pkg.merge_root()),
    })
}

/// Translate a PubGrub `DepEdge` (keyed on `(PortagePackage, Version)`) into the
/// solver-agnostic form keyed on [`SelectedPackage`]. Returns `None` if either
/// endpoint is a solver-internal virtual node (graph edges are real-only, so
/// this is defensive only).
fn map_dep_edge(edge: &PgDepEdge) -> Option<DepEdge> {
    Some(DepEdge {
        from: to_selected(&edge.from.0, &edge.from.1)?,
        to: to_selected(&edge.to.0, &edge.to.1)?,
        class: map_dep_class(edge.class),
        via_use_flag: edge.via_use_flag,
    })
}

fn map_dep_class(class: PgDepClass) -> DepClass {
    match class {
        PgDepClass::Depend => DepClass::Depend,
        PgDepClass::Rdepend => DepClass::Rdepend,
        PgDepClass::Bdepend => DepClass::Bdepend,
        PgDepClass::Pdepend => DepClass::Pdepend,
        PgDepClass::Idepend => DepClass::Idepend,
    }
}

fn map_merge_root(root: PgMergeRoot) -> MergeRoot {
    match root {
        PgMergeRoot::Host => MergeRoot::Host,
        PgMergeRoot::Target => MergeRoot::Target,
    }
}

fn map_installed_policy(policy: portage_solver::InstalledPolicy) -> PgInstalledPolicy {
    match policy {
        portage_solver::InstalledPolicy::Favor => PgInstalledPolicy::Favor,
        portage_solver::InstalledPolicy::Lock => PgInstalledPolicy::Lock,
        portage_solver::InstalledPolicy::Rebuild => PgInstalledPolicy::Rebuild,
    }
}

/// Map a PubGrub `Error` advisory into the solver-agnostic [`Violation`].
fn map_violation(error: PgError) -> Violation {
    Violation::from(error)
}

impl From<PgError> for Violation {
    fn from(error: PgError) -> Self {
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
}

/// Map a PubGrub resolution failure into a [`SolveError`].
fn map_error(error: PubGrubError<PortageDependencyProvider>) -> SolveError {
    match error {
        PubGrubError::NoSolution(tree) => SolveError::NoSolution(format!("{tree:?}")),
        other => SolveError::Provider(format!("{other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InMemoryRepository, PackageDeps as PgDeps, PortageDependencyProvider, Solver};
    use portage_atom::interner::{DefaultInterner, Interned};
    use portage_atom::{Cpn, Cpv, Dep, DepEntry};
    use portage_solver::{DepClass, TargetSpec};

    fn empty_deps() -> PgDeps {
        PgDeps {
            depend: vec![],
            rdepend: vec![],
            bdepend: vec![],
            pdepend: vec![],
            idepend: vec![],
        }
    }

    /// Two versions of one package, no deps: the solver picks the newest.
    #[test]
    fn solver_trait_returns_plan_with_newest_version() {
        let mut repo = InMemoryRepository::new();
        repo.add_version(
            Cpv::parse("dev-lang/rust-1.75.0").unwrap(),
            None,
            None,
            empty_deps(),
        );
        repo.add_version(
            Cpv::parse("dev-lang/rust-1.76.0").unwrap(),
            None,
            None,
            empty_deps(),
        );

        let mut provider = PortageDependencyProvider::new(repo);
        // UFCS pins the call to the `Solver` trait method (returns `Plan`),
        // disambiguating from the inherent `resolve_targets` of the same name.
        let plan = Solver::resolve_targets(
            &mut provider,
            &[TargetSpec::any_in(
                Cpn::parse("dev-lang/rust").unwrap(),
                None,
            )],
        )
        .expect("resolve");

        assert_eq!(plan.selected.len(), 1);
        assert_eq!(
            plan.selected[0].version,
            Cpv::parse("dev-lang/rust-1.76.0").unwrap().version
        );
        assert_eq!(plan.install_order.len(), 1);
        assert!(plan.graph.is_empty());
        assert!(plan.dropped_deps.is_empty());
        assert!(plan.violations.is_empty());
        assert!(plan.ceded_flags.is_empty());
        assert!(plan.use_flag_requirements.is_empty());
    }

    /// A DEPEND edge surfaces in the graph and orders the dependency first.
    #[test]
    fn solver_trait_reports_dependency_edge_and_install_order() {
        let mut repo = InMemoryRepository::new();
        repo.add_version(
            Cpv::parse("app-misc/top-1.0").unwrap(),
            None,
            None,
            PgDeps {
                depend: vec![DepEntry::Atom(Dep::parse("dev-libs/bottom-1.0").unwrap())],
                ..empty_deps()
            },
        );
        repo.add_version(
            Cpv::parse("dev-libs/bottom-1.0").unwrap(),
            None,
            None,
            empty_deps(),
        );

        let mut provider = PortageDependencyProvider::new(repo);
        let plan = Solver::resolve_targets(
            &mut provider,
            &[TargetSpec::any_in(
                Cpn::parse("app-misc/top").unwrap(),
                None,
            )],
        )
        .expect("resolve");

        assert_eq!(plan.selected.len(), 2);
        assert!(plan.graph.iter().any(|e| {
            e.class == DepClass::Depend
                && e.from.cpn.package.as_str() == "top"
                && e.to.cpn.package.as_str() == "bottom"
        }));
        let pos = |name: &str| {
            plan.install_order
                .iter()
                .position(|p| p.cpn.package.as_str() == name)
                .unwrap()
        };
        assert!(pos("bottom") < pos("top"));
    }
}
