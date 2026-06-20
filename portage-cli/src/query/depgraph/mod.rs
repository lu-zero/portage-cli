mod autounmask;
mod bdepend_trim;
mod depend_trim;
mod effective_use;
mod host_copies;
mod root_aware;

pub use portage_atom_pubgrub::MergeRoot;
#[cfg(test)]
mod c7;
mod conflicts;
mod download_size;
mod force_mask;
mod installed;
mod output;
mod overlay;
mod package_use;
mod repo;
mod required_use;
mod subslot;
mod use_env;

use std::collections::HashMap;

use camino::Utf8Path;
use gentoo_core::Arch;
use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, Dep, Operator, Version};
use portage_atom_pubgrub::{
    DepClass, InstalledPackage as SolverInstalledPackage, InstalledPolicy,
    PortageDependencyProvider, PortagePackage, PortageVersionSet, UseFlagRequirement, UseOverride,
};
use portage_repo::Repository;

use crate::cli::DepgraphFormat;

/// One entry of the resolved merge list, in install order — everything the
/// build loop needs to emerge it.
pub struct PlannedMerge {
    /// Where this package is merged (`BROOT` host vs target `ROOT`).
    pub merge_root: MergeRoot,
    /// `category/name-version` (display + work-dir naming).
    pub cpv: String,
    /// Absolute path to the ebuild.
    pub ebuild_path: camino::Utf8PathBuf,
    /// Effective enabled USE flags for this build: the global config and
    /// per-package overrides resolved per the displayed plan (including
    /// profile-injected implicit flags like `elibc_glibc`/`kernel_linux`,
    /// which USE conditionals test).
    pub use_flags: Vec<Interned<DefaultInterner>>,
    /// `DEPEND` (build-against-sysroot), pre-USE-evaluation, for the pre-flight
    /// build-dependency check (see `preflight`). Empty when no cache entry.
    pub depend: Vec<portage_atom::DepEntry>,
    /// `BDEPEND` (build-host tools), pre-USE-evaluation, for the pre-flight
    /// build-dependency check.
    pub bdepend: Vec<portage_atom::DepEntry>,
}

/// What [`depgraph`] resolved.
pub struct DepgraphOutcome {
    /// Process exit code: `1` when configuration changes are required to
    /// realise the displayed plan (matching `emerge -p`), `0` otherwise.
    pub exit_code: i32,
    /// The merge list in install order.
    pub plan: Vec<PlannedMerge>,
    /// For each `plan` entry, the indices of earlier entries that must finish
    /// building before it can build — its in-plan build-time dependencies
    /// (`DEPEND`/`BDEPEND` edges). Restricted to earlier indices, so it is
    /// always acyclic; the `--jobs` scheduler uses it to parallelise builds
    /// while respecting build order. Empty entry ⇒ no in-plan build deps.
    pub build_blockers: Vec<Vec<usize>>,
}

pub struct DepgraphOpts<'a> {
    pub repo_path: &'a Utf8Path,
    pub atoms: &'a [String],
    pub arch: &'a Arch,
    pub format: DepgraphFormat,
    pub verbose: u8,
    pub empty: bool,
    pub autounmask_write: bool,
    pub autosolve_use: bool,
    /// Load every repo from `repos.conf` (overlays sourced as needed). Off
    /// when the user pinned a repo with `--repo`.
    pub multi_repo: bool,
    /// The resolved root set (config / base / target). See docs/root-model.md.
    pub roots: &'a crate::cli::Roots,
    /// `--onlydeps`: drop the explicitly-requested targets from the plan,
    /// keeping only their dependencies (emerge's `--onlydeps`).
    pub onlydeps: bool,
    /// Include BDEPEND in resolution (emerge's `--with-bdeps`). Default false
    /// (exclude BDEPEND) to match emerge's default.
    pub with_bdeps: bool,
    /// `--deep`: re-examine transitive deps for updates. Used here to bump a
    /// `:*` any-slot dep to the newest slot (like `emerge -uD`) rather than
    /// keeping a satisfying installed slot.
    pub deep: bool,
}

pub async fn depgraph(opts: DepgraphOpts<'_>) -> anyhow::Result<DepgraphOutcome> {
    let DepgraphOpts {
        repo_path,
        atoms,
        arch,
        format,
        verbose,
        empty,
        autounmask_write,
        autosolve_use,
        multi_repo,
        roots,
        onlydeps,
        with_bdeps,
        deep,
    } = opts;
    let cross = root_aware::detect(roots);
    let config_root = roots.config();
    let host_config_stage = cross.active && cross.sysroot.as_str() != cross.target.as_str();
    // Native `emerge -pe`: pretend nothing merged on TARGET, but BROOT still
    // satisfies BDEPEND (emerge sets `bdeps=auto` unless overridden).
    let emptytree_native = empty && !host_config_stage && !cross.active;
    let solve_with_bdeps = with_bdeps || emptytree_native;
    let repo = Repository::open(repo_path)
        .map_err(|e| anyhow::anyhow!("failed to open repo at {repo_path}: {e}"))?;

    // Overlays from repos.conf (the main repo is loaded above). Masters are
    // resolved relative to the main repo's parent directory (e.g. the
    // crossdev overlay's `masters = gentoo` → /var/db/repos/gentoo).
    let overlays: Vec<(Repository, Vec<Repository>)> = if multi_repo {
        let repos_dir = repo
            .path()
            .parent()
            .map(std::path::PathBuf::from)
            .unwrap_or_default();
        match portage_repo::ReposConf::load() {
            Ok(rc) => rc
                .repos()
                .iter()
                .filter(|e| e.location.as_path() != repo.path().as_std_path())
                .filter_map(|e| {
                    match Repository::open_with_masters(e.location.clone(), &repos_dir) {
                        Ok(pair) => Some(pair),
                        Err(err) => {
                            eprintln!(
                                "!!! skipping repo '{}' at {}: {err}",
                                e.name,
                                e.location.display()
                            );
                            None
                        }
                    }
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    } else {
        Vec::new()
    };

    let (data, (target_installed, installed_blockers), host_installed, use_env_result) = tokio::join!(
        repo::load_repos(&repo, &overlays),
        // Also precompute each installed package's blocker atoms on this task
        // (for `check_blockers`): the walk only needs the VDB, so it overlaps the
        // other concurrent loads instead of running serially before the solve.
        async {
            let ti = installed::load_target_installed(roots);
            let blockers: Vec<Vec<Dep>> =
                ti.iter().map(conflicts::installed_blocker_atoms).collect();
            (ti, blockers)
        },
        async { installed::load_host_installed() },
        use_env::build_use_env(&repo, config_root, roots.config_overlay()),
    );
    let use_env = use_env_result?;
    let use_env::UseEnv {
        config: use_config,
        expand: use_expand,
        expand_hidden: use_expand_hidden,
        package_use,
        package_mask,
        package_unmask,
        force_mask,
        accept_keywords,
        package_accept_keywords,
        accept_license,
        package_license,
        distdir,
    } = use_env;

    // Fold global ACCEPT_KEYWORDS and per-package package.accept_keywords into a
    // single interned acceptance decision (precomputed for the host arch).
    let accept_keywords =
        repo::AcceptKeywords::new(arch, &accept_keywords, package_accept_keywords);
    // Likewise fold global ACCEPT_LICENSE with per-package package.license.
    let accept_licenses = repo::AcceptLicenses::new(accept_license, package_license);

    let target_installed_cpvs: std::collections::HashSet<Cpv> = target_installed
        .iter()
        .map(|e| Cpv::new(e.cpn, e.version.clone()))
        .collect();
    // Under `--emptytree` the solver treats target packages as rebuilds (not
    // "already installed" for cede/ingest), while action tags still use the
    // real VDB via `target_installed_cpvs`.
    let empty_solver_cpvs = std::collections::HashSet::new();
    let solver_installed_cpvs: &std::collections::HashSet<Cpv> = if emptytree_native {
        &empty_solver_cpvs
    } else {
        &target_installed_cpvs
    };
    let installed_policy = if emptytree_native {
        InstalledPolicy::Rebuild
    } else {
        InstalledPolicy::Favor
    };

    let mut installed: HashMap<Cpn, HashMap<String, Version>> = HashMap::new();
    for e in &target_installed {
        let slot_key = e.slot.clone().unwrap_or_default();
        installed
            .entry(e.cpn)
            .or_default()
            .insert(slot_key, e.version.clone());
    }

    let mut root_deps = Vec::new();
    let mut root_cpns: std::collections::HashSet<Cpn> = std::collections::HashSet::new();
    for target in atoms {
        let dep = Dep::parse(target).map_err(|e| anyhow::anyhow!("bad atom '{target}': {e}"))?;
        root_cpns.insert(dep.cpn);
        let pkg = repo::target_package(
            &data,
            &dep,
            &accept_keywords,
            &package_mask,
            &package_unmask,
            &accept_licenses,
            &use_config,
            &package_use,
            &force_mask,
        );
        let vs = match &dep.version {
            Some(v) => {
                let op = dep.op.unwrap_or(Operator::GreaterOrEqual);
                PortageVersionSet::from_operator(op, dep.glob, v.clone())
            }
            None => PortageVersionSet::any(),
        };
        if !data.versions.contains_key(&dep.cpn) {
            anyhow::bail!(
                "no ebuilds found for '{target}' (searched ::{}{})",
                data.repo_name,
                if multi_repo { " and overlays" } else { "" },
            );
        }
        root_deps.push((pkg, vs));
    }

    // Build a provider (with the given cede policy) and run the solve. Factored
    // so a failed --autosolve-use attempt can fall back to a fixed-USE (Level A)
    // solve instead of erroring — matching the doc invariant.
    let build_and_solve = |autosolve_use: bool, pkg_use: &[(Dep, Vec<UseOverride>)]| {
        let adapter = repo::Adapter {
            data: &data,
            accept_keywords: &accept_keywords,
            package_mask: &package_mask,
            package_unmask: &package_unmask,
            accept_licenses: &accept_licenses,
            use_config: &use_config,
            package_use: pkg_use,
            force_mask: &force_mask,
            installed_cpvs: solver_installed_cpvs,
            autosolve_use,
        };
        // Closure-seeded ingestion: only packages reachable from the targets
        // and the installed set get converted (a few hundred for a typical
        // resolve), instead of the whole tree — this is what makes the
        // co-solve fixpoint's per-iteration provider rebuild affordable.
        let mut seeds: Vec<Cpn> = root_deps
            .iter()
            .filter(|(pkg, _)| !pkg.is_virtual())
            .map(|(pkg, _)| *pkg.cpn())
            .collect();
        if !emptytree_native {
            seeds.extend(target_installed.iter().map(|e| e.cpn));
        }
        let mut provider =
            PortageDependencyProvider::new_for_targets_with_bdeps(adapter, seeds, solve_with_bdeps);
        provider.set_cross_active(cross.active);
        provider.set_rebuild_tree(emptytree_native);
        // `--deep` and native emptytree bump `:*` deps to the newest slot.
        provider.set_prefer_newest_slot(deep || emptytree_native);
        if cross.active {
            for e in installed::load_sysroot_entries(cross.sysroot.as_path()) {
                let pkg = match e.slot.as_deref().filter(|s| !s.is_empty()) {
                    Some(s) => PortagePackage::slotted(e.cpn, Interned::intern(s)),
                    None => PortagePackage::unslotted(e.cpn),
                };
                provider.add_sysroot_installed(pkg, e.version.clone());
            }
        }
        for (e, blockers) in target_installed.iter().zip(&installed_blockers) {
            let pkg = match e.slot.as_deref().filter(|s| !s.is_empty()) {
                Some(s) => PortagePackage::slotted(e.cpn, Interned::intern(s)),
                None => PortagePackage::unslotted(e.cpn),
            };
            provider.add_installed_blockers(&pkg, blockers);
            provider.add_installed(SolverInstalledPackage {
                package: pkg,
                version: e.version.clone(),
                policy: installed_policy,
                active_use: e.active_use.clone(),
                iuse: e.iuse.clone(),
            });
        }
        // BROOT (the host) provides build tools: a BDEPEND already present there
        // is satisfied without building it into the plan — unless a USE-dep on
        // that edge demands a flag the host lacks, in which case the package is
        // rebuilt (the host entry carries USE/IUSE for that check).
        for e in &host_installed {
            provider.add_host_installed(
                e.package.clone(),
                e.version.clone(),
                e.active_use.clone(),
                e.iuse.clone(),
            );
        }
        let result = provider.resolve_targets(root_deps.clone());
        (provider, result)
    };

    // Auto-apply cross-package `[flag]` USE-deps by forcing the demanded flags
    // on real-IUSE targets via synthetic `package.use` and re-solving to a
    // fixpoint. This mirrors emerge's default *preview* semantics: `emerge -p`
    // computes the graph as if the needed USE changes were applied, prints a
    // mandatory "USE changes are necessary to proceed" block, and exits
    // non-zero. User-pinned flags are never forced. `--autosolve-use`
    // additionally cedes REQUIRED_USE flags to the solver (Level C).
    // The fixpoint hands back the final solve it converged on, so we reuse it
    // instead of solving again; `solved` is `None` when the fixpoint
    // failed/bailed and we must re-solve.
    let pristine_package_use = package_use.clone();
    let (package_use, applied_reqs, solved) = package_use::cosolve_use_deps(
        package_use,
        &data,
        |pu| {
            let (provider, result) = build_and_solve(autosolve_use, pu);
            result.ok().map(|sol| (provider, sol))
        },
        |(provider, _)| provider.use_flag_requirements().to_vec(),
    );

    let (provider, solution) = match solved {
        Some(solved) => solved,
        None => {
            let (provider, result) = build_and_solve(autosolve_use, &package_use);
            match result {
                Ok(sol) => (provider, sol),
                Err(_) if autosolve_use => {
                    // REQUIRED_USE could not be auto-satisfied; fall back to a
                    // fixed-USE solve so the plan + Level-A advisory still appear.
                    eprintln!(
                        "!!! --autosolve-use could not satisfy REQUIRED_USE; \
                         falling back to a fixed-USE plan."
                    );
                    let (provider, result) = build_and_solve(false, &package_use);
                    let sol =
                        result.map_err(|e2| anyhow::anyhow!("resolution failed: {:?}", e2))?;
                    (provider, sol)
                }
                Err(e) => return Err(anyhow::anyhow!("resolution failed: {:?}", e)),
            }
        }
    };

    // Fold per-package profile force/mask and the Level-C ceded flag values back
    // into the effective USE used for display, the REQUIRED_USE check, and
    // autounmask, by appending synthetic `=cpv flag`/`-flag` package.use entries.
    // The force/mask entries surface package.use.force/mask (+ stable variants)
    // in the plan — e.g. crossdev's multilib/cet on cross-* packages — mirroring
    // what `desired_use` already applied for the solver. With no force/mask policy
    // and --autosolve-use off this is a no-op and `package_use` is unchanged
    // (parity preserved).
    let ceded = provider.solved_use_decisions();
    let package_use: Vec<(Dep, Vec<UseOverride>)> = if ceded.is_empty() && force_mask.is_empty() {
        package_use
    } else {
        let mut by_cpn: HashMap<Cpn, Vec<&portage_atom_pubgrub::CededFlag>> = HashMap::new();
        for c in &ceded {
            by_cpn.entry(c.cpn).or_default().push(c);
        }
        let mut combined = package_use;
        for (pkg, ver) in solution.iter() {
            if pkg.is_virtual() {
                continue;
            }
            let atom = format!("={}/{}-{}", pkg.cpn().category, pkg.cpn().package, ver);
            let Ok(dep) = Dep::parse(&atom) else { continue };

            // Profile force/mask for this resolved version (mask rendered as
            // `-flag`; force as `flag`). Stable variants apply only when the
            // version is merged due to a stable keyword.
            if !force_mask.is_empty() {
                let cpv = Cpv::new(*pkg.cpn(), ver.clone());
                let cache = repo::find_cache(&data, pkg, ver);
                let keywords = cache.map(|c| c.metadata.keywords.as_slice()).unwrap_or(&[]);
                let slot = cache.map(|c| c.metadata.slot.slot);
                let stable = accept_keywords.is_stable(keywords, &cpv, slot);
                let (forced, masked) = force_mask.effective(&cpv, stable);
                if !forced.is_empty() || !masked.is_empty() {
                    let mut overrides: Vec<UseOverride> = forced
                        .iter()
                        .map(|&flag| UseOverride { flag, enable: true })
                        .collect();
                    overrides.extend(masked.iter().map(|&flag| UseOverride {
                        flag,
                        enable: false,
                    }));
                    combined.push((dep.clone(), overrides));
                }
            }

            // Level-C: the solver's chosen ceded-flag values (already interned).
            if let Some(flags) = by_cpn.get(pkg.cpn()) {
                let overrides = flags
                    .iter()
                    .map(|c| UseOverride {
                        flag: c.flag,
                        enable: c.value,
                    })
                    .collect();
                combined.push((dep, overrides));
            }
        }
        combined
    };

    if verbose >= 3 {
        output::report_dropped_deps(provider.dropped_deps(), &data, arch.as_str());
    }

    // Autounmask: detect filtered candidates from dropped deps.
    let autounmask_candidates = repo::find_autounmask_candidates(
        &data,
        provider.dropped_deps(),
        &accept_keywords,
        &package_mask,
        &package_unmask,
        &accept_licenses,
        &use_config,
        &package_use,
        &force_mask,
    );

    let root_pkgs: Vec<PortagePackage> = root_deps.iter().map(|(p, _)| p.clone()).collect();

    // A candidate is only actionable if:
    // 1. Its CPN is not already in the solution (an available version satisfies the dep).
    // 2. Its CPN is referenced in the raw dep data of at least one package in the
    //    NEW install plan — deps of already-installed packages were already satisfied
    //    when those packages were built and don't need fixing now.
    let solution_cpns: std::collections::HashSet<Cpn> = solution
        .iter()
        .filter(|(p, _)| !p.is_virtual())
        .map(|(p, _)| *p.cpn())
        .collect();

    // Packages that need a same-version rebuild (USE change) must stay in the
    // merge list even though their installed CPV is unchanged — keep them in
    // their topological position rather than appending them after the target.
    let reinstall_cpns: std::collections::HashSet<Cpn> = provider
        .reinstall_deps()
        .iter()
        .map(|r| *r.package.cpn())
        .collect();

    // When a rebuild is forced on an installed package and a newer version is
    // available, favour the upgrade: build the newest version rather than
    // rebuilding the installed one (matching emerge, and required when the
    // installed version has been removed from the tree — it can't be rebuilt).
    let upgrades: HashMap<Cpn, Version> = provider
        .reinstall_deps()
        .iter()
        .filter_map(|r| r.upgrade_to.as_ref().map(|v| (*r.package.cpn(), v.clone())))
        .collect();

    let mut order: Vec<_> = provider
        .install_order(&solution)
        .into_iter()
        .filter(|(pkg, ver)| {
            if pkg.is_virtual() {
                return false;
            }
            let cpv = Cpv::new(*pkg.cpn(), ver.clone());
            // Drop packages already installed at this version, except:
            //  - same-version USE rebuilds (reinstall_cpns), and
            //  - explicitly-requested targets, which emerge reinstalls by
            //    default ([ebuild R]) even when already at the best version.
            !target_installed_cpvs.contains(&cpv)
                || reinstall_cpns.contains(pkg.cpn())
                // Explicit target: reinstalled even at best version ([R]). Match
                // the resolved target *slot*, not the bare CPN — a sibling slot
                // merely pulled as a satisfied dep (e.g. python:3.13 under a
                // `python` target) must not be re-listed.
                || root_pkgs
                    .iter()
                    .any(|r| r.cpn() == pkg.cpn() && r.slot() == pkg.slot())
                || emptytree_native
        })
        .map(|(pkg, ver)| {
            // Apply the favoured upgrade version if one was recorded.
            let ver = upgrades.get(pkg.cpn()).cloned().unwrap_or(ver);
            (pkg, ver)
        })
        .collect();

    // Fallback: any reinstall the solver didn't route through install_order
    // (rare) is appended so it is not silently dropped.
    {
        let in_order: std::collections::HashSet<Cpn> =
            order.iter().map(|(pkg, _)| *pkg.cpn()).collect();
        let to_reinstall: Vec<(PortagePackage, Version)> = provider
            .reinstall_deps()
            .into_iter()
            .filter(|r| !in_order.contains(r.package.cpn()))
            .map(|r| {
                let ver = r.upgrade_to.as_ref().unwrap_or(&r.version).clone();
                (r.package.clone(), ver)
            })
            .collect();
        order.extend(to_reinstall);
    }

    // Cross-arch host-config stage: pretend output lists target ROOT merges only
    // (emerge -p). A native offset instead keeps the Host build-dep merges (the
    // host-side installs needed to build the target packages), matching emerge.
    if host_config_stage && cross.is_cross_arch() {
        order.retain(|(pkg, _)| pkg.merge_root() == MergeRoot::Target);
    }

    let trim_ctx = bdepend_trim::TrimCtx {
        roots,
        data: &data,
        use_config: &use_config,
        package_use: &package_use,
        root_cpns: &root_cpns,
        reinstall_cpns: &reinstall_cpns,
    };
    if host_config_stage {
        order = depend_trim::trim_sysroot_satisfied_depend(
            order,
            roots.sysroot(),
            cross.target.as_path(),
            &trim_ctx,
        );
    }

    if !emptytree_native {
        // Built packages always carry their BDEPEND now (it's required to build
        // them), so always run the within-run trim to drop entries only needed
        // for BDEPEND already satisfied on BROOT or by an earlier kept entry —
        // matching emerge, which trims a built package's redundant build tools
        // regardless of `--with-bdeps`.
        order = bdepend_trim::trim_within_run_bdepend(order, true, &trim_ctx);
    }
    // Native --emptytree lists the full deep closure straight from the solve
    // (the provider returns un-pruned deps under `rebuild_tree`); no post-solve
    // re-list. See todo/em-emptytree.md "AGREED REDESIGN".

    let edges: Vec<_> = provider
        .dependency_graph(&solution)
        .into_iter()
        .filter(|e| !e.from.0.is_virtual() && !e.to.0.is_virtual())
        .collect();

    // Emerge convention: list the explicitly-requested target(s) last.  Only
    // move a target that nothing else depends on (not a `to` in any edge), so
    // the order stays topologically valid for `em -p A B` where one target is a
    // dependency of another.
    {
        let depended_upon: std::collections::HashSet<Cpn> =
            edges.iter().map(|e| *e.to.0.cpn()).collect();
        let (targets, rest): (Vec<_>, Vec<_>) = order.into_iter().partition(|(pkg, _)| {
            root_cpns.contains(pkg.cpn()) && !depended_upon.contains(pkg.cpn())
        });
        order = rest;
        order.extend(targets);
    }

    // `--onlydeps`: build only the dependencies of the requested targets, not
    // the targets themselves. Drop them from the install order before the plan
    // is displayed and built, so the table, merge list, and `build_blockers`
    // indices all agree (emerge's `--onlydeps`).
    if onlydeps {
        order.retain(|(pkg, _)| !root_cpns.contains(pkg.cpn()));
    }

    // Slot-operator (`:=`) rebuilds: installed consumers whose VDB-recorded
    // subslot binding is invalidated by a planned dependency are pulled into
    // the plan as same-version rebuilds, placed right after their trigger
    // (emerge's __auto_slot_operator_replace_installed__ set). Both ends carry
    // the `r` (forced rebuild) marker in the output.
    let mut slot_op_cpns: std::collections::HashSet<Cpn> = Default::default();
    if !empty {
        let mut planned_slots: HashMap<Cpn, Vec<(Version, portage_atom::Slot)>> = HashMap::new();
        for (pkg, ver) in &order {
            if let Some(cache) = repo::find_cache(&data, pkg, ver) {
                planned_slots
                    .entry(*pkg.cpn())
                    .or_default()
                    .push((ver.clone(), cache.metadata.slot));
            }
        }
        let in_plan: std::collections::HashSet<Cpn> =
            order.iter().map(|(pkg, _)| *pkg.cpn()).collect();
        for rb in subslot::find_rebuilds(&target_installed, &planned_slots, &in_plan) {
            let pos = order
                .iter()
                .rposition(|(pkg, _)| rb.triggers.contains(pkg.cpn()))
                .map_or(order.len(), |i| i + 1);
            let pkg = match rb.slot.as_deref().filter(|s| !s.is_empty()) {
                Some(s) => PortagePackage::slotted(rb.cpn, Interned::intern(s)),
                None => PortagePackage::unslotted(rb.cpn),
            };
            order.insert(pos, (pkg, rb.version.clone()));
            slot_op_cpns.insert(rb.cpn);
            slot_op_cpns.extend(rb.triggers.iter().copied());
        }
    }

    // Native offset (same-arch `--root`/`--prefix`): schedule host build-copies
    // — a target package's build edges (`DEPEND`/`BDEPEND`/`IDEPEND`) the host
    // lacks are merged to BROOT (`/`) so the target can build against them
    // (emerge lists these `to /` alongside the ROOT runtime copy). Computed as a
    // post-solve walk over the finalized Target plan, not in the solver, to keep
    // the Target solve pristine (the dual-root aliasing balloons it otherwise).
    let host_copies = host_copies::compute(&order, &data, &use_config, &package_use, &cross);
    if !host_copies.is_empty() {
        order.splice(0..0, host_copies);
    }

    let flag_reqs: HashMap<&PortagePackage, &UseFlagRequirement> = provider
        .use_flag_requirements()
        .iter()
        .map(|r| (&r.package, r))
        .collect();

    let portage_dir = config_root
        .unwrap_or(camino::Utf8Path::new("/"))
        .join("etc/portage");

    // CPNs referenced in the raw dep data of newly-installed packages.
    let new_needed_cpns: std::collections::HashSet<Cpn> = order
        .iter()
        .filter(|(pkg, _)| !pkg.is_virtual())
        .flat_map(|(pkg, ver)| repo::cpns_for(&data, pkg.cpn(), ver))
        .collect();

    let autounmask_candidates: Vec<_> = autounmask_candidates
        .into_iter()
        .filter(|c| !solution_cpns.contains(&c.cpv.cpn) && new_needed_cpns.contains(&c.cpv.cpn))
        .collect();

    // A required dependency was filtered out of *every* version (keyword / mask
    // / license) and had no `||` alternative, so the solver dropped it and the
    // printed plan is silently incomplete. Surface these unconditionally — like
    // emerge, an unsatisfiable requirement must never be hidden, regardless of
    // `--autounmask`. The flag now only governs *writing* the fix:
    // `--autounmask-write` persists the keyword/mask/license changes.
    // Report in order of severity: mask → keywords → license.
    if !autounmask_candidates.is_empty() {
        autounmask::report(&autounmask_candidates);
        if autounmask_write {
            autounmask::write(&autounmask_candidates, &portage_dir)?;
        }
    }

    // emerge preview semantics: the plan was computed as if the needed USE
    // changes were applied (the co-solve fixpoint), so the changes the user
    // must make are mandatory output — `applied_reqs` (satisfied in the final
    // solve only because they were forced) plus any leftover unapplied demands
    // — judged against the *pristine* configuration. Reported after the merge
    // list (emerge puts caveats at the bottom); like emerge, the run exits
    // non-zero when changes are required.
    let use_change_entries = {
        let mut combined: Vec<_> = applied_reqs;
        combined.extend(provider.use_flag_requirements().to_vec());
        let entries = package_use::build_entries(
            &combined,
            atoms,
            &edges,
            &use_config,
            &pristine_package_use,
        );
        if autounmask_write && !entries.is_empty() {
            package_use::write(&entries, &portage_dir.join("package.use"))?;
        }
        entries
    };

    let _display_adapter = repo::Adapter {
        data: &data,
        accept_keywords: &accept_keywords,
        package_mask: &package_mask,
        package_unmask: &package_unmask,
        accept_licenses: &accept_licenses,
        use_config: &use_config,
        package_use: &package_use,
        force_mask: &force_mask,
        installed_cpvs: solver_installed_cpvs,
        autosolve_use: false,
    };
    let plan_entries = root_aware::build_plan(order.clone());

    match format {
        DepgraphFormat::Pretty => {
            // Verbose mode shows per-package download size and a total; skip the
            // Manifest/DISTDIR work entirely in plain mode.
            let sizes = if verbose >= 1 {
                download_size::compute(
                    repo_path,
                    &distdir,
                    &data,
                    &order,
                    &use_config,
                    &package_use,
                )
            } else {
                HashMap::new()
            };
            output::print_pretty_rooted(
                &data,
                &plan_entries,
                &installed,
                &target_installed,
                &use_config,
                &package_use,
                &use_expand,
                &use_expand_hidden,
                &flag_reqs,
                &sizes,
                &slot_op_cpns,
                verbose,
                &cross,
            )
        }
        DepgraphFormat::Json => output::print_json(&data, &order, &edges, &installed, &flag_reqs),
        DepgraphFormat::Tree => {
            let roots: Vec<_> = root_pkgs
                .iter()
                .filter_map(|pkg| {
                    let ver = edges
                        .iter()
                        .find_map(|e| {
                            if &e.from.0 == pkg {
                                Some(e.from.1.clone())
                            } else if &e.to.0 == pkg {
                                Some(e.to.1.clone())
                            } else {
                                None
                            }
                        })
                        .or_else(|| order.iter().find(|(p, _)| p == pkg).map(|(_, v)| v.clone()));
                    ver.map(|v| (pkg.clone(), v))
                })
                .collect();
            output::print_tree(&roots, &edges, &target_installed_cpvs)
        }
    }

    // Advisory warnings are emitted after the plan so the merge list reads
    // first and the caveats follow it (emerge lists issues at the bottom too).
    // These are non-fatal: the plan is still produced.
    //
    //  - reverse-dependency constraints: a complete-graph check that emerge's
    //    default targeted `-p` skips (e.g. upgrading docutils past an installed
    //    package's `<` bound);
    //  - blockers (`!foo` / `!!foo`) and `::repo` constraints, which the solver
    //    does not model;
    //  - REQUIRED_USE, evaluated per-package against its effective USE.
    {
        let proposed: Vec<conflicts::ProposedPkg> = order
            .iter()
            .filter(|(pkg, _)| !pkg.is_virtual())
            .map(|(pkg, ver)| conflicts::ProposedPkg {
                cpn: *pkg.cpn(),
                slot: pkg.slot(),
                version: ver.clone(),
            })
            .collect();
        let dep_conflicts = conflicts::find_conflicts(&target_installed, &proposed);
        if !dep_conflicts.is_empty() {
            output::report_conflicts(&dep_conflicts);
        }

        let mut violations = provider.check_blockers(&solution);
        violations.extend(provider.check_repo_constraints(&solution));
        if !violations.is_empty() {
            output::report_solver_violations(&violations);
        }

        let ru_violations = required_use::find_violations(&data, &order, &use_config, &package_use);
        if !ru_violations.is_empty() {
            output::report_required_use(&ru_violations);
        }

        // Level-C: report the flags the solver flipped from their configured
        // value to satisfy REQUIRED_USE (they appear set in the plan via the
        // synthetic package.use above; this tells the user what changed).
        let flips: Vec<&portage_atom_pubgrub::CededFlag> =
            ceded.iter().filter(|c| c.flipped).collect();
        if !flips.is_empty() {
            output::report_autosolved_use(&flips, solution.iter(), &data);
        }

        // C5 advisory: a UseDecision is keyed per (cpn, flag), so when several
        // slots of one package are in the plan the same value bound all of them.
        let shared = output::shared_slot_decisions(&ceded, solution.iter());
        if !shared.is_empty() {
            output::report_shared_slot_use_decisions(&shared);
        }

        package_use::report(&use_change_entries);
    }

    // The merge plan for the build loop: ebuild paths come from the package's
    // source repo (main or overlay), USE from the same effective fold the
    // displayed plan used.
    let repo_path_of = |cpv: &Cpv| -> camino::Utf8PathBuf {
        let name = repo::repo_name_of(&data, cpv);
        if name == data.repo_name {
            repo_path.to_owned()
        } else {
            overlays
                .iter()
                .find(|(o, _)| o.name() == name)
                .map(|(o, _)| o.path().to_owned())
                .unwrap_or_else(|| repo_path.to_owned())
        }
    };
    let plan: Vec<PlannedMerge> = plan_entries
        .iter()
        .filter(|e| !e.pkg.is_virtual())
        .map(|entry| {
            let pkg = &entry.pkg;
            let ver = &entry.version;
            let cpn = pkg.cpn();
            let cpv = Cpv::new(*cpn, ver.clone());
            let (depend, bdepend, mut flags) =
                if let Some(cache) = repo::find_cache(&data, pkg, ver) {
                    let effective =
                        effective_use::effective_use(&use_config, &package_use, pkg, ver, cache);
                    (
                        cache.metadata.depend.clone(),
                        cache.metadata.bdepend.clone(),
                        effective.enabled_flags(),
                    )
                } else {
                    let effective = portage_atom_pubgrub::apply_package_use(
                        &use_config,
                        &cpv,
                        pkg.slot(),
                        &package_use,
                    );
                    (Vec::new(), Vec::new(), effective.enabled_flags())
                };
            flags.sort();
            flags.dedup();
            let ebuild_path = repo_path_of(&cpv)
                .join(cpn.category.as_str())
                .join(cpn.package.as_str())
                .join(format!("{}-{}.ebuild", cpn.package, ver));
            PlannedMerge {
                merge_root: entry.merge_root,
                cpv: format!("{}/{}-{}", cpn.category, cpn.package, ver),
                ebuild_path,
                use_flags: flags,
                depend,
                bdepend,
            }
        })
        .collect();

    // Build-order adjacency for `--jobs`: for each plan entry, the indices of
    // *earlier* entries it depends on at build time (DEPEND/BDEPEND). Matching
    // is by CPN (an upgrade may remap the version), restricted to earlier
    // indices so the relation is acyclic — `install_order` already linearised
    // any cycle. A spurious blocker only costs parallelism; a missing one would
    // risk building before a dep is merged, so CPN matching errs on the safe
    // (more-blocking) side.
    let index_of: HashMap<(MergeRoot, Cpn), usize> = plan
        .iter()
        .enumerate()
        .filter_map(|(i, p)| {
            Cpv::parse(&p.cpv)
                .ok()
                .map(|cpv| ((p.merge_root, cpv.cpn), i))
        })
        .collect();
    let mut build_blockers: Vec<Vec<usize>> = vec![Vec::new(); plan.len()];
    for e in &edges {
        if !matches!(e.class, DepClass::Depend | DepClass::Bdepend) {
            continue;
        }
        let from_key = (e.from.0.merge_root(), *e.from.0.cpn());
        let to_key = (e.to.0.merge_root(), *e.to.0.cpn());
        let (Some(&from), Some(&to)) = (index_of.get(&from_key), index_of.get(&to_key)) else {
            continue;
        };
        if to < from && !build_blockers[from].contains(&to) {
            build_blockers[from].push(to);
        }
    }

    Ok(DepgraphOutcome {
        // Non-zero when the displayed plan needs config changes to be realised:
        // either USE changes (co-solve fixpoint) or unmask/keyword/license
        // changes for a required dep the solver had to drop. Either way the plan
        // as printed is not directly installable — emerge exits non-zero too.
        exit_code: if use_change_entries.is_empty() && autounmask_candidates.is_empty() {
            0
        } else {
            1
        },
        plan,
        build_blockers,
    })
}
