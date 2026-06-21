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

use std::collections::{HashMap, HashSet};

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
pub struct SolverAdapter<'repo> {
    repo: Box<dyn portage_solver::PackageRepository + 'repo>,
    /// Installed packages registered via [`Solver::add_installed`], folded into
    /// the provider at each resolve.
    installed: Vec<InstalledPackage>,
    /// Solver-decided (ceded) flags harvested from `desired_use` across all
    /// repo versions: the flag and the caller's preferred value. Drives the
    /// per-(cpn, flag) decision virtuals in the resolvo provider.
    ceded: HashMap<Interned<DefaultInterner>, bool>,
}

impl<'repo> SolverAdapter<'repo> {
    /// Build a resolvo-backed solver over a solver-agnostic repository.
    ///
    /// Desired USE is resolved per version via `repo.desired_use(cpv)`; the
    /// enabled flags (intersected with each version's IUSE) become the
    /// candidate's effective `use_flags`. USE-conditional deps are then
    /// evaluated eagerly against this set, matching pubgrub's behaviour when
    /// no flags are ceded to the solver.
    ///
    /// Flags a version's `desired_use` marks `SolverDecided` are ceded to the
    /// solver: a per-(cpn, flag) decision virtual is created so the solver
    /// chooses the value, biased toward the caller's `prefer`.
    pub fn new(repo: Box<dyn portage_solver::PackageRepository + 'repo>) -> Self {
        let ceded = harvest_ceded_flags(repo.as_ref());
        Self {
            repo,
            installed: Vec::new(),
            ceded,
        }
    }

    /// Build a fresh provider from the retained repo + installed set, ready to
    /// intern requirements and solve.
    fn build_provider(&self) -> PortageDependencyProvider {
        let adapted = RepoAdapter {
            repo: self.repo.as_ref(),
        };
        let use_config = ceded_resolvo_config(&self.ceded);
        if self.installed.is_empty() {
            PortageDependencyProvider::new(&adapted, &use_config)
        } else {
            let set = self.installed_set();
            PortageDependencyProvider::with_installed(&adapted, &use_config, &set)
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

impl<'repo> Solver for SolverAdapter<'repo> {
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
        Ok(build_plan(solver.provider(), &solution, &self.ceded))
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

/// Harvest every `SolverDecided` flag declared by any version's `desired_use`
/// across the repository, with the caller's preferred value. If a flag is
/// ceded with differing `prefer` across versions, the last-wins order is
/// deterministic but arbitrary — consistent with pubgrub's per-(cpn,flag)
/// granularity only when the flag's ceding is uniform within a package.
fn harvest_ceded_flags(
    repo: &dyn portage_solver::PackageRepository,
) -> HashMap<Interned<DefaultInterner>, bool> {
    let mut ceded: HashMap<Interned<DefaultInterner>, bool> = HashMap::new();
    for cpn in repo.all_packages() {
        for (cpv, _facts) in repo.versions_for(&cpn) {
            let desired = repo.desired_use(&cpv);
            for flag in desired.solver_decided_flags() {
                let prefer = match desired.get(flag) {
                    UseFlagState::SolverDecided { prefer } => prefer,
                    _ => false,
                };
                ceded.entry(flag).or_insert(prefer);
            }
        }
    }
    ceded
}

/// Translate the harvested ceded-flag map into a resolvo `UseConfig`:
/// `solver_decided` carries the flags, `solver_decided_prefer` carries each
/// flag's preferred value.
fn ceded_resolvo_config(ceded: &HashMap<Interned<DefaultInterner>, bool>) -> ResUseConfig {
    ResUseConfig {
        enabled: HashSet::new(),
        disabled: HashSet::new(),
        solver_decided: ceded.keys().copied().collect(),
        solver_decided_prefer: ceded.iter().map(|(f, p)| (*f, *p)).collect(),
    }
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
fn build_plan(
    provider: &PortageDependencyProvider,
    solution: &[resolvo::SolvableId],
    ceded: &HashMap<Interned<DefaultInterner>, bool>,
) -> Plan {
    // Strip solver-internal decision virtuals (__internal__/USE_* / NotUSE_*):
    // they encode ceded-USE choices, not real packages, and are surfaced
    // separately via `ceded_flags`.
    let real_sids: Vec<resolvo::SolvableId> = solution
        .iter()
        .copied()
        .filter(|&sid| provider.pool().resolve_solvable(sid).cpv.cpn.category != "__internal__")
        .collect();

    let selected: Vec<SelectedPackage> = real_sids
        .iter()
        .map(|&sid| solvable_to_selected(provider, sid))
        .collect();

    let graph: Vec<DepEdge> = provider
        .dependency_graph(&real_sids)
        .into_iter()
        .map(|edge| map_dep_edge(provider, &edge))
        .collect();

    let install_order: Vec<SelectedPackage> = match provider.install_order(&real_sids) {
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
        dropped_deps: provider
            .dropped_deps(&real_sids)
            .into_iter()
            .map(|cpn| portage_solver::DroppedDep { cpn })
            .collect(),
        // Only USE-dep violations are possible from a successful resolvo solve
        // (blockers and ::repo are hard-enforced; see pool::Violation docs).
        violations: provider
            .check_use_dep_violations(&real_sids)
            .into_iter()
            .map(|v| match v {
                crate::pool::Violation::UseDep { pkg, detail } => {
                    portage_solver::Violation::UseDep(pkg, detail)
                }
            })
            .collect(),
        // The "needed" set derived from unsatisfied USE-deps. upgrade_to is
        // always None (resolvo has no upgrade fixpoint).
        use_flag_requirements: provider
            .use_flag_requirements(&real_sids)
            .into_iter()
            .map(|r| portage_solver::UseFlagRequirement {
                cpn: r.cpn,
                version: r.version,
                upgrade_to: None,
                required_enabled: r.required_enabled,
                required_disabled: r.required_disabled,
                required_by: r.required_by,
            })
            .collect(),
        ceded_flags: provider
            .ceded_flags(solution, ceded)
            .into_iter()
            .map(|(cpn, flag, value, prefer)| portage_solver::CededFlag {
                cpn,
                flag,
                value,
                flipped: value != prefer,
            })
            .collect(),
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

    /// A dep on a package absent from the repo is reported as dropped, while
    /// the resolvable packages still form the plan.
    #[test]
    fn resolvo_solver_reports_dropped_dep() {
        struct Repo;
        impl SolverRepo for Repo {
            fn all_packages(&self) -> Vec<Cpn> {
                vec![Cpn::parse("app-misc/top").unwrap()]
            }
            fn versions_for(&self, cpn: &Cpn) -> Vec<(Cpv, VersionFacts)> {
                if cpn != &Cpn::parse("app-misc/top").unwrap() {
                    return Vec::new();
                }
                vec![(
                    Cpv::parse("app-misc/top-1.0").unwrap(),
                    VersionFacts {
                        slot: slot("0"),
                        subslot: None,
                        repo: None,
                        iuse: Vec::new(),
                        iuse_defaults: Default::default(),
                        deps: SolverDeps {
                            depend: vec![DepEntry::Atom(Dep::parse("dev-libs/missing").unwrap())],
                            ..SolverDeps::default()
                        },
                        required_use: None,
                    },
                )]
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

        // top is selected; the missing dep is reported as dropped.
        assert!(
            plan.selected
                .iter()
                .any(|p| p.cpn.package.as_str() == "top")
        );
        let dropped: Vec<String> = plan
            .dropped_deps
            .iter()
            .map(|d| format!("{}/{}", d.cpn.category, d.cpn.package))
            .collect();
        assert_eq!(dropped, vec!["dev-libs/missing".to_string()]);
    }

    /// A USE-dep constraint (`[flag]`) not satisfied by the target's effective
    /// USE is reported as a post-solve violation — the solve still succeeds
    /// because resolvo does not enforce USE-deps during solving.
    #[test]
    fn resolvo_solver_reports_use_dep_violation() {
        struct Repo;
        impl SolverRepo for Repo {
            fn all_packages(&self) -> Vec<Cpn> {
                vec![
                    Cpn::parse("app-misc/foo").unwrap(),
                    Cpn::parse("dev-libs/bar").unwrap(),
                ]
            }
            fn versions_for(&self, cpn: &Cpn) -> Vec<(Cpv, VersionFacts)> {
                let mk = |v: &str, iuse: Vec<Interned<DefaultInterner>>, deps: SolverDeps| {
                    (
                        Cpv::parse(v).unwrap(),
                        VersionFacts {
                            slot: slot("0"),
                            subslot: None,
                            repo: None,
                            iuse,
                            iuse_defaults: Default::default(),
                            deps,
                            required_use: None,
                        },
                    )
                };
                match format!("{}/{}", cpn.category, cpn.package).as_str() {
                    "app-misc/foo" => vec![mk(
                        "app-misc/foo-1.0",
                        Vec::new(),
                        SolverDeps {
                            depend: vec![DepEntry::Atom(Dep::parse("dev-libs/bar[ssl]").unwrap())],
                            ..SolverDeps::default()
                        },
                    )],
                    // bar declares ssl in IUSE but its desired USE leaves it off,
                    // so the [ssl] use-dep is unsatisfied.
                    "dev-libs/bar" => vec![mk(
                        "dev-libs/bar-1.0",
                        vec![Interned::intern("ssl")],
                        SolverDeps::default(),
                    )],
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
                Cpn::parse("app-misc/foo").unwrap(),
                None,
            )],
        )
        .expect("resolve");

        // foo and bar are both selected (use-deps don't block the solve);
        // the unsatisfied [ssl] is surfaced as a violation.
        let has_use_dep = plan
            .violations
            .iter()
            .any(|v| matches!(v, portage_solver::Violation::UseDep(_, _)));
        assert!(
            has_use_dep,
            "expected a [ssl] use-dep violation, got {:?}",
            plan.violations
        );
    }

    /// The same unsatisfied USE-dep that produces a violation also produces a
    /// use_flag_requirement: bar must enable ssl. Grouped by target, with the
    /// requirer recorded, and no upgrade_to (resolvo has no upgrade fixpoint).
    #[test]
    fn resolvo_solver_reports_use_flag_requirement() {
        struct Repo;
        impl SolverRepo for Repo {
            fn all_packages(&self) -> Vec<Cpn> {
                vec![
                    Cpn::parse("app-misc/foo").unwrap(),
                    Cpn::parse("dev-libs/bar").unwrap(),
                ]
            }
            fn versions_for(&self, cpn: &Cpn) -> Vec<(Cpv, VersionFacts)> {
                let mk = |v: &str, iuse: Vec<Interned<DefaultInterner>>, deps: SolverDeps| {
                    (
                        Cpv::parse(v).unwrap(),
                        VersionFacts {
                            slot: slot("0"),
                            subslot: None,
                            repo: None,
                            iuse,
                            iuse_defaults: Default::default(),
                            deps,
                            required_use: None,
                        },
                    )
                };
                match format!("{}/{}", cpn.category, cpn.package).as_str() {
                    "app-misc/foo" => vec![mk(
                        "app-misc/foo-1.0",
                        Vec::new(),
                        SolverDeps {
                            depend: vec![DepEntry::Atom(Dep::parse("dev-libs/bar[ssl]").unwrap())],
                            ..SolverDeps::default()
                        },
                    )],
                    "dev-libs/bar" => vec![mk(
                        "dev-libs/bar-1.0",
                        vec![Interned::intern("ssl")],
                        SolverDeps::default(),
                    )],
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
                Cpn::parse("app-misc/foo").unwrap(),
                None,
            )],
        )
        .expect("resolve");

        let req = plan
            .use_flag_requirements
            .iter()
            .find(|r| r.cpn.package.as_str() == "bar")
            .expect("expected a use_flag_requirement for bar");
        assert!(
            req.required_enabled.iter().any(|f| f.as_str() == "ssl"),
            "expected ssl in required_enabled, got {:?}",
            req.required_enabled
        );
        assert!(
            req.required_by.iter().any(|s| s.contains("foo")),
            "expected foo in required_by, got {:?}",
            req.required_by
        );
        assert!(req.upgrade_to.is_none(), "resolvo has no upgrade fixpoint");
    }

    /// A ceded (solver-decided) flag is chosen by the solver and reported in
    /// Plan.ceded_flags, with the owning cpn and the picked value. The flag's
    /// conditional dep fires accordingly. Decision virtuals stay out of
    /// `selected`.
    #[test]
    fn resolvo_solver_reports_ceded_flag() {
        struct Repo;
        impl SolverRepo for Repo {
            fn all_packages(&self) -> Vec<Cpn> {
                vec![Cpn::parse("app-misc/foo").unwrap()]
            }
            fn versions_for(&self, cpn: &Cpn) -> Vec<(Cpv, VersionFacts)> {
                if cpn != &Cpn::parse("app-misc/foo").unwrap() {
                    return Vec::new();
                }
                vec![(
                    Cpv::parse("app-misc/foo-1.0").unwrap(),
                    VersionFacts {
                        slot: slot("0"),
                        subslot: None,
                        repo: None,
                        iuse: vec![Interned::intern("ssl")],
                        iuse_defaults: Default::default(),
                        deps: SolverDeps {
                            depend: vec![DepEntry::UseConditional {
                                flag: Interned::intern("ssl"),
                                negate: false,
                                children: vec![DepEntry::Atom(
                                    Dep::parse("dev-libs/openssl").unwrap(),
                                )],
                            }],
                            ..SolverDeps::default()
                        },
                        required_use: None,
                    },
                )]
            }
            // Cede `ssl` to the solver, preferring OFF. The deps only reference
            // ssl on foo itself, so nothing forces it on → solver keeps it off.
            fn desired_use(&self, _: &Cpv) -> UseConfig {
                let mut cfg = UseConfig::new();
                cfg.solver_decide(Interned::intern("ssl"), false);
                cfg
            }
        }

        let mut solver = SolverAdapter::new(Box::new(Repo));
        let plan = Solver::resolve_targets(
            &mut solver,
            &[TargetSpec::any_in(
                Cpn::parse("app-misc/foo").unwrap(),
                None,
            )],
        )
        .expect("resolve");

        // No decision virtual leaked into selected.
        assert!(
            plan.selected
                .iter()
                .all(|p| p.cpn.category != "__internal__")
        );
        // The ceded flag is reported with its owning cpn and chosen value (off).
        let ceded: Vec<_> = plan
            .ceded_flags
            .iter()
            .filter(|c| c.flag.as_str() == "ssl")
            .collect();
        assert!(
            !ceded.is_empty(),
            "expected a ceded ssl flag, got {:?}",
            plan.ceded_flags
        );
        assert!(ceded.iter().all(|c| c.cpn.package.as_str() == "foo"));
        assert!(
            ceded.iter().all(|c| !c.value),
            "expected ssl ceded to OFF (prefer off, nothing forces on), got {:?}",
            ceded
        );
        assert!(
            ceded.iter().all(|c| !c.flipped),
            "value matches prefer → not flipped"
        );
    }
}
