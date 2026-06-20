//! Adapter implementing [`portage_solver::Solver`] for the resolvo bridge.
//!
//! Resolvo's [`PortageDependencyProvider`] solves through a different drive
//! model than the shared trait: the caller interns requirements, builds a
//! `Problem`, and hands it to `resolvo::Solver::solve`. It also consumes a
//! *global* `UseConfig` and pre-resolved `use_flags` per candidate, whereas
//! the solver-agnostic [`portage_solver::PackageRepository`] resolves desired
//! USE *per cpv* via [`portage_solver::PackageRepository::desired_use`].
//!
//! [`SolverAdapter`] bridges both gaps:
//!
//! - It owns a boxed solver-agnostic repository and presents it to resolvo's
//!   own `PackageRepository` trait via [`RepoAdapter`], deriving each
//!   candidate's effective `use_flags` from `desired_use(cpv)` (the enabled
//!   flags, limited to IUSE — matching pubgrub's effective-USE semantics).
//! - `impl portage_solver::Solver` converts each [`TargetSpec`] into the `Dep`
//!   `intern_requirement` expects, builds a `Problem`, solves, and maps the
//!   `Vec<SolvableId>` into a solver-agnostic [`Plan`].
//!
//! The provider is rebuilt from the retained repository on each resolve.
//! resolvo's `Solver` owns its provider and exposes it only by shared
//! reference, so the provider cannot be reused across solves; rebuilding keeps
//! the trait honest without that constraint. (The repository walk dominates
//! real `em` runtime anyway — it is the md5-cache parse, done once upstream —
//! so a per-solve re-ingest of an in-memory repo is acceptable for the
//! comparison/benchmark use cases this adapter targets.)
//!
//! This is a best-effort subset of the pubgrub bridge's coverage: version
//! resolution, `||`/`^^`/`??` groups, USE-conditional deps, blockers, slots,
//! subslots, repo constraints, and install ordering are supported. The upgrade
//! fixpoint, ceded-USE (Level-C), cross-compilation, and `--with-bdeps` are
//! not yet modelled — the corresponding `Plan` fields stay empty.

use std::collections::HashSet;

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, Dep, Slot, SlotDep};
use portage_solver::{
    DepClass, DepEdge, InstalledPackage, MergeRoot, Plan, SelectedPackage, SolveError, Solver,
    TargetSpec, UseFlagState,
};
use resolvo::{Problem, Solver as ResolvoSolver};

use crate::pool::{
    DepClass as ResDepClass, DepEdge as ResDepEdge, InstalledPolicy as ResInstalledPolicy,
    InstalledSet, PackageDeps as ResPackageDeps, PackageMetadata, UseConfig as ResUseConfig,
};
use crate::provider::PortageDependencyProvider;
use crate::repository::PackageRepository as ResolvoRepo;

/// Adapter that drives the resolvo bridge through the solver-agnostic
/// [`portage_solver::Solver`] trait.
///
/// Construct with [`SolverAdapter::new`], then call
/// [`Solver::resolve_targets`] (or hold the value as `Box<dyn Solver>` to swap
/// with the pubgrub bridge).
pub struct SolverAdapter {
    repo: Box<dyn portage_solver::PackageRepository>,
    /// Installed packages registered via [`Solver::add_installed`], folded into
    /// the provider at each resolve.
    installed: Vec<InstalledPackage>,
}

impl SolverAdapter {
    /// Build a resolvo-backed solver over a solver-agnostic repository.
    ///
    /// Desired USE is resolved per version via `repo.desired_use(cpv)`; the
    /// enabled flags (intersected with each version's IUSE) become the
    /// candidate's effective `use_flags`. USE-conditional deps are then
    /// evaluated eagerly against this set, matching pubgrub's behaviour when
    /// no flags are ceded to the solver.
    pub fn new(repo: Box<dyn portage_solver::PackageRepository>) -> Self {
        Self {
            repo,
            installed: Vec::new(),
        }
    }

    /// Build a fresh provider from the retained repo + installed set, ready to
    /// intern requirements and solve.
    fn build_provider(&self) -> PortageDependencyProvider {
        let adapted = RepoAdapter {
            repo: self.repo.as_ref(),
        };
        if self.installed.is_empty() {
            PortageDependencyProvider::new(&adapted, &ResUseConfig::default())
        } else {
            let set = self.installed_set();
            PortageDependencyProvider::with_installed(&adapted, &ResUseConfig::default(), &set)
        }
    }

    /// Materialise the registered installed packages into a resolvo
    /// [`InstalledSet`], deriving each one's effective USE from its IUSE.
    fn installed_set(&self) -> InstalledSet {
        let mut set = InstalledSet::new();
        for pkg in &self.installed {
            let use_flags: HashSet<Interned<DefaultInterner>> = pkg.iuse.iter().copied().collect();
            let meta = PackageMetadata {
                cpv: Cpv {
                    cpn: pkg.cpn,
                    version: pkg.version.clone(),
                },
                slot: pkg.slot,
                subslot: None,
                iuse: pkg.iuse.clone(),
                use_flags,
                repo: None,
                dependencies: ResPackageDeps::default(),
            };
            let policy = match pkg.policy {
                portage_solver::InstalledPolicy::Favor => ResInstalledPolicy::Favored,
                // resolvo has no Rebuild policy; Favored is the closest
                // soft-preference approximation.
                portage_solver::InstalledPolicy::Rebuild => ResInstalledPolicy::Favored,
                portage_solver::InstalledPolicy::Lock => ResInstalledPolicy::Locked,
            };
            set.add(meta, policy);
        }
        set
    }
}

impl Solver for SolverAdapter {
    fn add_installed(&mut self, pkg: InstalledPackage) {
        self.installed.push(pkg);
    }

    fn resolve_targets(&mut self, targets: &[TargetSpec]) -> Result<Plan, SolveError> {
        let mut provider = self.build_provider();
        let requirements: Vec<_> = targets
            .iter()
            .map(|spec| provider.intern_requirement(&target_to_dep(spec)))
            .collect();

        let problem = Problem::new().requirements(requirements);
        let mut solver = ResolvoSolver::new(provider);

        let solution = solver.solve(problem).map_err(map_unsolvable)?;
        Ok(build_plan(solver.provider(), &solution))
    }
}

/// Adapter presenting a solver-agnostic repository as resolvo's own
/// `PackageRepository`, deriving per-candidate effective USE from
/// `desired_use`.
struct RepoAdapter<'a> {
    repo: &'a dyn portage_solver::PackageRepository,
}

impl<'a> ResolvoRepo for RepoAdapter<'a> {
    fn all_packages(&self) -> Vec<Cpn> {
        self.repo.all_packages()
    }

    fn versions_for(&self, cpn: &Cpn) -> Vec<PackageMetadata> {
        self.repo
            .versions_for(cpn)
            .into_iter()
            .map(|(cpv, facts)| {
                let desired = self.repo.desired_use(&cpv);
                let use_flags = effective_use_flags(&facts.iuse, &desired);
                PackageMetadata {
                    cpv,
                    slot: facts.slot,
                    subslot: facts.subslot,
                    iuse: facts.iuse,
                    use_flags,
                    repo: facts.repo,
                    dependencies: ResPackageDeps {
                        depend: facts.deps.depend,
                        rdepend: facts.deps.rdepend,
                        bdepend: facts.deps.bdepend,
                        pdepend: facts.deps.pdepend,
                        idepend: facts.deps.idepend,
                    },
                }
            })
            .collect()
    }
}

/// The effective enabled USE flags for a candidate: the desired config's
/// `Enabled` flags, restricted to the version's declared IUSE. A flag enabled
/// in policy but absent from IUSE is not a real flag for this package.
fn effective_use_flags(
    iuse: &[Interned<DefaultInterner>],
    desired: &portage_solver::UseConfig,
) -> HashSet<Interned<DefaultInterner>> {
    iuse.iter()
        .filter(|flag| matches!(desired.get(**flag), UseFlagState::Enabled))
        .copied()
        .collect()
}

/// Build a `Dep` from a solver-agnostic [`TargetSpec`].
fn target_to_dep(spec: &TargetSpec) -> Dep {
    let mut dep = Dep::new(spec.cpn);
    dep.op = spec.op;
    dep.version = spec.version.clone();
    dep.glob = spec.glob;
    if let Some(slot) = spec.slot {
        dep.slot_dep = Some(SlotDep::Slot {
            slot: Some(Slot {
                slot,
                subslot: None,
            }),
            op: None,
        });
    }
    dep
}

/// Map a resolvo solve failure to a [`SolveError`].
fn map_unsolvable(err: resolvo::UnsolvableOrCancelled) -> SolveError {
    match err {
        resolvo::UnsolvableOrCancelled::Unsolvable(_) => {
            SolveError::NoSolution("resolvo reports the problem is unsolvable".into())
        }
        resolvo::UnsolvableOrCancelled::Cancelled(_) => {
            SolveError::Provider("resolvo solve was cancelled".into())
        }
    }
}

/// Build the solver-agnostic [`Plan`] from a resolvo solution.
fn build_plan(provider: &PortageDependencyProvider, solution: &[resolvo::SolvableId]) -> Plan {
    let selected: Vec<SelectedPackage> = solution
        .iter()
        .map(|&sid| solvable_to_selected(provider, sid))
        .collect();

    let graph: Vec<DepEdge> = provider
        .dependency_graph(solution)
        .into_iter()
        .map(|edge| map_dep_edge(provider, &edge))
        .collect();

    let install_order: Vec<SelectedPackage> = match provider.install_order(solution) {
        Ok(order) => order
            .iter()
            .map(|&sid| solvable_to_selected(provider, sid))
            .collect(),
        // On an unbreakable cycle, fall back to the solve order.
        Err(_) => selected.clone(),
    };

    Plan {
        selected,
        graph,
        install_order,
        // Not yet modelled by the resolvo bridge:
        dropped_deps: Vec::new(),
        ceded_flags: Vec::new(),
        use_flag_requirements: Vec::new(),
        violations: Vec::new(),
    }
}

/// Translate a resolvo `DepEdge` (SolvableId-keyed) into the solver-agnostic
/// `DepEdge` (SelectedPackage-keyed).
fn map_dep_edge(provider: &PortageDependencyProvider, edge: &ResDepEdge) -> DepEdge {
    let class = match edge.class {
        ResDepClass::Depend => DepClass::Depend,
        ResDepClass::Rdepend => DepClass::Rdepend,
        ResDepClass::Bdepend => DepClass::Bdepend,
        ResDepClass::Pdepend => DepClass::Pdepend,
        ResDepClass::Idepend => DepClass::Idepend,
    };
    DepEdge {
        from: solvable_to_selected(provider, edge.from),
        to: solvable_to_selected(provider, edge.to),
        class,
        via_use_flag: None,
    }
}

fn solvable_to_selected(
    provider: &PortageDependencyProvider,
    sid: resolvo::SolvableId,
) -> SelectedPackage {
    let meta = provider.package_metadata(sid);
    SelectedPackage {
        cpn: meta.cpv.cpn,
        version: meta.cpv.version.clone(),
        slot: meta.slot,
        merge_root: MergeRoot::Target,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use portage_atom::{Cpn, Cpv, Dep, DepEntry};
    use portage_solver::{
        PackageDeps as SolverDeps, PackageRepository as SolverRepo, UseConfig, VersionFacts,
    };

    fn slot(s: &str) -> Option<Interned<DefaultInterner>> {
        Some(Interned::intern(s))
    }

    /// Two versions of one package, no deps: the solver picks the newest.
    #[test]
    fn resolvo_solver_picks_newest_version() {
        struct Repo;
        impl SolverRepo for Repo {
            fn all_packages(&self) -> Vec<Cpn> {
                vec![Cpn::parse("dev-lang/rust").unwrap()]
            }
            fn versions_for(&self, cpn: &Cpn) -> Vec<(Cpv, VersionFacts)> {
                if cpn != &Cpn::parse("dev-lang/rust").unwrap() {
                    return Vec::new();
                }
                let mk = |v: &str| {
                    (
                        Cpv::parse(v).unwrap(),
                        VersionFacts {
                            slot: slot("0"),
                            subslot: None,
                            repo: None,
                            iuse: Vec::new(),
                            iuse_defaults: Default::default(),
                            deps: SolverDeps::default(),
                            required_use: None,
                        },
                    )
                };
                vec![mk("dev-lang/rust-1.75.0"), mk("dev-lang/rust-1.76.0")]
            }
            fn desired_use(&self, _: &Cpv) -> UseConfig {
                UseConfig::new()
            }
        }

        let mut solver = SolverAdapter::new(Box::new(Repo));
        let plan = Solver::resolve_targets(
            &mut solver,
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
        assert!(plan.graph.is_empty());
        assert_eq!(plan.install_order.len(), 1);
    }

    /// A DEPEND edge surfaces in the graph and orders the dependency first.
    #[test]
    fn resolvo_solver_dependency_edge_and_install_order() {
        struct Repo;
        impl SolverRepo for Repo {
            fn all_packages(&self) -> Vec<Cpn> {
                vec![
                    Cpn::parse("app-misc/top").unwrap(),
                    Cpn::parse("dev-libs/bottom").unwrap(),
                ]
            }
            fn versions_for(&self, cpn: &Cpn) -> Vec<(Cpv, VersionFacts)> {
                let mk = |v: &str, deps: SolverDeps| {
                    (
                        Cpv::parse(v).unwrap(),
                        VersionFacts {
                            slot: slot("0"),
                            subslot: None,
                            repo: None,
                            iuse: Vec::new(),
                            iuse_defaults: Default::default(),
                            deps,
                            required_use: None,
                        },
                    )
                };
                match format!("{}/{}", cpn.category, cpn.package).as_str() {
                    "app-misc/top" => vec![mk(
                        "app-misc/top-1.0",
                        SolverDeps {
                            depend: vec![DepEntry::Atom(Dep::parse("dev-libs/bottom").unwrap())],
                            ..SolverDeps::default()
                        },
                    )],
                    "dev-libs/bottom" => vec![mk("dev-libs/bottom-1.0", SolverDeps::default())],
                    _ => Vec::new(),
                }
            }
            fn desired_use(&self, _: &Cpv) -> UseConfig {
                UseConfig::new()
            }
        }

        let mut solver = SolverAdapter::new(Box::new(Repo));
        let plan = Solver::resolve_targets(
            &mut solver,
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
