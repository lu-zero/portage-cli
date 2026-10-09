//! Load and classify everything [`super::depgraph`] needs before it solves.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, Dep, Version};
use portage_atom_pubgrub::{
    MergeRoot, PortagePackage, PortageVersionSet, SlotMap, UseOverride, build_slot_map,
};

use portage_resolve::{conflicts, installed, repo, root_aware, use_env};

use super::{
    DepgraphOpts, ResolveMetrics, RootTargets, classify_root_targets, package_needs_use_reinstall,
    provided_availability,
};

/// Owned inputs for one [`super::depgraph`] call.
///
/// [`repo::ResolvePolicy`] borrows these fields, so the solve rebuilds it
/// instead of storing the policy beside its data.
#[allow(clippy::struct_excessive_bools)]
pub(super) struct Prepared<'a> {
    pub(super) set: portage_repo::RepoSet,
    pub(super) atoms: &'a [super::TargetAtom],
    pub(super) world_additions: &'a [Dep],
    pub(super) arch: &'a gentoo_core::Arch,
    pub(super) format: crate::cli::DepgraphFormat,
    pub(super) verbose: u8,
    pub(super) empty: bool,
    pub(super) autounmask_write: bool,
    pub(super) autounmask_persist: super::AutounmaskPersist,
    pub(super) ask: bool,
    pub(super) autosolve_use: bool,
    pub(super) autounmask_widen: bool,
    pub(super) roots: &'a portage_resolve::Roots,
    pub(super) onlydeps: bool,
    pub(super) root_deps_rdeps: bool,
    pub(super) deep: bool,
    pub(super) update: bool,
    pub(super) nodeps: bool,
    pub(super) binpkg_index: Option<&'a portage_binpkg::BinpkgIndex>,
    pub(super) resume_completed: std::collections::HashSet<(MergeRoot, String)>,
    pub(super) complete_graph: bool,
    pub(super) quiet: bool,
    pub(super) exclude_atoms: Vec<Dep>,
    pub(super) selective: bool,
    pub(super) cross: root_aware::CrossContext,
    pub(super) config_root: Option<&'a camino::Utf8Path>,
    pub(super) host_config_stage: bool,
    pub(super) emptytree_native: bool,
    pub(super) solve_with_bdeps: bool,
    pub(super) vdb_snapshots: Arc<installed::VdbSnapshotCache>,
    pub(super) data: repo::RepoData,
    pub(super) target_installed: Vec<installed::VdbEntry>,
    pub(super) installed_blockers: Vec<Vec<Dep>>,
    pub(super) broot_snapshot: installed::BrootSnapshot,
    pub(super) host_installed: Vec<installed::HostInstalledEntry>,
    pub(super) resolved: repo::ResolvedPolicy,
    pub(super) use_expand: Vec<String>,
    pub(super) use_expand_hidden: Vec<String>,
    pub(super) distdir: String,
    pub(super) provided: Vec<Cpv>,
    pub(super) target_installed_cpvs: std::collections::HashSet<Cpv>,
    pub(super) host_installed_cpvs: std::collections::HashSet<Cpv>,
    pub(super) provided_cpvs: std::collections::HashSet<Cpv>,
    pub(super) use_reinstall_mode: Option<portage_resolve::use_reinstall::UseReinstallMode>,
    pub(super) installed: HashMap<Cpn, HashMap<Interned<DefaultInterner>, Version>>,
    pub(super) policy_facts: repo::PolicyFactsCache,
    pub(super) provided_avail: Vec<(Cpv, Option<String>)>,
    pub(super) root_deps: Vec<(PortagePackage, PortageVersionSet)>,
    pub(super) root_cpns: std::collections::HashSet<Cpn>,
    pub(super) unsatisfiable: Vec<super::output::UnsatisfiableTarget>,
    pub(super) slot_map: SlotMap,
    pub(super) widened_slot_map: Option<SlotMap>,
    pub(super) sysroot_installed: Vec<(PortagePackage, Version)>,
    pub(super) base_installed_cpvs: std::collections::HashSet<Cpv>,
    pub(super) root_pkgs: Vec<PortagePackage>,
    pub(super) pristine_package_use: Vec<(Dep, Vec<UseOverride>)>,
    pub(super) rebuilding_installed_cpvs: std::collections::HashSet<Cpv>,
}

/// Load repos, the installed trees, and profile policy, then classify roots.
pub(super) async fn prepare<'a>(
    opts: DepgraphOpts<'a>,
    metrics: &mut ResolveMetrics,
) -> anyhow::Result<Prepared<'a>> {
    let DepgraphOpts {
        set,
        atoms,
        world_additions,
        arch,
        format,
        verbose,
        empty,
        autounmask_write,
        autounmask_persist,
        ask,
        autosolve_use,
        autounmask_widen,
        roots,
        onlydeps,
        with_bdeps,
        root_deps_rdeps,
        deep,
        update,
        newuse,
        changed_use,
        noreplace,
        nodeps,
        host_merge_root,
        extra_use_override,
        extra_package_use,
        sysroot_override,
        binpkg_index,
        exclude,
        resume_completed,
        complete_graph,
        quiet,
    } = opts;
    let exclude_atoms: Vec<Dep> = exclude
        .iter()
        .filter_map(|s| match Dep::parse(s) {
            Ok(d) => Some(d),
            Err(e) => {
                crate::style::warn_line!("skipping invalid --exclude atom '{s}': {e}");
                None
            }
        })
        .collect();
    // `create_depgraph_params`' `selective`, restricted to the flags em has: an
    // installed instance may satisfy a named target, so the target is left alone
    // instead of being reinstalled. `--emptytree` is the explicit opposite.
    let selective = (update || noreplace || newuse || changed_use) && !empty;
    let cross = root_aware::detect(roots, host_merge_root);
    let config_root = roots.config();
    let host_config_stage = cross.active && cross.sysroot.as_str() != cross.target.as_str();
    // Native `emerge -pe`: pretend nothing merged on TARGET, but BROOT still
    // satisfies BDEPEND (emerge sets `bdeps=auto` unless overridden).
    let emptytree_native = empty && !host_config_stage && !cross.active;
    let solve_with_bdeps = with_bdeps || emptytree_native;
    // `set` is built once by the caller (shared with the atom-resolution step)
    // and already carries any caller-supplied aliases prepended onto it.

    let vdb_snapshots = Arc::new(installed::VdbSnapshotCache::default());
    let target_snapshots = Arc::clone(&vdb_snapshots);
    let broot_snapshots = Arc::clone(&vdb_snapshots);
    let (
        (raw_data, repo_load_elapsed),
        ((target_installed, installed_blockers), target_load_elapsed),
        (broot_snapshot, host_installed, host_load_elapsed),
        (use_env_result, use_env_elapsed),
    ) = tokio::join!(
        async {
            let start = Instant::now();
            let data = repo::load_repos(&set).await;
            (data, start.elapsed())
        },
        // Also precompute each installed package's blocker atoms on this task
        // (for `check_blockers`): the walk only needs the VDB, so it overlaps the
        // other concurrent loads instead of running serially before the solve.
        async {
            let start = Instant::now();
            let ti = installed::load_target_installed_with_cache(roots, &target_snapshots);
            let blockers: Vec<Vec<Dep>> =
                ti.iter().map(conflicts::installed_blocker_atoms).collect();
            ((ti, blockers), start.elapsed())
        },
        async {
            let start = Instant::now();
            let snapshot = installed::BrootSnapshot::load_with_cache(roots, &broot_snapshots);
            let host = installed::load_host_installed_from_snapshot(&snapshot);
            (snapshot, host, start.elapsed())
        },
        async {
            let start = Instant::now();
            let result = use_env::build_use_env(
                set.main(),
                config_root,
                roots.config_overlay(),
                extra_use_override,
                sysroot_override,
            )
            .await;
            (result, start.elapsed())
        },
    );
    metrics.record("repo_load", repo_load_elapsed);
    metrics.record("target_installed_load", target_load_elapsed);
    metrics.record("host_installed_load", host_load_elapsed);
    metrics.record("use_env_load", use_env_elapsed);
    let use_env = use_env_result?;

    // Fold global ACCEPT_KEYWORDS and per-package package.accept_keywords into a
    // single interned acceptance decision. A cross build accepts by the TARGET
    // arch (derived from the sysroot `CHOST`), not the host `--arch`, so the
    // target's keywords are honoured — a package keyworded `~riscv`/`riscv` is
    // accepted for a riscv sysroot even though the host is arm64. Without this
    // every target package would be filtered out (NoVersions).
    let accept_arch = cross.target_arch().unwrap_or(arch);
    let (resolved, extras) = repo::ResolvedPolicy::from_use_env(use_env, accept_arch);
    let repo::ResolvedPolicy {
        accept_keywords,
        accept_licenses,
        accept_properties,
        accept_restrict,
        package_mask,
        package_unmask,
        defaults,
        conf,
        env_use,
        package_use,
        profile_package_use,
        force_mask,
    } = resolved;
    let mut package_use = package_use;
    package_use.extend(extra_package_use.iter().cloned());
    let repo::UseEnvExtras {
        expand: use_expand,
        expand_hidden: use_expand_hidden,
        distdir,
        provided,
    } = extras;

    let target_installed_cpvs: std::collections::HashSet<Cpv> = target_installed
        .iter()
        .map(|e| Cpv::new(e.cpn, e.version.clone()))
        .collect();
    // `Cpv` carries no `merge_root`, so a `Host`-routed requirement (e.g.
    // `dev-lang/perl` needed at `base_roots()` as a BDEPEND tool) must never be
    // checked against `target_installed_cpvs`: a real target system commonly
    // has its own same-named, same-version package (a *different* build, for
    // a different root) which would otherwise wrongly look "already
    // installed" here.
    let host_installed_cpvs: std::collections::HashSet<Cpv> = host_installed
        .iter()
        .map(|e| Cpv::new(*e.package.cpn(), e.version.clone()))
        .collect();
    // `package.provided` CPVs, target-side only (BROOT satisfaction is a
    // separate, already-independent mechanism — see `add_host_installed`
    // below). Lets the plan-membership filter below treat a provided CPV
    // the same way a real installed one is treated: omit it unless an
    // explicit target/USE-rebuild pulls it back in.
    let provided_cpvs: std::collections::HashSet<Cpv> = provided.iter().cloned().collect();
    // Under `--emptytree` the solver treats target packages as rebuilds (not
    // "already installed" for cede/ingest), while action tags still use the
    // real VDB via `target_installed_cpvs`.
    let empty_solver_cpvs = std::collections::HashSet::new();
    let solver_installed_cpvs: &std::collections::HashSet<Cpv> = if emptytree_native {
        &empty_solver_cpvs
    } else {
        &target_installed_cpvs
    };
    // `-N`/`-U` reinstall mode (orthogonal to emptytree Rebuild).
    let use_reinstall_mode = if newuse {
        Some(portage_resolve::use_reinstall::UseReinstallMode::Newuse)
    } else if changed_use {
        Some(portage_resolve::use_reinstall::UseReinstallMode::ChangedUse)
    } else {
        None
    };

    let mut installed: HashMap<Cpn, HashMap<Interned<DefaultInterner>, Version>> = HashMap::new();
    for e in &target_installed {
        let slot_key = e.slot.unwrap_or_else(|| Interned::intern(""));
        installed
            .entry(e.cpn)
            .or_default()
            .insert(slot_key, e.version.clone());
    }

    let policy_facts = repo::PolicyFactsCache::new();
    let target_policy = repo::ResolvePolicy {
        accept_keywords: &accept_keywords,
        package_mask: &package_mask,
        package_unmask: &package_unmask,
        accept_licenses: &accept_licenses,
        accept_properties: &accept_properties,
        accept_restrict: &accept_restrict,
        defaults: &defaults,
        conf: &conf,
        env_use: &env_use,
        package_use: &package_use,
        profile_package_use: &profile_package_use,
        force_mask: &force_mask,
        facts: None,
    };

    // Collapse each repo's own copy of a duplicate cpv (see
    // `repo::collapse_duplicates`'s own doc) now that policy exists — masks/
    // keywords/license decide the winner instead of an arbitrary priority-only
    // pick, so a masked higher-priority repo's copy can't hide an otherwise-
    // available identical version from a lower-priority one.
    let collapse_start = Instant::now();
    let data = repo::collapse_duplicates(raw_data, &target_policy);
    metrics.record("collapse_duplicates", collapse_start.elapsed());
    let target_policy = target_policy.with_package_use(&policy_facts, &package_use);

    let provided_avail = provided_availability(&provided, &data);

    let RootTargets {
        deps: root_deps,
        cpns: root_cpns,
        unsatisfiable,
    } = classify_root_targets(
        atoms,
        &data,
        &target_policy,
        &installed,
        empty,
        selective,
        set.is_multi(),
    )?;

    // The whole-repository slot map (unslotted-dep resolution against
    // multi-slot packages) computed once, up front, and reused by every
    // co-solve fixpoint iteration below instead of being recomputed on each
    // provider rebuild — see `build_slot_map`'s doc comment for why that
    // recomputation is the single largest redundant cost per iteration.
    //
    // Uses the pristine (pre-cosolve) `package_use`: license acceptance can
    // depend on `package_use` through a USE-conditional LICENSE expression,
    // which *does* vary across iterations, so a package whose acceptance
    // flips because of a flag the fixpoint later cedes would see a stale
    // slot entry here. PMS-legal but vanishingly rare, not worth
    // recomputing the whole map every iteration to cover.
    let slot_map_start = Instant::now();
    let slot_map = build_slot_map(&repo::Adapter {
        data: &data,
        accept_keywords: &accept_keywords,
        package_mask: &package_mask,
        package_unmask: &package_unmask,
        accept_licenses: &accept_licenses,
        accept_properties: &accept_properties,
        accept_restrict: &accept_restrict,
        defaults: &defaults,
        conf: &conf,
        env_use: &env_use,
        package_use: &package_use,
        profile_package_use: &profile_package_use,
        force_mask: &force_mask,
        facts: target_policy.facts,
        installed_cpvs: solver_installed_cpvs,
        // Computed later (needs `root_pkgs`, not yet built here); inert
        // anyway since `autosolve_use: false` below means `cede_required_use`
        // is never reached through this Adapter.
        rebuilding_cpvs: &empty_solver_cpvs,
        autosolve_use: false,
        autounmask_widen: false,
    });
    metrics.record("slot_map", slot_map_start.elapsed());
    // Widened-mode slot map: tagged-only slots rank strictly below accepted
    // ones there. Phase 1 keeps the strict map so its graph shape stays
    // untouched; the extra whole-tree scan is paid only by callers that
    // request widening (crossdev flows).
    let widened_slot_map: Option<SlotMap> = autounmask_widen.then(|| {
        build_slot_map(&repo::Adapter {
            data: &data,
            accept_keywords: &accept_keywords,
            package_mask: &package_mask,
            package_unmask: &package_unmask,
            accept_licenses: &accept_licenses,
            accept_properties: &accept_properties,
            accept_restrict: &accept_restrict,
            defaults: &defaults,
            conf: &conf,
            env_use: &env_use,
            package_use: &package_use,
            profile_package_use: &profile_package_use,
            force_mask: &force_mask,
            facts: target_policy.facts,
            installed_cpvs: solver_installed_cpvs,
            rebuilding_cpvs: &empty_solver_cpvs,
            autosolve_use: false,
            autounmask_widen: true,
        })
    });

    // Sysroot VDB entries for `DEPEND` satisfaction under a cross build:
    // static for the whole invocation (doesn't depend on `pkg_use`), so
    // reading it from disk on every fixpoint iteration — as `build_and_solve`
    // used to, inline — was pure waste. Computed once here instead.
    let sysroot_installed: Vec<(PortagePackage, Version)> = if cross.active {
        installed::load_sysroot_entries_with_cache(cross.sysroot.as_path(), &vdb_snapshots)
            .into_iter()
            .map(|e| {
                let pkg = match e.slot.filter(|s| !s.is_empty()) {
                    Some(s) => PortagePackage::slotted(e.cpn, s),
                    None => PortagePackage::unslotted(e.cpn),
                };
                (pkg, e.version)
            })
            .collect()
    } else {
        Vec::new()
    };
    // `sysroot_installed` as `Cpv`s: what a `MergeRoot::Base` entry (the
    // board-root topology's toolchain-sysroot DEPEND copy) checks "already
    // installed" against — never `target_installed_cpvs`, the board root's
    // own, unrelated VDB.
    let base_installed_cpvs: std::collections::HashSet<Cpv> = sysroot_installed
        .iter()
        .map(|(pkg, v)| Cpv::new(*pkg.cpn(), v.clone()))
        .collect();

    // Only names originally targeted (never a repair target added below) are
    // "explicit" for reinstall/tree-root/onlydeps purposes — computed once,
    // from the untouched `root_deps`, so a repair loop round can never make an
    // added dependent look like a user-requested target.
    let root_pkgs: Vec<PortagePackage> = root_deps.iter().map(|(p, _)| p.clone()).collect();
    let pristine_package_use = package_use.clone();

    // Installed cpvs this run rebuilds anyway — Level-C's cede gate
    // (`repo::Adapter::rebuilding_cpvs`) treats these as build targets, not
    // as "installed and staying installed" (see its doc comment for why).
    // Mirrors the plan's own already-installed filter below: a
    // non-selective explicit root target (reinstalled `[R]` at its
    // installed version) or a `-N`/`-U` USE-drift rebuild.
    //
    // Computed once, using the pristine `target_policy`/`package_use` —
    // same accepted approximation `slot_map` above documents, PMS-legal
    // and vanishingly rare, not worth recomputing per iteration for.
    // `--emptytree` needs no entry: its `installed_cpvs` is already empty,
    // so cede already applies universally there.
    let mut rebuilding_installed_cpvs: std::collections::HashSet<Cpv> =
        std::collections::HashSet::new();
    if !emptytree_native {
        for e in &target_installed {
            let pkg = match e.slot.filter(|s| !s.is_empty()) {
                Some(s) => PortagePackage::slotted(e.cpn, s),
                None => PortagePackage::unslotted(e.cpn),
            };
            let explicit_reinstall = !selective
                && root_pkgs
                    .iter()
                    .any(|r| r.cpn() == pkg.cpn() && r.slot() == pkg.slot());
            let use_rebuild = use_reinstall_mode.is_some_and(|mode| {
                package_needs_use_reinstall(mode, e, &pkg, &data, &target_policy)
            });
            if explicit_reinstall || use_rebuild {
                rebuilding_installed_cpvs.insert(Cpv::new(e.cpn, e.version.clone()));
            }
        }
    }
    let resolved = repo::ResolvedPolicy {
        accept_keywords,
        accept_licenses,
        accept_properties,
        accept_restrict,
        package_mask,
        package_unmask,
        defaults,
        conf,
        env_use,
        package_use,
        profile_package_use,
        force_mask,
    };
    Ok(Prepared {
        set,
        atoms,
        world_additions,
        arch,
        format,
        verbose,
        empty,
        autounmask_write,
        autounmask_persist,
        ask,
        autosolve_use,
        autounmask_widen,
        roots,
        onlydeps,
        root_deps_rdeps,
        deep,
        update,
        nodeps,
        binpkg_index,
        resume_completed,
        complete_graph,
        quiet,
        exclude_atoms,
        selective,
        cross,
        config_root,
        host_config_stage,
        emptytree_native,
        solve_with_bdeps,
        vdb_snapshots,
        data,
        target_installed,
        installed_blockers,
        broot_snapshot,
        host_installed,
        resolved,
        use_expand,
        use_expand_hidden,
        distdir,
        provided,
        target_installed_cpvs,
        host_installed_cpvs,
        provided_cpvs,
        use_reinstall_mode,
        installed,
        policy_facts,
        provided_avail,
        root_deps,
        root_cpns,
        unsatisfiable,
        slot_map,
        widened_slot_map,
        sysroot_installed,
        base_installed_cpvs,
        root_pkgs,
        pristine_package_use,
        rebuilding_installed_cpvs,
    })
}
