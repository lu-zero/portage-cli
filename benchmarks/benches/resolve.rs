use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

#[cfg(all(feature = "mimalloc", not(feature = "dhat-heap")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static DHAT_ALLOC: dhat::Alloc = dhat::Alloc;

use portage_atom::interner::Interned;
use portage_atom::{Cpn, Cpv, Dep, Operator, Slot, SlotDep, SlotOperator};
use portage_atom_pubgrub::{
    IUseDefault, InMemoryRepository, InstalledPackage, InstalledPolicy, PackageDeps,
    PackageVersions, PortageDependencyProvider, PortagePackage, PortageVersionSet, UseConfig,
};
use portage_metadata::{Keyword, Stability};
use portage_repo::{ProfileStack, Repository};

const DEFAULT_REPO: &str = "/var/db/repos/gentoo";
const DEFAULT_PROFILE: &str = "/etc/portage/make.profile";
const DEFAULT_MAKE_CONF: &str = "/etc/portage/make.conf";
const DEFAULT_VDB: &str = "/var/db/pkg";

const TARGETS: &[(&str, &str)] = &[
    ("firefox", "www-client/firefox"),
    ("gcc", "sys-devel/gcc"),
    ("rust", "dev-lang/rust"),
    ("openssh", "net-misc/openssh"),
    ("python", "dev-lang/python"),
];

// ---------------------------------------------------------------------------
// System configuration
// ---------------------------------------------------------------------------

/// Gentoo system configuration loaded from the active profile and make.conf.
struct SystemConfig {
    /// Accepted keyword tokens, e.g. `["arm64", "~arm64"]`.
    accept_keywords: Vec<String>,
    /// Effective USE flags after full profile + make.conf evaluation.
    effective_use: Vec<String>,
    /// Atoms masked by the profile stack (package.mask).
    package_mask: Vec<Dep>,
    /// Per-package USE overrides (profile package.use + /etc/portage/package.use).
    package_use: Vec<(Dep, Vec<String>)>,
}

impl SystemConfig {
    fn load(repo_path: &str) -> Self {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        rt.block_on(async { Self::load_async(repo_path).await })
    }

    async fn load_async(repo_path: &str) -> Self {
        let repo = Repository::builder()
            .in_memory_cache()
            .open(repo_path)
            .expect("failed to open repo");

        let profile_path = resolve_profile_symlink(DEFAULT_PROFILE);

        let stack = match ProfileStack::build(profile_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("warn: could not load profile: {e}");
                return Self::unconfigured();
            }
        };

        let package_mask = stack.package_mask().unwrap_or_default();

        // Per-package USE: profile stack entries first, then /etc/portage/package.use.
        let mut package_use = stack.package_use().unwrap_or_default();
        if let Ok(user_pkg_use) = parse_package_use_dir("/etc/portage/package.use") {
            package_use.extend(user_pkg_use);
        }

        // Source make.defaults chain + make.conf through a real shell,
        // then apply use.force / use.mask — identical to portage's logic.
        let mut shell = match repo.shell().await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("warn: could not start shell: {e}");
                return Self::unconfigured();
            }
        };
        let make_conf = std::path::Path::new(DEFAULT_MAKE_CONF);
        let extra = if make_conf.exists() {
            vec![make_conf]
        } else {
            vec![]
        };
        if let Err(e) = stack.configure_shell(&mut shell, &extra).await {
            eprintln!("warn: configure_shell failed: {e}");
        }

        // configure_shell accumulates USE by sequential bash sourcing, which
        // doesn't replicate portage's per-profile delta accumulation correctly
        // (child make.defaults that set USE without ${USE} overwrite parent
        // flags).  Fall back to portageq for the authoritative values.
        let accept_keywords = portageq_var("ACCEPT_KEYWORDS")
            .unwrap_or_else(|| shell.get_var("ACCEPT_KEYWORDS").unwrap_or_default())
            .split_whitespace()
            .map(String::from)
            .collect();

        let effective_use = portageq_var("USE")
            .unwrap_or_else(|| shell.get_var("USE").unwrap_or_default())
            .split_whitespace()
            .map(String::from)
            .filter(|f| !f.starts_with('-'))
            .collect();

        SystemConfig {
            accept_keywords,
            effective_use,
            package_mask,
            package_use,
        }
    }

    fn unconfigured() -> Self {
        SystemConfig {
            accept_keywords: vec![],
            effective_use: vec![],
            package_mask: vec![],
            package_use: vec![],
        }
    }

    /// Build a `UseConfig` from the effective USE flag list.
    fn use_config(&self) -> UseConfig {
        let mut cfg = UseConfig::new();
        for flag in &self.effective_use {
            cfg.enable(Interned::intern(flag.as_str()));
        }
        cfg
    }

    /// Return true if the version's KEYWORDS are accepted under `ACCEPT_KEYWORDS`.
    fn keyword_accepted(&self, keywords: &[Keyword]) -> bool {
        if self.accept_keywords.is_empty() {
            return true;
        }
        for kw in keywords {
            let arch = kw.arch.as_str();
            let accepted = match kw.stability {
                Stability::Stable => self
                    .accept_keywords
                    .iter()
                    .any(|a| a == arch || a == &format!("~{arch}")),
                Stability::Testing => self
                    .accept_keywords
                    .iter()
                    .any(|a| a == &format!("~{arch}")),
                Stability::Disabled | Stability::DisabledAll => false,
            };
            if accepted {
                return true;
            }
        }
        false
    }

    /// Return true if the CPV (with its slot) matches any package.mask atom.
    fn is_masked(&self, cpv: &Cpv, slot: &Slot) -> bool {
        self.package_mask
            .iter()
            .any(|dep| dep_matches_cpv(dep, cpv, slot))
    }
}

/// Query `portageq envvar <VAR>` for the authoritative portage value of a
/// variable.  Returns `None` if portageq is not installed or fails.
fn portageq_var(var: &str) -> Option<String> {
    let out = std::process::Command::new("portageq")
        .args(["envvar", var])
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

/// Resolve `/etc/portage/make.profile` to an absolute path, following
/// relative symlinks relative to the symlink's parent directory.
fn resolve_profile_symlink(link: &str) -> PathBuf {
    match std::fs::read_link(link) {
        Ok(target) if target.is_absolute() => target,
        Ok(target) => {
            let base = PathBuf::from(link)
                .parent()
                .unwrap_or(std::path::Path::new("/"))
                .to_path_buf();
            base.join(target)
        }
        Err(_) => PathBuf::from(link),
    }
}

/// Check whether a `Dep` atom matches a given `Cpv` and slot (used for package.mask).
///
/// Slot-constrained atoms like `virtual/libcrypt:0/1` only match packages with
/// that specific slot (and subslot, if specified).  A bare CPN without a slot
/// spec matches all versions.
fn dep_matches_cpv(dep: &Dep, cpv: &Cpv, pkg_slot: &Slot) -> bool {
    if dep.cpn != cpv.cpn {
        return false;
    }

    // Check slot constraint if the mask atom has one.
    if let Some(slot_dep) = &dep.slot_dep {
        match slot_dep {
            SlotDep::Slot { slot: Some(s), .. } => {
                if pkg_slot.slot.as_str() != s.slot.as_str() {
                    return false;
                }
                if let Some(sub) = s.subslot {
                    match pkg_slot.subslot {
                        Some(pkg_sub) if pkg_sub.as_str() == sub.as_str() => {}
                        _ => return false,
                    }
                }
            }
            SlotDep::Slot { slot: None, .. } | SlotDep::Operator(SlotOperator::Star) => {
                // bare :* or :op with no explicit slot — matches all slots
            }
            SlotDep::Operator(_) => {
                // bare := without slot name — matches all slots
            }
        }
    }

    match (dep.op, &dep.version) {
        (None, None) => true, // bare CPN (or slot-filtered CPN) matches
        (Some(op), Some(ver)) => {
            let cand = &cpv.version;
            match op {
                Operator::GreaterOrEqual => cand >= ver,
                Operator::Greater => cand > ver,
                Operator::LessOrEqual => cand <= ver,
                Operator::Less => cand < ver,
                Operator::Equal => {
                    if dep.glob {
                        cand.to_string().starts_with(&ver.to_string())
                    } else {
                        cand == ver
                    }
                }
                Operator::Approximate => {
                    let mut bc = cand.clone();
                    bc.revision = portage_atom::Revision::default();
                    let mut bv = ver.clone();
                    bv.revision = portage_atom::Revision::default();
                    bc == bv
                }
            }
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Repo loading
// ---------------------------------------------------------------------------

fn load_repo(repo_path: &str, sys: &SystemConfig) -> InMemoryRepository {
    let repo = Repository::builder()
        .in_memory_cache()
        .open(repo_path)
        .expect("failed to open repo");
    let repo_name = repo.name();
    let mut out = InMemoryRepository::new();

    for cat in repo.categories() {
        for pkg in cat.packages() {
            for ebuild in pkg.ebuilds().expect("failed to read ebuilds") {
                let cpv = ebuild.cpv().clone();
                let Ok(Some(cache)) = repo.cache_entry(&cpv) else {
                    continue;
                };
                let meta = &cache.metadata;

                // Skip versions not accepted by ACCEPT_KEYWORDS.
                if !sys.keyword_accepted(&meta.keywords) {
                    continue;
                }
                // Skip versions masked by the profile.
                if sys.is_masked(&cpv, &meta.slot) {
                    continue;
                }

                let iuse: Vec<Interned<portage_atom::interner::DefaultInterner>> = meta
                    .iuse
                    .iter()
                    .map(|i| Interned::intern(i.name()))
                    .collect();

                let iuse_defaults: HashMap<
                    Interned<portage_atom::interner::DefaultInterner>,
                    IUseDefault,
                > = meta
                    .iuse
                    .iter()
                    .filter_map(|i| {
                        let default = match i.default {
                            Some(portage_metadata::IUseDefault::Enabled) => IUseDefault::Enabled,
                            Some(portage_metadata::IUseDefault::Disabled) => IUseDefault::Disabled,
                            None => return None,
                        };
                        Some((Interned::intern(i.name()), default))
                    })
                    .collect();

                let deps = PackageDeps::new(
                    meta.depend.clone(),
                    meta.rdepend.clone(),
                    meta.bdepend.clone(),
                    meta.pdepend.clone(),
                    meta.idepend.clone(),
                );

                out.add_package_versions(
                    cpv,
                    PackageVersions {
                        slot: Some(meta.slot.slot),
                        subslot: meta.slot.subslot,
                        repo: Some(repo_name),
                        iuse,
                        iuse_defaults,
                        deps,
                        // REQUIRED_USE is a dormant Level-C fact; the benchmark
                        // adapter does not feed it.
                        required_use: None,
                        empty_any_of_matches: meta.eapi.empty_any_of_matches(),
                    },
                );
            }
        }
    }

    out
}

fn build_provider(
    mut repo: InMemoryRepository,
    use_cfg: UseConfig,
    _package_use: &[(Dep, Vec<String>)],
) -> PortageDependencyProvider {
    // USE policy now travels through the repository's desired_use; the in-memory
    // repo applies global USE + IUSE defaults (package.use isn't modelled here).
    repo.set_use_config(use_cfg);
    PortageDependencyProvider::new(repo)
}

/// Load installed packages from the Gentoo VDB at `vdb_path` (default: /var/db/pkg).
/// Each installed package is registered with `InstalledPolicy::Favor` so the solver
/// prefers installed alternatives in OR groups without pinning versions.
fn load_installed(vdb_path: &str) -> Vec<InstalledPackage> {
    let vdb = std::path::Path::new(vdb_path);
    if !vdb.exists() {
        return vec![];
    }
    let mut result = Vec::new();
    let Ok(categories) = std::fs::read_dir(vdb) else {
        return result;
    };
    for cat_entry in categories.flatten() {
        let cat_path = cat_entry.path();
        if !cat_path.is_dir() {
            continue;
        }
        let category = cat_entry.file_name().to_string_lossy().to_string();
        if category.starts_with('.') {
            continue;
        }
        let Ok(pkgs) = std::fs::read_dir(&cat_path) else {
            continue;
        };
        for pkg_entry in pkgs.flatten() {
            let pkg_path = pkg_entry.path();
            if !pkg_path.is_dir() {
                continue;
            }
            let name_ver = pkg_entry.file_name().to_string_lossy().to_string();
            let cpv_str = format!("{category}/{name_ver}");
            let Ok(cpv) = Cpv::parse(&cpv_str) else {
                continue;
            };
            let slot_str = std::fs::read_to_string(pkg_path.join("SLOT")).unwrap_or_default();
            let slot_part = slot_str.trim().split('/').next().unwrap_or("0");
            let slot = Interned::intern(slot_part);
            result.push(InstalledPackage {
                package: PortagePackage::slotted(cpv.cpn, slot),
                version: cpv.version,
                policy: InstalledPolicy::Favor,
                active_use: vec![],
                iuse: vec![],
            });
        }
    }
    result
}

/// Parse a `package.use` file or directory into `(Dep, flags)` pairs.
fn parse_package_use_dir(path: &str) -> Result<Vec<(Dep, Vec<String>)>, std::io::Error> {
    use portage_atom::Dep as PDep;
    let p = std::path::Path::new(path);
    if !p.exists() {
        return Ok(vec![]);
    }
    let mut result = Vec::new();
    let entries: Vec<_> = if p.is_dir() {
        let mut v: Vec<_> = std::fs::read_dir(p)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file())
            .collect();
        v.sort();
        v
    } else {
        vec![p.to_path_buf()]
    };
    for entry in entries {
        let content = std::fs::read_to_string(&entry)?;
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let mut parts = line.split_whitespace();
            let Some(atom_str) = parts.next() else {
                continue;
            };
            let Ok(dep) = PDep::parse(atom_str) else {
                continue;
            };
            let flags: Vec<String> = parts.map(String::from).collect();
            if !flags.is_empty() {
                result.push((dep, flags));
            }
        }
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// Benchmarks
// ---------------------------------------------------------------------------

fn bench_load(c: &mut Criterion) {
    let repo_path = std::env::var("GENTOO_REPO").unwrap_or_else(|_| DEFAULT_REPO.to_string());
    if !std::path::Path::new(&repo_path).exists() {
        eprintln!("skipping resolve benchmarks: {repo_path} not found");
        return;
    }

    let sys = SystemConfig::load(&repo_path);

    let mut group = c.benchmark_group("resolve/load");
    group.sample_size(10);

    group.bench_function("load_repo", |b| {
        b.iter(|| std::hint::black_box(load_repo(&repo_path, &sys)))
    });

    group.bench_function("build_provider", |b| {
        b.iter_with_setup(
            || load_repo(&repo_path, &sys),
            |repo| std::hint::black_box(build_provider(repo, sys.use_config(), &sys.package_use)),
        )
    });

    group.finish();
}

fn bench_resolve(c: &mut Criterion) {
    let repo_path = std::env::var("GENTOO_REPO").unwrap_or_else(|_| DEFAULT_REPO.to_string());
    if !std::path::Path::new(&repo_path).exists() {
        return;
    }

    let sys = SystemConfig::load(&repo_path);

    let t0 = Instant::now();
    let base_repo = load_repo(&repo_path, &sys);
    eprintln!("repo loaded in {:.2?}", t0.elapsed());

    let mut group = c.benchmark_group("resolve/targets");
    group.sample_size(10);

    // One provider per target, built outside the iter loop. resolve_targets
    // is idempotent w.r.t. provider state (the synthetic root it inserts is
    // removed before the function returns), so reusing the provider across
    // iterations is safe and avoids billing setup time to the measurement.
    // See docs/profiling.md for why iter_with_setup confused earlier profiles.
    for (label, atom) in TARGETS {
        let cpn = match Cpn::parse(atom) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("skip {atom}: {e}");
                continue;
            }
        };

        let mut provider = build_provider(base_repo.clone(), sys.use_config(), &sys.package_use);
        let pkgs = provider.packages_for_cpn(&cpn);
        if pkgs.is_empty() {
            eprintln!("skip {atom}: not found in provider");
            continue;
        }
        let targets: Vec<_> = pkgs
            .into_iter()
            .map(|p| (p, PortageVersionSet::any()))
            .collect();

        group.bench_with_input(
            BenchmarkId::new("portage-atom-pubgrub", label),
            label,
            |b, _| b.iter(|| std::hint::black_box(provider.resolve_targets(targets.clone()).ok())),
        );
    }

    group.finish();
}

fn bench_solution_size(c: &mut Criterion) {
    let repo_path = std::env::var("GENTOO_REPO").unwrap_or_else(|_| DEFAULT_REPO.to_string());
    if !std::path::Path::new(&repo_path).exists() {
        return;
    }

    let sys = SystemConfig::load(&repo_path);
    eprintln!(
        "ACCEPT_KEYWORDS: {:?}  effective_use: {}  package_mask: {} atoms  package_use: {} entries",
        sys.accept_keywords,
        sys.effective_use.len(),
        sys.package_mask.len(),
        sys.package_use.len(),
    );
    let has_pam = sys.effective_use.iter().any(|f| f == "pam");
    let has_ssl = sys.effective_use.iter().any(|f| f == "ssl");
    let has_static = sys.effective_use.iter().any(|f| f == "static");
    eprintln!("  USE contains: pam={has_pam} ssl={has_ssl} static={has_static}");

    let vdb_path = std::env::var("GENTOO_VDB").unwrap_or_else(|_| DEFAULT_VDB.to_string());
    let installed = load_installed(&vdb_path);
    eprintln!("  installed packages from VDB: {}", installed.len());

    let base_repo = load_repo(&repo_path, &sys);
    let probe = build_provider(base_repo.clone(), sys.use_config(), &sys.package_use);

    println!("\n=== Solution sizes (keyword+mask filtered, profile USE) ===");
    for (label, atom) in TARGETS {
        let cpn = Cpn::parse(atom).unwrap();
        let pkgs = probe.packages_for_cpn(&cpn);
        if pkgs.is_empty() {
            println!("  {label:<12} → not found in provider");
            continue;
        }
        let targets: Vec<_> = pkgs
            .into_iter()
            .map(|p| (p, PortageVersionSet::any()))
            .collect();
        let mut p = build_provider(base_repo.clone(), sys.use_config(), &sys.package_use);
        for inst in &installed {
            p.add_installed(inst.clone());
        }
        if *label == "openssh" {
            let dropped = p.dropped_deps();
            eprintln!("  {label}: {} deps dropped total", dropped.len());
            // Count drops per package name and show top 20
            let mut counts: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            for d in dropped {
                if !d.package.is_virtual() {
                    *counts.entry(d.package.cpn().to_string()).or_default() += 1;
                }
            }
            let mut counts_vec: Vec<_> = counts.into_iter().collect();
            counts_vec.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
            for (name, count) in counts_vec.iter().take(20) {
                eprintln!("    dropped {name}: {count}x");
            }

            // Inspect openssh's actual deps to trace pam/libcrypt
            let openssh_cpn = Cpn::parse("net-misc/openssh").unwrap();
            for openssh_pkg in p.packages_for_cpn(&openssh_cpn) {
                let mut vers = p.versions_for_pkg(&openssh_pkg);
                vers.sort();
                if let Some(latest) = vers.last() {
                    let deps = p.deps_for(&openssh_pkg, latest).unwrap_or_default();
                    let has_pam = deps
                        .iter()
                        .any(|(d, _)| !d.is_virtual() && d.cpn().package.as_str().contains("pam"));
                    let has_libcrypt = deps.iter().any(|(d, _)| {
                        !d.is_virtual() && d.cpn().package.as_str().contains("libcrypt")
                    });
                    let has_openssl = deps.iter().any(|(d, _)| {
                        !d.is_virtual() && d.cpn().package.as_str().contains("openssl")
                    });
                    eprintln!(
                        "  openssh {latest}: {} merged deps | pam={has_pam} libcrypt={has_libcrypt} openssl={has_openssl}",
                        deps.len()
                    );
                    for (d, _) in deps.iter().filter(|(d, _)| {
                        !d.is_virtual()
                            && (d.cpn().package.as_str().contains("pam")
                                || d.cpn().package.as_str().contains("libcrypt"))
                    }) {
                        eprintln!("    dep: {d}");
                    }
                }
            }
            // Check pam is in provider at all
            let pam_cpn = Cpn::parse("sys-libs/pam").unwrap();
            let pam_pkgs = p.packages_for_cpn(&pam_cpn);
            eprintln!("  pam packages in provider: {pam_pkgs:?}");

            // Check virtual/libcrypt is in provider
            let libcrypt_cpn = Cpn::parse("virtual/libcrypt").unwrap();
            let libcrypt_pkgs = p.packages_for_cpn(&libcrypt_cpn);
            eprintln!("  virtual/libcrypt packages in provider: {libcrypt_pkgs:?}");
        }
        match p.resolve_targets(targets) {
            Ok(sol) => {
                println!("  {label:<12} → {} packages", sol.iter().count());
                let out_path = format!("/tmp/solver_{label}.txt");
                let mut names: Vec<String> = sol.iter().map(|(p, _)| p.cpn().to_string()).collect();
                names.sort();
                names.dedup();
                std::fs::write(&out_path, names.join("\n") + "\n").ok();
                eprintln!("  wrote {out_path}");
            }
            Err(e) => println!("  {label:<12} → FAILED: {e:?}"),
        }
    }
    drop(probe);

    let _ = c;
}

criterion_group!(benches, bench_load, bench_resolve, bench_solution_size);
criterion_main!(benches);
