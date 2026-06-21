use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use clap::Parser;
use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, Dep, Operator};
use portage_metadata::CacheEntry;
use portage_metadata::{Keyword, Stability};
use portage_repo::Repository;

/// Compare portage-atom-pubgrub and portage-atom-resolvo on real Gentoo data.
#[derive(Parser)]
struct Args {
    /// Path to the Gentoo repository.
    repo: PathBuf,

    /// Packages to resolve (e.g. "dev-libs/openssl" "sys-libs/zlib").
    #[clap(required = true)]
    packages: Vec<String>,

    /// Accept versions keyworded for this arch (stable or ~testing).
    #[clap(long, default_value = "arm64")]
    keyword: String,

    /// Run the comparison through the shared `portage_solver::Solver` trait
    /// (Box<dyn Solver>) instead of each bridge's concrete API. Exercises the
    /// unified Plan surface end-to-end.
    #[clap(long)]
    trait_compare: bool,
}

struct RepoData {
    cpns: Vec<Cpn>,
    versions: HashMap<Cpn, Vec<(Cpv, CacheEntry)>>,
    repo_name: String,
    keyword: String,
}

fn keyword_accepts(keywords: &[Keyword], arch: &str) -> bool {
    keywords.iter().any(|kw| {
        kw.arch.as_str() == arch && matches!(kw.stability, Stability::Stable | Stability::Testing)
    })
}

/// Hardcoded effective USE for the benchmark comparison (the flags the pubgrub
/// path enables). Shared by both the concrete-API and `--trait-compare` paths
/// so they solve the same policy.
const BENCH_USE_FLAGS: &[&str] = &[
    "acl",
    "arm64",
    "big-endian",
    "bzip2",
    "cpu_flags_arm_edsp",
    "cpu_flags_arm_v8",
    "cpu_flags_arm_vfp",
    "cpu_flags_arm_vfp-d32",
    "cpu_flags_arm_vfpv3",
    "cpu_flags_arm_vfpv4",
    "crypt",
    "dist",
    "elibc_glibc",
    "gdbm",
    "iconv",
    "ipv6",
    "kernel_linux",
    "libtirpc",
    "llvm_targets_AArch64",
    "llvm_targets_RISCV",
    "mimalloc",
    "ncurses",
    "nls",
    "npm",
    "openmp",
    "pam",
    "pcre",
    "python_single_target_python3_13",
    "python_targets_python3_13",
    "python_targets_python3_14",
    "qemu",
    "readline",
    "relapack",
    "rust-analyzer",
    "rust-src",
    "seccomp",
    "split-usr",
    "ssl",
    "test-rust",
    "unicode",
    "xattr",
    "zlib",
];

fn load_repo(path: &PathBuf, keyword: &str) -> RepoData {
    eprintln!("Loading repository from {}...", path.display());
    let start = Instant::now();
    let repo = Repository::open(path).expect("failed to open repo");
    let repo_name = repo.name().to_string();

    let ebuilds = repo
        .ebuilds()
        .expect("failed to walk ebuilds")
        .collect_vec();
    eprintln!(
        "Found {} ebuilds in {:.1}s",
        ebuilds.len(),
        start.elapsed().as_secs_f64()
    );

    let mut cpns_set: HashSet<Cpn> = HashSet::new();
    let mut versions: HashMap<Cpn, Vec<(Cpv, CacheEntry)>> = HashMap::new();
    let mut errors = 0usize;

    let load_start = Instant::now();
    for ebuild in &ebuilds {
        let cpv = ebuild.cpv().clone();
        let cpn = cpv.cpn;

        match repo.cache_entry(&cpv) {
            Ok(Some(entry)) => {
                cpns_set.insert(cpn);
                versions.entry(cpn).or_default().push((cpv, entry));
            }
            _ => {
                errors += 1;
            }
        }
    }

    eprintln!(
        "Loaded {} packages, {} versions in {:.1}s ({} cache misses)",
        cpns_set.len(),
        versions.values().map(|v| v.len()).sum::<usize>(),
        load_start.elapsed().as_secs_f64(),
        errors,
    );

    let mut cpns: Vec<Cpn> = cpns_set.into_iter().collect();
    cpns.sort_by_key(|c| format!("{}/{}", c.category, c.package));

    RepoData {
        cpns,
        versions,
        repo_name,
        keyword: keyword.to_string(),
    }
}

pub(crate) mod pubgrub_solver {
    use super::*;
    use portage_atom_pubgrub::{
        IUseDefault, PackageDeps, PackageRepository, PackageVersions, PortageDependencyProvider,
        PortagePackage, PortageVersionSet, UseConfig, UseFlagState,
    };

    pub struct Adapter<'a> {
        data: &'a RepoData,
        use_config: UseConfig,
    }

    impl<'a> Adapter<'a> {
        pub fn new(data: &'a RepoData, use_config: UseConfig) -> Self {
            Self { data, use_config }
        }
    }

    impl PackageRepository for Adapter<'_> {
        fn all_packages(&self) -> Vec<Cpn> {
            self.data.cpns.clone()
        }

        fn desired_use(&self, cpv: &Cpv) -> UseConfig {
            let mut cfg = self.use_config.clone();
            if let Some(entries) = self.data.versions.get(&cpv.cpn)
                && let Some((_, cache)) = entries.iter().find(|(c, _)| c.version == cpv.version)
            {
                for iu in &cache.metadata.iuse {
                    let flag = Interned::intern(iu.name());
                    if cfg.get_opt(flag).is_none()
                        && let Some(def) = iu.default
                    {
                        cfg.set(
                            flag,
                            match def {
                                portage_metadata::IUseDefault::Enabled => UseFlagState::Enabled,
                                portage_metadata::IUseDefault::Disabled => UseFlagState::Disabled,
                            },
                        );
                    }
                }
            }
            cfg
        }

        fn versions_for(&self, cpn: &Cpn) -> Vec<(Cpv, PackageVersions)> {
            self.data
                .versions
                .get(cpn)
                .map(|entries| {
                    entries
                        .iter()
                        .filter(|(_, cache)| {
                            keyword_accepts(&cache.metadata.keywords, &self.data.keyword)
                        })
                        .map(|(cpv, cache)| {
                            let meta = &cache.metadata;
                            let slot = if meta.slot.slot.as_str().is_empty() {
                                None
                            } else {
                                Some(meta.slot.slot)
                            };
                            let subslot = meta.slot.subslot;
                            let repo =
                                Some(Interned::<DefaultInterner>::intern(&self.data.repo_name));
                            let iuse: Vec<Interned<DefaultInterner>> = meta
                                .iuse
                                .iter()
                                .map(|iu| Interned::intern(iu.name()))
                                .collect();
                            let iuse_defaults: HashMap<Interned<DefaultInterner>, IUseDefault> =
                                meta.iuse
                                    .iter()
                                    .filter_map(|iu| {
                                        iu.default.map(|d| {
                                            let val = match d {
                                                portage_metadata::IUseDefault::Enabled => {
                                                    IUseDefault::Enabled
                                                }
                                                portage_metadata::IUseDefault::Disabled => {
                                                    IUseDefault::Disabled
                                                }
                                            };
                                            (Interned::intern(iu.name()), val)
                                        })
                                    })
                                    .collect();
                            let deps = PackageDeps {
                                depend: meta.depend.clone(),
                                rdepend: meta.rdepend.clone(),
                                bdepend: meta.bdepend.clone(),
                                pdepend: meta.pdepend.clone(),
                                idepend: meta.idepend.clone(),
                            };
                            (
                                cpv.clone(),
                                PackageVersions {
                                    slot,
                                    subslot,
                                    repo,
                                    iuse,
                                    iuse_defaults,
                                    deps,
                                    // REQUIRED_USE is a dormant Level-C fact; the
                                    // benchmark adapter does not feed it.
                                    required_use: None,
                                },
                            )
                        })
                        .collect()
                })
                .unwrap_or_default()
        }
    }

    pub fn resolve(data: &RepoData, targets: &[String]) -> Result<Vec<String>, String> {
        let mut use_config = UseConfig::new();
        for flag in BENCH_USE_FLAGS {
            use_config.enable(Interned::intern(flag));
        }
        use_config.disable(Interned::intern("pthread"));
        let adapter = Adapter::new(data, use_config);
        let mut provider = PortageDependencyProvider::new(adapter);

        let mut root_deps = Vec::new();
        for target in targets {
            let dep = Dep::parse(target).map_err(|e| format!("bad target '{}': {}", target, e))?;
            let pkg = match data.versions.get(&dep.cpn) {
                Some(entries) => {
                    let mut slots: Vec<_> = entries
                        .iter()
                        .filter_map(|(_, cache)| {
                            let s = &cache.metadata.slot.slot;
                            if s.as_str().is_empty() {
                                None
                            } else {
                                Some(*s)
                            }
                        })
                        .collect();
                    slots.sort_by(|a, b| a.as_str().cmp(b.as_str()));
                    slots.dedup();
                    match slots.as_slice() {
                        [] => PortagePackage::unslotted(dep.cpn),
                        [sole] => PortagePackage::slotted(dep.cpn, *sole),
                        _ => {
                            let latest_slot = entries
                                .iter()
                                .filter_map(|(cpv, cache)| {
                                    let s = &cache.metadata.slot.slot;
                                    if s.as_str().is_empty() {
                                        None
                                    } else {
                                        Some((cpv.version.clone(), *s))
                                    }
                                })
                                .max_by(|a, b| a.0.cmp(&b.0))
                                .map(|(_, s)| s)
                                .unwrap();
                            PortagePackage::slotted(dep.cpn, latest_slot)
                        }
                    }
                }
                None => PortagePackage::unslotted(dep.cpn),
            };
            let vs = match &dep.version {
                Some(v) => {
                    let op = dep.op.unwrap_or(Operator::GreaterOrEqual);
                    PortageVersionSet::from_operator(op, dep.glob, v.clone())
                }
                None => PortageVersionSet::any(),
            };
            root_deps.push((pkg, vs));
        }

        let dropped = provider.dropped_deps();
        if !dropped.is_empty() {
            let mut cpns: Vec<String> = dropped
                .iter()
                .filter(|d| !d.package.is_virtual())
                .map(|d| d.package.cpn().to_string())
                .collect();
            cpns.sort();
            cpns.dedup();
            eprintln!(
                "WARNING: {} dropped deps ({} unique CPNs):",
                dropped.len(),
                cpns.len()
            );
            for cpn in cpns.iter().take(80) {
                eprintln!("  {}", cpn);
            }
            if cpns.len() > 80 {
                eprintln!("  ... and {} more", cpns.len() - 80);
            }
        }

        let start = Instant::now();
        match provider.resolve_targets(root_deps) {
            Ok(solution) => {
                let elapsed = start.elapsed();
                let mut pkgs: Vec<_> = solution.iter().collect();
                pkgs.sort_by_key(|(p, _)| p.to_string());
                eprintln!(
                    "\n=== PubGrub: resolved {} packages in {:.1}ms ===",
                    pkgs.len(),
                    elapsed.as_secs_f64() * 1000.0
                );

                let mut names: Vec<String> = Vec::new();
                for (pkg, ver) in &pkgs {
                    let line = format!("{}-{}", pkg.cpn(), ver);
                    eprintln!("  {}", line);
                    names.push(line);
                }
                let blocker_errors = provider.check_blockers(&solution);
                if !blocker_errors.is_empty() {
                    eprintln!("  Blocker conflicts: {:?}", blocker_errors);
                }
                Ok(names)
            }
            Err(pubgrub::PubGrubError::NoSolution(derivation_tree)) => {
                let elapsed = start.elapsed();
                let msg = format!("{:?}", derivation_tree);
                let truncated = if msg.len() > 1000 {
                    format!("{}...[truncated]", &msg[..1000])
                } else {
                    msg
                };
                Err(format!(
                    "PubGrub: no solution in {:.1}ms: {}",
                    elapsed.as_secs_f64() * 1000.0,
                    truncated
                ))
            }
            Err(e) => {
                let elapsed = start.elapsed();
                Err(format!(
                    "PubGrub: error in {:.1}ms: {:?}",
                    elapsed.as_secs_f64() * 1000.0,
                    e
                ))
            }
        }
    }
}

pub(crate) mod resolvo_solver {
    use super::*;
    use portage_atom_resolvo::{
        PackageDeps, PackageMetadata, PackageRepository as ResolvoRepo, PortageDependencyProvider,
        UseConfig,
    };

    pub struct Adapter<'a> {
        data: &'a RepoData,
    }

    impl<'a> Adapter<'a> {
        pub fn new(data: &'a RepoData) -> Self {
            Self { data }
        }
    }

    impl ResolvoRepo for Adapter<'_> {
        fn all_packages(&self) -> Vec<Cpn> {
            self.data.cpns.clone()
        }

        fn versions_for(&self, cpn: &Cpn) -> Vec<PackageMetadata> {
            self.data
                .versions
                .get(cpn)
                .map(|entries| {
                    entries
                        .iter()
                        .filter(|(_, cache)| {
                            keyword_accepts(&cache.metadata.keywords, &self.data.keyword)
                        })
                        .map(|(cpv, cache)| {
                            let meta = &cache.metadata;
                            let slot = if meta.slot.slot.as_str().is_empty() {
                                None
                            } else {
                                Some(meta.slot.slot)
                            };
                            let subslot = meta.slot.subslot;
                            let repo =
                                Some(Interned::<DefaultInterner>::intern(&self.data.repo_name));
                            let use_flags: HashSet<Interned<DefaultInterner>> = meta
                                .iuse
                                .iter()
                                .map(|iu| Interned::intern(iu.name()))
                                .collect();
                            PackageMetadata {
                                cpv: cpv.clone(),
                                slot,
                                subslot,
                                iuse: use_flags.iter().copied().collect(),
                                use_flags,
                                repo,
                                dependencies: PackageDeps {
                                    depend: meta.depend.clone(),
                                    rdepend: meta.rdepend.clone(),
                                    bdepend: meta.bdepend.clone(),
                                    pdepend: meta.pdepend.clone(),
                                    idepend: meta.idepend.clone(),
                                },
                            }
                        })
                        .collect()
                })
                .unwrap_or_default()
        }
    }

    pub fn resolve(data: &RepoData, targets: &[String]) -> Result<Vec<String>, String> {
        let adapter = Adapter::new(data);
        let use_config = UseConfig::default();
        let mut provider = PortageDependencyProvider::new(&adapter, &use_config);

        // Debug: find names with no candidates
        {
            let _pool = provider.pool();
            let empty_names: Vec<_> = provider.debug_empty_candidates();
            if !empty_names.is_empty() {
                eprintln!(
                    "WARNING: {} names with no candidates after construction:",
                    empty_names.len()
                );
                for name in &empty_names[..empty_names.len().min(20)] {
                    eprintln!("  {}", name);
                }
                if empty_names.len() > 20 {
                    eprintln!("  ... and {} more", empty_names.len() - 20);
                }
            }
        }

        let mut requirements = Vec::new();
        for target in targets {
            let dep = Dep::parse(target).map_err(|e| format!("bad target '{}': {}", target, e))?;
            let req = provider.intern_requirement(&dep);
            requirements.push(req);
        }

        let problem = resolvo::Problem::new().requirements(requirements);
        let mut solver = resolvo::Solver::new(provider);

        let start = Instant::now();
        match solver.solve(problem) {
            Ok(solution) => {
                let elapsed = start.elapsed();
                eprintln!(
                    "\n=== Resolvo: resolved {} packages in {:.1}ms ===",
                    solution.len(),
                    elapsed.as_secs_f64() * 1000.0
                );
                let mut items: Vec<_> = solution
                    .iter()
                    .map(|&sid| {
                        let meta = solver.provider().package_metadata(sid);
                        (meta.cpv.cpn, meta.cpv.version.clone(), meta.slot)
                    })
                    .collect();
                items.sort_by_key(|(cpn, _, _)| format!("{}/{}", cpn.category, cpn.package));
                let mut names: Vec<String> = Vec::new();
                for (cpn, ver, _slot) in &items {
                    let line = format!("{}-{}", cpn, ver);
                    eprintln!("  {}", line);
                    names.push(line);
                }
                Ok(names)
            }
            Err(e) => {
                let elapsed = start.elapsed();
                if let resolvo::UnsolvableOrCancelled::Unsolvable(conflict) = &e {
                    let report = conflict.display_user_friendly(&solver);
                    eprintln!(
                        "Resolvo: no solution in {:.1}ms:\n{}",
                        elapsed.as_secs_f64() * 1000.0,
                        report
                    );
                    Err(format!(
                        "Resolvo: no solution in {:.1}ms (see above)",
                        elapsed.as_secs_f64() * 1000.0
                    ))
                } else {
                    Err(format!(
                        "Resolvo: error in {:.1}ms: {:?}",
                        elapsed.as_secs_f64() * 1000.0,
                        e
                    ))
                }
            }
        }
    }
}

/// Trait-based comparison: build one shared `portage_solver::PackageRepository`
/// from the loaded repo data and run both bridges through `Box<dyn Solver>`,
/// diffing the unified `Plan` surface. This exercises everything the
/// `portage-solver` abstraction provides end-to-end, independent of each
/// bridge's concrete API.
mod trait_compare {
    use super::*;
    use portage_solver::{
        PackageDeps as SolverDeps, PackageRepository as SolverRepo, TargetSpec,
        UseConfig as SolverUseConfig, VersionFacts,
    };
    // `Solver` is brought in via `portage_solver::Solver` at the call site to
    // avoid ambiguity with pubgrub's re-export.

    /// Shared solver-agnostic repository over the loaded `RepoData`. Each
    /// version's `desired_use` is the benchmark USE set intersected with the
    /// version's IUSE — the same policy the concrete pubgrub path applies.
    struct SharedRepo<'a> {
        data: &'a RepoData,
        use_config: SolverUseConfig,
    }

    impl<'a> SolverRepo for SharedRepo<'a> {
        fn all_packages(&self) -> Vec<Cpn> {
            self.data.cpns.clone()
        }

        fn versions_for(&self, cpn: &Cpn) -> Vec<(Cpv, VersionFacts)> {
            self.data
                .versions
                .get(cpn)
                .map(|entries| {
                    entries
                        .iter()
                        .filter(|(_, cache)| {
                            keyword_accepts(&cache.metadata.keywords, &self.data.keyword)
                        })
                        .map(|(cpv, cache)| {
                            let meta = &cache.metadata;
                            let slot = if meta.slot.slot.as_str().is_empty() {
                                None
                            } else {
                                Some(meta.slot.slot)
                            };
                            let subslot = meta.slot.subslot;
                            let repo =
                                Some(Interned::<DefaultInterner>::intern(&self.data.repo_name));
                            let iuse: Vec<Interned<DefaultInterner>> = meta
                                .iuse
                                .iter()
                                .map(|iu| Interned::intern(iu.name()))
                                .collect();
                            (
                                cpv.clone(),
                                VersionFacts {
                                    slot,
                                    subslot,
                                    repo,
                                    iuse,
                                    iuse_defaults: HashMap::new(),
                                    deps: SolverDeps {
                                        depend: meta.depend.clone(),
                                        rdepend: meta.rdepend.clone(),
                                        bdepend: meta.bdepend.clone(),
                                        pdepend: meta.pdepend.clone(),
                                        idepend: meta.idepend.clone(),
                                    },
                                    required_use: None,
                                },
                            )
                        })
                        .collect()
                })
                .unwrap_or_default()
        }

        fn desired_use(&self, _: &Cpv) -> SolverUseConfig {
            self.use_config.clone()
        }
    }

    fn bench_use_config() -> SolverUseConfig {
        let mut cfg = SolverUseConfig::new();
        for flag in BENCH_USE_FLAGS {
            cfg.enable(Interned::intern(flag));
        }
        cfg.disable(Interned::intern("pthread"));
        cfg
    }

    /// PubGrub's own `UseConfig` from the same benchmark flag set, for the
    /// pubgrub bridge's concrete `PackageRepository` adapter.
    fn pubgrub_use_config() -> portage_atom_pubgrub::UseConfig {
        let mut cfg = portage_atom_pubgrub::UseConfig::new();
        for flag in BENCH_USE_FLAGS {
            cfg.enable(Interned::intern(flag));
        }
        cfg.disable(Interned::intern("pthread"));
        cfg
    }

    /// Build a `TargetSpec` from a CLI atom string, picking the newest-keyworded
    /// slot when the atom is unqualified (mirrors the concrete paths' choice).
    fn target_spec(data: &RepoData, target: &str) -> Result<TargetSpec, String> {
        let dep = Dep::parse(target).map_err(|e| format!("bad target '{target}': {e}"))?;
        let entries = data
            .versions
            .get(&dep.cpn)
            .ok_or_else(|| format!("no versions for {target}"))?;
        // Newest accepted version's slot, mirroring the concrete paths.
        let slot = entries
            .iter()
            .filter(|(_, c)| keyword_accepts(&c.metadata.keywords, &data.keyword))
            .max_by(|a, b| a.0.version.cmp(&b.0.version))
            .and_then(|(_, c)| {
                let s = c.metadata.slot.slot;
                if s.as_str().is_empty() { None } else { Some(s) }
            });
        Ok(TargetSpec {
            cpn: dep.cpn,
            slot,
            op: dep.op,
            version: dep.version,
            glob: dep.glob,
        })
    }

    fn run_solver(
        label: &str,
        solver: &mut dyn portage_solver::Solver,
        targets: &[TargetSpec],
    ) -> Result<Vec<String>, String> {
        let start = Instant::now();
        match solver.resolve_targets(targets) {
            Ok(plan) => {
                let elapsed = start.elapsed();
                let mut names: Vec<String> = plan
                    .selected
                    .iter()
                    .map(|p| format!("{}-{}", p.cpn, p.version))
                    .collect();
                names.sort();
                eprintln!(
                    "\n=== {label}: resolved {} packages in {:.1}ms (dropped {}, violations {}, ceded {}) ===",
                    names.len(),
                    elapsed.as_secs_f64() * 1000.0,
                    plan.dropped_deps.len(),
                    plan.violations.len(),
                    plan.ceded_flags.len(),
                );
                Ok(names)
            }
            Err(e) => Err(format!("{label}: {e:?}")),
        }
    }

    pub fn run(data: &RepoData, targets: &[String], _keyword: &str) {
        let specs: Vec<TargetSpec> = match targets
            .iter()
            .map(|t| target_spec(data, t))
            .collect::<Result<_, _>>()
        {
            Ok(s) => s,
            Err(e) => {
                eprintln!("ERROR: {e}");
                return;
            }
        };

        // Two independent solvers over the same shared repository.
        //
        // PubGrub's bridge still consumes its own `PackageRepository` (the
        // vocabulary dedup is not yet done), so we reuse `pubgrub_solver`'s
        // adapter for it and `SharedRepo` for resolvo. Both providers implement
        // `portage_solver::Solver`, so they're driven uniformly behind
        // `Box<dyn Solver>` after construction.
        let mut pg: Box<dyn portage_solver::Solver> =
            Box::new(portage_atom_pubgrub::PortageDependencyProvider::new(
                pubgrub_solver::Adapter::new(data, pubgrub_use_config()),
            ));
        let mut res: Box<dyn portage_solver::Solver> = Box::new(
            portage_atom_resolvo::SolverAdapter::new(Box::new(SharedRepo {
                data,
                use_config: bench_use_config(),
            })),
        );

        let pg_result = run_solver("PubGrub(trait)", &mut *pg, &specs);
        let res_result = run_solver("Resolvo(trait)", &mut *res, &specs);

        for r in [&pg_result, &res_result] {
            if let Err(e) = r {
                eprintln!("ERROR: {e}");
            }
        }

        if let (Ok(pg_pkgs), Ok(res_pkgs)) = (&pg_result, &res_result) {
            let pg_set: HashSet<&str> = pg_pkgs.iter().map(|s| s.as_str()).collect();
            let res_set: HashSet<&str> = res_pkgs.iter().map(|s| s.as_str()).collect();
            let only_pg: Vec<_> = pg_set.difference(&res_set).copied().collect();
            let only_res: Vec<_> = res_set.difference(&pg_set).copied().collect();
            eprintln!(
                "\n=== Trait diff: {} shared, {} only PubGrub, {} only Resolvo ===",
                pg_set.intersection(&res_set).count(),
                only_pg.len(),
                only_res.len(),
            );
            for (label, mut s) in [("Only in Resolvo", only_res), ("Only in PubGrub", only_pg)] {
                if !s.is_empty() {
                    s.sort();
                    eprintln!("\n  {label}:");
                    for p in s {
                        eprintln!("    {p}");
                    }
                }
            }
        }
    }
}

fn main() {
    let args = Args::parse();
    let data = load_repo(&args.repo, &args.keyword);
    eprintln!("Accepting keywords: {} ~{}", args.keyword, args.keyword);

    let targets: Vec<String> = args.packages;

    if args.trait_compare {
        return trait_compare::run(&data, &targets, &args.keyword);
    }

    let mut all_dep_cpns: HashSet<Cpn> = HashSet::new();
    for entries in data.versions.values() {
        for (_, cache) in entries {
            let m = &cache.metadata;
            for cls in [&m.depend, &m.rdepend, &m.bdepend, &m.pdepend, &m.idepend] {
                collect_cpns(cls, &mut all_dep_cpns);
            }
        }
    }
    let missing: Vec<_> = all_dep_cpns
        .iter()
        .filter(|c| !data.versions.contains_key(c))
        .collect();
    eprintln!(
        "{} packages referenced in deps but missing from repo",
        missing.len()
    );
    for c in &missing {
        eprintln!("  {}/{}", c.category, c.package);
    }

    let pg_result = pubgrub_solver::resolve(&data, &targets);
    let res_result = resolvo_solver::resolve(&data, &targets);

    if let Err(e) = &pg_result {
        eprintln!("ERROR: {}", e);
    }
    if let Err(e) = &res_result {
        eprintln!("ERROR: {}", e);
    }

    if let (Ok(pg_pkgs), Ok(res_pkgs)) = (&pg_result, &res_result) {
        let pg_set: HashSet<&str> = pg_pkgs.iter().map(|s| s.as_str()).collect();
        let res_set: HashSet<&str> = res_pkgs.iter().map(|s| s.as_str()).collect();

        let only_pg: Vec<_> = pg_set.difference(&res_set).copied().collect();
        let only_res: Vec<_> = res_set.difference(&pg_set).copied().collect();

        eprintln!(
            "\n=== Diff: {} shared, {} only in PubGrub, {} only in Resolvo ===",
            pg_set.intersection(&res_set).count(),
            only_pg.len(),
            only_res.len(),
        );

        if !only_res.is_empty() {
            let mut s = only_res;
            s.sort();
            eprintln!("\n  Only in Resolvo:");
            for p in s {
                eprintln!("    {}", p);
            }
        }
        if !only_pg.is_empty() {
            let mut s = only_pg;
            s.sort();
            eprintln!("\n  Only in PubGrub:");
            for p in s {
                eprintln!("    {}", p);
            }
        }
    }
}

fn collect_cpns(entries: &[portage_atom::DepEntry], cpns: &mut HashSet<Cpn>) {
    for entry in entries {
        match entry {
            portage_atom::DepEntry::Atom(dep) => {
                cpns.insert(dep.cpn);
            }
            portage_atom::DepEntry::AnyOf(children)
            | portage_atom::DepEntry::ExactlyOneOf(children)
            | portage_atom::DepEntry::AtMostOneOf(children) => {
                collect_cpns(children, cpns);
            }
            portage_atom::DepEntry::UseConditional { children, .. } => {
                collect_cpns(children, cpns);
            }
            portage_atom::DepEntry::AllOf(children) => {
                collect_cpns(children, cpns);
            }
        }
    }
}
