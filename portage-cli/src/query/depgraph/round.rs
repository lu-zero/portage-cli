//! One solve-and-plan round of [`super::depgraph`].

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use portage_atom::interner::Interned;
use portage_atom::{Cpn, Cpv, Dep, Version};
use portage_atom_pubgrub::{
    CededFlag, DepEdge, InstalledPackage as SolverInstalledPackage, InstalledPolicy, MergeRoot,
    PortageDependencyProvider, PortagePackage, PortageVersionSet, SlotMap, UseFlagRequirement,
    UseOverride,
};
use portage_resolve::{
    bdepend_trim, conflicts, depend_trim, effective_use, installed, repo, root_aware, root_closure,
    subslot,
};

use super::{
    ResolveMetrics, apply_order_filters, best_rebuild_version, package_needs_use_reinstall,
    package_use, target_conflicts, widened_autounmask_candidates,
};

/// Inputs shared by every solve round of one [`super::depgraph`] call.
///
/// One bool per resolve flag, so a round reads the same names the caller bound.
#[derive(Clone, Copy)]
#[allow(clippy::struct_excessive_bools)]
pub(super) struct RoundCtx<'a> {
    pub(super) data: &'a repo::RepoData,
    pub(super) accept_keywords: &'a repo::AcceptKeywords,
    pub(super) package_mask: &'a repo::PolicyMaskList,
    pub(super) package_unmask: &'a repo::PolicyMaskList,
    pub(super) accept_licenses: &'a repo::AcceptLicenses,
    pub(super) accept_properties: &'a repo::AcceptProperties,
    pub(super) accept_restrict: &'a repo::AcceptRestrict,
    pub(super) defaults: &'a portage_atom_pubgrub::UseLayer,
    pub(super) conf: &'a portage_atom_pubgrub::UseLayer,
    pub(super) env_use: &'a portage_atom_pubgrub::UseLayer,
    pub(super) profile_package_use: &'a [portage_atom_pubgrub::ProfileUseNode],
    pub(super) force_mask: &'a portage_resolve::force_mask::ForceMask,
    pub(super) policy_facts: &'a repo::PolicyFactsCache,
    pub(super) solver_installed_cpvs: &'a HashSet<Cpv>,
    pub(super) rebuilding_installed_cpvs: &'a HashSet<Cpv>,
    pub(super) slot_map: &'a SlotMap,
    pub(super) widened_slot_map: &'a Option<SlotMap>,
    pub(super) target_policy: &'a repo::ResolvePolicy<'a>,
    pub(super) package_use: &'a Vec<(Dep, Vec<UseOverride>)>,
    pub(super) target_installed: &'a [installed::VdbEntry],
    pub(super) installed_blockers: &'a [Vec<Dep>],
    pub(super) sysroot_installed: &'a [(PortagePackage, Version)],
    pub(super) host_installed: &'a [installed::HostInstalledEntry],
    pub(super) provided: &'a [Cpv],
    pub(super) provided_avail: &'a [(Cpv, Option<String>)],
    pub(super) root_pkgs: &'a [PortagePackage],
    pub(super) root_cpns: &'a HashSet<Cpn>,
    pub(super) exclude_atoms: &'a [Dep],
    pub(super) resume_completed: &'a HashSet<(MergeRoot, String)>,
    pub(super) cross: &'a root_aware::CrossContext,
    pub(super) roots: &'a portage_resolve::Roots,
    pub(super) broot_snapshot: &'a installed::BrootSnapshot,
    pub(super) vdb_snapshots: &'a Arc<installed::VdbSnapshotCache>,
    pub(super) host_installed_cpvs: &'a HashSet<Cpv>,
    pub(super) base_installed_cpvs: &'a HashSet<Cpv>,
    pub(super) target_installed_cpvs: &'a HashSet<Cpv>,
    pub(super) provided_cpvs: &'a HashSet<Cpv>,
    pub(super) emptytree_native: bool,
    pub(super) solve_with_bdeps: bool,
    pub(super) empty: bool,
    pub(super) selective: bool,
    pub(super) update: bool,
    pub(super) deep: bool,
    pub(super) nodeps: bool,
    pub(super) root_deps_rdeps: bool,
    pub(super) onlydeps: bool,
    pub(super) autosolve_use: bool,
    pub(super) host_config_stage: bool,
    pub(super) autounmask_widen: bool,
    pub(super) use_reinstall_mode: Option<portage_resolve::use_reinstall::UseReinstallMode>,
}

/// One co-solve fixpoint plus its final solve, at the current widened setting.
type SolveAttempt = anyhow::Result<(
    PortageDependencyProvider,
    pubgrub::SelectedDependencies<PortagePackage, Version>,
    Vec<(Dep, Vec<UseOverride>)>,
    Vec<UseFlagRequirement>,
)>;

/// One solve round's provider, solution, and filtered install order.
///
/// The `--complete-graph` repair loop re-runs this and keeps the last good
/// attempt. Nothing is printed until the caller settles on a round.
pub(super) struct RoundOutcome {
    pub(super) provider: PortageDependencyProvider,
    pub(super) solution: pubgrub::SelectedDependencies<PortagePackage, Version>,
    pub(super) order: Vec<(PortagePackage, Version)>,
    pub(super) edges: Vec<DepEdge>,
    // `--jobs N` blocker-only edges from `root_closure`'s synthetic
    // entries — kept separate from `edges` (which also feeds
    // `--tree`/`--json` display) since these aren't real PMS dependency
    // relationships, only scheduler ordering.
    pub(super) closure_blockers: Vec<(PortagePackage, PortagePackage)>,
    pub(super) package_use: Vec<(Dep, Vec<UseOverride>)>,
    pub(super) applied_reqs: Vec<UseFlagRequirement>,
    pub(super) ceded: Vec<CededFlag>,
    pub(super) autounmask_candidates: Vec<repo::AutounmaskCandidate>,
    /// Phase-2 widened selections (chosen versions outside acceptance);
    /// merged after the dropped-dep post-filter, which would otherwise
    /// discard them by construction.
    pub(super) widened_autounmask_candidates: Vec<repo::AutounmaskCandidate>,
    pub(super) slot_op_cpns: HashSet<Cpn>,
    pub(super) dep_conflicts: Vec<conflicts::Conflict>,
    // Target-routed merge plan entries, kept alongside `dep_conflicts` for
    // the blocker classifier (`conflicts::classify_blockers`), which needs
    // the same "what's proposed" view to simulate an auto-unmerge.
    pub(super) proposed: Vec<conflicts::ProposedPkg>,
    pub(super) exclude_omitted: usize,
    pub(super) resume_omitted: usize,
    pub(super) round_metrics: ResolveMetrics,
}

/// Solve `targets` and return the filtered order for this round.
///
/// A failed `--autosolve-use` attempt falls back to a fixed-USE solve.
/// When widening is requested, a strict failure or a strict success that
/// dropped hard dependencies retries once with widened candidate supply.
pub(super) fn solve_round(
    ctx: &RoundCtx<'_>,
    targets: Vec<(PortagePackage, PortageVersionSet)>,
) -> anyhow::Result<RoundOutcome> {
    let RoundCtx {
        data,
        accept_keywords,
        package_mask,
        package_unmask,
        accept_licenses,
        accept_properties,
        accept_restrict,
        defaults,
        conf,
        env_use,
        profile_package_use,
        force_mask,
        policy_facts,
        solver_installed_cpvs,
        rebuilding_installed_cpvs,
        slot_map,
        widened_slot_map,
        target_policy,
        package_use,
        target_installed,
        installed_blockers,
        sysroot_installed,
        host_installed,
        provided,
        provided_avail,
        root_pkgs,
        root_cpns,
        exclude_atoms,
        resume_completed,
        cross,
        roots,
        broot_snapshot,
        vdb_snapshots,
        host_installed_cpvs,
        base_installed_cpvs,
        target_installed_cpvs,
        provided_cpvs,
        emptytree_native,
        solve_with_bdeps,
        empty,
        selective,
        update,
        deep,
        nodeps,
        root_deps_rdeps,
        onlydeps,
        autosolve_use,
        host_config_stage,
        autounmask_widen,
        use_reinstall_mode,
    } = *ctx;

    let mut round_metrics = ResolveMetrics::default();
    // Phase 2 flips this on: the strict solve failed, so retry (and
    // keep) widened candidate supply. Strict-first keeps every
    // non-crossdev resolve byte-identical.
    let widened = std::cell::Cell::new(false);
    let build_and_solve = |autosolve_use: bool, pkg_use: &[(Dep, Vec<UseOverride>)]| {
        let adapter = repo::Adapter {
            data,
            accept_keywords,
            package_mask,
            package_unmask,
            accept_licenses,
            accept_properties,
            accept_restrict,
            defaults,
            conf,
            env_use,
            package_use: pkg_use,
            profile_package_use,
            force_mask,
            facts: Some(policy_facts.generation_for(pkg_use)),
            installed_cpvs: solver_installed_cpvs,
            rebuilding_cpvs: rebuilding_installed_cpvs,
            autosolve_use,
            autounmask_widen: widened.get(),
        };
        let slot_map_ref: &SlotMap = if widened.get() {
            widened_slot_map
                .as_ref()
                .expect("widened phase without a widened slot map")
        } else {
            slot_map
        };
        // Outlives `adapter` (it borrows the same run-scoped refs, including
        // this iteration's `pkg_use`), so it stays usable after the move below.
        let iteration_policy = adapter.policy();
        // Closure-seeded ingestion: only packages reachable from the targets
        // and the installed set get converted (a few hundred for a typical
        // resolve), instead of the whole tree — this is what makes the
        // co-solve fixpoint's per-iteration provider rebuild affordable.
        let mut seeds: Vec<Cpn> = targets
            .iter()
            .filter(|(pkg, _)| !pkg.is_virtual())
            .map(|(pkg, _)| *pkg.cpn())
            .collect();
        if !emptytree_native {
            seeds.extend(target_installed.iter().map(|e| e.cpn));
        }
        // `package.provided` CPNs also need real tree/cache data loaded so they
        // can be registered as installed below — unconditional, unlike the
        // target_installed seeding above: emptytree only affects VDB selection.
        seeds.extend(provided.iter().map(|cpv| cpv.cpn));
        let provider_start = Instant::now();
        let mut provider = PortageDependencyProvider::new_for_targets_with_bdeps_and_slot_map(
            adapter,
            seeds,
            solve_with_bdeps,
            slot_map_ref,
        );
        tracing::debug!(
            elapsed_ms = provider_start.elapsed().as_secs_f64() * 1000.0,
            "provider build"
        );
        provider.set_cross_active(cross.active);
        provider.set_is_cross_arch(cross.is_cross_arch());
        // crossdev `--root-deps=rdeps`: caller-supplied (see `DepgraphOpts::
        // root_deps_rdeps`) — a property of which operation is running, not of
        // the sysroot's CHOST/CBUILD.
        provider.set_root_deps_rdeps(root_deps_rdeps);
        provider.set_nodeps(nodeps);
        provider.set_rebuild_tree(emptytree_native);
        // `--deep` and native emptytree bump `:*` deps to the newest slot.
        provider.set_prefer_newest_slot(deep || emptytree_native);
        // `-uD`: in-slot upgrades for the whole solve (not emptytree Rebuild).
        // Also the path that re-opens host-satisfied build edges so deep tools
        // can upgrade/rebuild; `-N` alone must not do that (Portage parity).
        provider.set_prefer_update(update && deep && !emptytree_native);
        // `-n`/`-N`/`-U` without `-u`: a root target an installed version
        // already satisfies keeps that version instead of taking the newest.
        provider.set_selective_no_update(selective && !update);
        for (pkg, version) in sysroot_installed {
            provider.add_sysroot_installed(pkg.clone(), version.clone());
        }
        for (e, blockers) in target_installed.iter().zip(installed_blockers) {
            let pkg = match e.slot.filter(|s| !s.is_empty()) {
                Some(s) => PortagePackage::slotted(e.cpn, s),
                None => PortagePackage::unslotted(e.cpn),
            };
            provider.add_installed_blockers(&pkg, blockers);
            let policy = if emptytree_native {
                InstalledPolicy::Rebuild
            } else if let Some(mode) = use_reinstall_mode {
                // Compare VDB USE/IUSE to the planned fold for this CPV.
                // Rebuild ⇒ no Favor + full build-dep expansion when selected.
                if package_needs_use_reinstall(mode, e, &pkg, data, &iteration_policy) {
                    InstalledPolicy::Rebuild
                } else {
                    InstalledPolicy::Favor
                }
            } else {
                InstalledPolicy::Favor
            };
            provider.add_installed(SolverInstalledPackage {
                package: pkg,
                version: e.version.clone(),
                policy,
                active_use: e.active_use.clone(),
                iuse: e.iuse.clone(),
            });
        }
        // `package.provided`: CPVs the system supplies externally. Registered as
        // an ordinary installed/Favor package (not a separate edge filter), so a
        // dependency edge is satisfied normally (version-range-checked) and an
        // explicit target naming a provided CPN still gets solved/built via the
        // existing root_pkgs reinstall check below — same as any installed pkg.
        // Skipped when a real VDB entry already covers the same (cpn, slot): once
        // genuinely built, the real entry wins on every later resolve for free.
        let target_installed_keys: HashSet<(Cpn, Option<String>)> = target_installed
            .iter()
            .map(|e| (e.cpn, e.slot.map(|s| s.to_string())))
            .collect();
        // A `package.provided` CPV is supplied by the *system*, so it is present
        // on the build host (BROOT) too: seed it as host-installed so BDEPEND on
        // it (e.g. a build tool needing the interpreter) is satisfied without
        // resolving a repo version onto @host — otherwise a slot the repo can't
        // build (python:3.14 on arm64-macos) would be pulled to the newest
        // available (python-3.15.9999), conflicting with the provided slot.
        for (cpv, slot) in provided_avail {
            let pkg = match slot {
                Some(s) => PortagePackage::slotted(cpv.cpn, Interned::intern(s)),
                None => PortagePackage::unslotted(cpv.cpn),
            };
            if !target_installed_keys.contains(&(cpv.cpn, slot.clone())) {
                let (active_use, iuse) = match repo::find_cache(data, &pkg, &cpv.version) {
                    Some(cache) => {
                        use portage_atom_pubgrub::UseFlagState;
                        let cfg = effective_use::effective_use(
                            &iteration_policy,
                            &pkg,
                            &cpv.version,
                            cache,
                            true,
                            &[],
                        );
                        let iuse = effective_use::iuse_set(
                            cache,
                            &iteration_policy.force_mask.iuse_injection,
                        );
                        let active = iuse
                            .iter()
                            .copied()
                            .filter(|f| matches!(cfg.get(*f), UseFlagState::Enabled))
                            .collect();
                        (active, iuse.into_iter().collect())
                    }
                    None => (Vec::new(), Vec::new()),
                };
                provider.add_installed(SolverInstalledPackage {
                    package: pkg.clone(),
                    version: cpv.version.clone(),
                    policy: InstalledPolicy::Provided,
                    active_use,
                    iuse,
                });
            }
            provider.add_host_installed(pkg, cpv.version.clone(), Vec::new(), Vec::new());
        }
        // BROOT (the host) provides build tools: a BDEPEND already present there
        // is satisfied without building it into the plan — unless a USE-dep on
        // that edge demands a flag the host lacks, in which case the package is
        // rebuilt (the host entry carries USE/IUSE for that check).
        for e in host_installed {
            provider.add_host_installed(
                e.package.clone(),
                e.version.clone(),
                e.active_use.clone(),
                e.iuse.clone(),
            );
        }
        let resolve_start = Instant::now();
        let result = provider.resolve_targets(targets.clone());
        tracing::debug!(
            elapsed_ms = resolve_start.elapsed().as_secs_f64() * 1000.0,
            "solver resolve"
        );
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
    //
    // One attempt = cosolve fixpoint + final solve, at the current
    // `widened` setting. Phase 2 re-runs it with widened candidate
    // supply when the strict attempt hard-fails (crossdev keywording
    // gaps); the strict error is kept as the user-facing failure when
    // even the widened attempt can't resolve.
    let attempt_solve = || -> SolveAttempt {
        let (pu, applied_reqs, solved) = package_use::cosolve_use_deps(
            package_use.clone(),
            data,
            |pu| {
                let (provider, result) = build_and_solve(autosolve_use, pu);
                result.ok().map(|sol| (provider, sol))
            },
            |(provider, _)| provider.use_flag_requirements().to_vec(),
        );

        let (provider, solution) = match solved {
            Some(solved) => solved,
            None => {
                let (provider, result) = build_and_solve(autosolve_use, &pu);
                match result {
                    Ok(sol) => (provider, sol),
                    Err(_) if autosolve_use => {
                        // REQUIRED_USE could not be auto-satisfied; fall back to a
                        // fixed-USE solve so the plan + Level-A advisory still appear.
                        crate::style::warn_line!(
                            "--autosolve-use could not satisfy REQUIRED_USE; \
                         falling back to a fixed-USE plan.",
                        );
                        let (provider, result) = build_and_solve(false, &pu);
                        let sol = result.map_err(|e2| {
                            anyhow::anyhow!(
                                "resolution failed:\n{}",
                                portage_atom_pubgrub::format_solve_error(e2)
                            )
                        })?;
                        (provider, sol)
                    }
                    Err(e) => {
                        return Err(anyhow::anyhow!(
                            "resolution failed:\n{}",
                            portage_atom_pubgrub::format_solve_error(e)
                        ));
                    }
                }
            }
        };
        Ok((provider, solution, pu, applied_reqs))
    };

    let (provider, solution, package_use, applied_reqs) = match attempt_solve() {
        Ok(outcome) => {
            // A strict solve can still be degraded: hard deps whose
            // every version was acceptance-filtered are dropped at
            // provider construction, so PubGrub never fails on them
            // and the failure-path retry below never runs. Escalate
            // once when that happened — the widened attempt either
            // resolves them (complete plan, bounded grants) or its
            // own dropped list keeps the exact-pin advisories.
            if autounmask_widen
                && !widened.get()
                && repo::has_widening_eligible_drops(outcome.0.dropped_deps(), data)
            {
                widened.set(true);
                tracing::debug!(
                    "acceptance-filtered hard deps were dropped; \
                     retrying with widened candidate supply"
                );
                // Phase 2 can fail outright (e.g. the dropped dep's
                // only matching versions are tagged-live and
                // `filter_to_preferred_tier` empties them for a
                // non-root) — keep the degraded strict plan and its
                // exact-pin advisories rather than losing the plan.
                attempt_solve().unwrap_or_else(|e| {
                    tracing::debug!("widened retry failed ({e}); keeping the degraded strict plan");
                    outcome
                })
            } else {
                outcome
            }
        }
        Err(strict_err) if autounmask_widen && !widened.get() => {
            widened.set(true);
            tracing::debug!("strict solve failed; retrying with widened candidate supply");
            attempt_solve().map_err(|_| strict_err)?
        }
        Err(e) => return Err(e),
    };

    // Fold Level-C ceded flag values into package.use for report/autounmask
    // consumers that still walk that list. Force/mask is **not** smuggled here
    // anymore — it is applied as a true post-fold step (like Portage) via
    // `effective_use::apply_force_mask` / `Adapter::desired_use`, so env-level
    // `USE="-* …"` cannot wipe forced flags on the display/build path.
    let ceded = provider.solved_use_decisions();
    let package_use: Vec<(Dep, Vec<UseOverride>)> = if ceded.is_empty() {
        package_use
    } else {
        let mut by_cpn: HashMap<Cpn, Vec<&CededFlag>> = HashMap::new();
        for c in &ceded {
            by_cpn.entry(c.cpn).or_default().push(c);
        }
        let mut combined = package_use;
        for (pkg, ver) in solution.iter() {
            if pkg.is_virtual() {
                continue;
            }
            let Some(flags) = by_cpn.get(pkg.cpn()) else {
                continue;
            };
            let atom = format!("={}/{}-{}", pkg.cpn().category, pkg.cpn().package, ver);
            let Ok(dep) = Dep::parse(&atom) else { continue };
            let overrides = flags
                .iter()
                .map(|c| UseOverride {
                    flag: c.flag,
                    enable: c.value,
                })
                .collect();
            combined.push((dep, overrides));
        }
        combined
    };

    // `package_use` is settled from here on (no further rebind below), so
    // this is built once and reused by every remaining filter/size call
    // instead of each one re-listing the same 8 fields.
    let final_policy = repo::ResolvePolicy {
        accept_keywords,
        package_mask,
        package_unmask,
        accept_licenses,
        accept_properties,
        accept_restrict,
        defaults,
        conf,
        env_use,
        package_use: &package_use,
        profile_package_use,
        force_mask,
        facts: None,
    }
    .with_package_use(policy_facts, &package_use);

    // Autounmask: detect filtered candidates from dropped deps. Filtered
    // to just this round's actionable set and reported once the repair
    // loop settles (see below).
    let autounmask_candidates =
        repo::find_autounmask_candidates(data, provider.dropped_deps(), &final_policy);
    let widened_candidates = widened_autounmask_candidates(
        widened.get(),
        &solution,
        &provider,
        data,
        &final_policy,
        &autounmask_candidates,
    );

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

    // Every `Real` package the solver selected (install_order already
    // omits solver-internal nodes; `!is_virtual()` is defensive). Gentoo
    // category `virtual/*` (e.g. `virtual/libcrypt`) **are** Real and
    // stay here. The next filter may drop them from the *displayed*
    // plan when already installed — keep `full_order` so bdepend_trim
    // still sees their RDEPEND (e.g. libxcrypt only required via the
    // virtual) and does not treat providers as orphaned.
    let install_order_start = Instant::now();
    let graph_result = provider.install_order_with_stats(&solution);
    let graph_stats = graph_result.stats;
    let full_order: Vec<(PortagePackage, Version)> = graph_result
        .order
        .into_iter()
        .filter(|(pkg, _)| !pkg.is_virtual())
        .collect();
    round_metrics.record("install_order", install_order_start.elapsed());
    round_metrics.record("dependency_graph", graph_stats.graph_build_time);

    let order: Vec<_> = full_order
        .iter()
        .filter(|(pkg, ver)| {
            let cpv = Cpv::new(*pkg.cpn(), ver.clone());
            // Drop packages already installed at this version, except:
            //  - same-version USE rebuilds (reinstall_cpns), and
            //  - explicitly-requested targets, which emerge reinstalls by
            //    default ([ebuild R]) even when already at the best version —
            //    unless the resolve is selective, where an up-to-date target is
            //    left alone.
            // "Already installed" is root-specific: a `Host` requirement
            // (built into `base_roots()`) must only be dropped if it's
            // installed *there*, never because the unrelated Target sysroot
            // happens to have a same-named, same-version package.
            let already_installed = match pkg.merge_root() {
                MergeRoot::Host => host_installed_cpvs.contains(&cpv),
                MergeRoot::Base => base_installed_cpvs.contains(&cpv),
                MergeRoot::Target => {
                    target_installed_cpvs.contains(&cpv) || provided_cpvs.contains(&cpv)
                }
            };
            // `-N`/`-U` registers USE-drift packages as `InstalledPolicy::Rebuild`
            // and still selects the installed CPV for a same-version rebuild —
            // those must stay in the plan ([R]), not be dropped as "already
            // installed".
            let use_rebuild = provider
                .installed_policy(pkg)
                .is_some_and(|p| matches!(p, InstalledPolicy::Rebuild));
            !already_installed
        || reinstall_cpns.contains(pkg.cpn())
        || use_rebuild
        // Explicit target: reinstalled even at best version ([R]). Match
        // the resolved target *slot*, not the bare CPN — a sibling slot
        // merely pulled as a satisfied dep (e.g. python:3.13 under a
        // `python` target) must not be re-listed. Set provenance plays
        // no part: `emerge @world` reinstalls its members exactly as it
        // reinstalls a named atom (measured).
        || (!selective
            && root_pkgs
                .iter()
                .any(|r| r.cpn() == pkg.cpn() && r.slot() == pkg.slot()))
        || emptytree_native
        })
        .cloned()
        .map(|(pkg, ver)| {
            // Apply the favoured upgrade version if one was recorded.
            let ver = upgrades.get(pkg.cpn()).cloned().unwrap_or(ver);
            (pkg, ver)
        })
        .collect();

    let (filtered_order, exclude_omitted, resume_omitted) = apply_order_filters(
        order,
        &provider.reinstall_deps(),
        exclude_atoms,
        resume_completed,
        host_config_stage && cross.is_cross_arch(),
    );
    let mut order = filtered_order;

    let trim_ctx = bdepend_trim::TrimCtx {
        broot_snapshot,
        data,
        policy: final_policy,
        root_cpns,
        reinstall_cpns: &reinstall_cpns,
    };
    if host_config_stage {
        // The trim drops DEPEND already satisfied on the *build* sysroot
        // (ESYSROOT), which is what the build links against. For a from-scratch
        // offset (`--root`, base == target) the shell builds with SYSROOT = ROOT,
        // so DEPEND must be satisfied in the ROOT, not the host config root —
        // `build_sysroot()` is `None` there, which we map to the target so the
        // trim is a no-op (nothing host-satisfied). Only a `--prefix` overlay
        // (base != target) has a distinct build sysroot to trim against.
        let before = order.len();
        let trim_start = Instant::now();
        order = depend_trim::trim_sysroot_satisfied_depend_with_cache(
            order,
            roots.build_sysroot().or(Some(cross.target.as_path())),
            cross.target.as_path(),
            &trim_ctx,
            vdb_snapshots,
        );
        round_metrics.depend_trimmed += before.saturating_sub(order.len());
        round_metrics.record("depend_trim", trim_start.elapsed());
    }

    // `-uD` only: do not post-trim host-satisfied BDEPEND tools (deep update
    // intentionally re-selected them). `-N` alone leaves normal trim — USE-drift
    // packages already in the graph are kept via `use_rebuild` / reinstall_cpns.
    let skip_bdepend_trim = update && deep && !emptytree_native;
    if !emptytree_native && !skip_bdepend_trim {
        // Built packages always carry their BDEPEND now (it's required to build
        // them), so always run the within-run trim to drop entries only needed
        // for BDEPEND already satisfied on BROOT or by an earlier kept entry —
        // matching emerge, which trims a built package's redundant build tools
        // regardless of `--with-bdeps`.
        let before = order.len();
        let trim_start = Instant::now();
        order = bdepend_trim::trim_within_run_bdepend(order, &full_order, true, &trim_ctx);
        round_metrics.bdepend_trimmed += before.saturating_sub(order.len());
        round_metrics.record("bdepend_trim", trim_start.elapsed());
    }
    // Native --emptytree lists the full deep closure straight from the solve
    // (the provider returns un-pruned deps under `rebuild_tree`); no post-solve
    // re-list.

    // Edges for "targets last" / build_blockers. Drop endpoints that are
    // **solver-internal** nodes (`Choice` / `UseDecision` / … via
    // `PortagePackage::is_virtual`) — not Gentoo `virtual/*` packages,
    // which are `Real` and must keep RDEPEND edges (e.g.
    // `virtual/libcrypt` → `sys-libs/libxcrypt`) for scheduling.
    let edges: Vec<_> = graph_result
        .edges
        .into_iter()
        .filter(|e| !e.from.0.is_virtual() && !e.to.0.is_virtual())
        .collect();
    round_metrics.graph_builds += graph_stats.graph_builds;
    round_metrics.graph_nodes += graph_stats.graph_nodes;
    round_metrics.virtual_expansions += graph_stats.virtual_expansions;
    round_metrics.graph_edges += edges.len();
    round_metrics.scc_count += graph_stats.scc_count;
    round_metrics.largest_scc = round_metrics.largest_scc.max(graph_stats.largest_scc);
    round_metrics.repair_reachability_calls += graph_stats.repair_reachability_calls;
    round_metrics.repair_reachability_nodes += graph_stats.repair_reachability_nodes;

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
            if let Some(cache) = repo::find_cache(data, pkg, ver) {
                planned_slots
                    .entry(*pkg.cpn())
                    .or_default()
                    .push((ver.clone(), cache.metadata.slot));
            }
        }
        let in_plan: std::collections::HashSet<Cpn> =
            order.iter().map(|(pkg, _)| *pkg.cpn()).collect();
        for rb in subslot::find_rebuilds(target_installed, &planned_slots, &in_plan) {
            let pos = order
                .iter()
                .rposition(|(pkg, _)| rb.triggers.contains(pkg.cpn()))
                .map_or(order.len(), |i| i + 1);
            let pkg = match rb.slot.as_deref().filter(|s| !s.is_empty()) {
                Some(s) => PortagePackage::slotted(rb.cpn, Interned::intern(s)),
                None => PortagePackage::unslotted(rb.cpn),
            };
            let ver = best_rebuild_version(data, target_policy, &rb, &planned_slots);
            order.insert(pos, (pkg, ver));
            slot_op_cpns.insert(rb.cpn);
            slot_op_cpns.extend(rb.triggers.iter().copied());
        }
    }

    // Shared by both closure walks below: `accept_keywords` here is
    // target-arch-scoped (`cross.target_arch()`), which is what a
    // sysroot entry needs too — it builds at the target arch, not
    // the build host's.
    let closure_adapter = repo::Adapter {
        data,
        accept_keywords,
        package_mask,
        package_unmask,
        accept_licenses,
        accept_properties,
        accept_restrict,
        defaults,
        conf,
        env_use,
        package_use: &package_use,
        profile_package_use,
        force_mask,
        facts: final_policy.facts,
        installed_cpvs: solver_installed_cpvs,
        rebuilding_cpvs: rebuilding_installed_cpvs,
        autosolve_use: false,
        autounmask_widen: false,
    };
    // Native offset (same-arch `--root`/`--prefix`): a target
    // package's build edges the host lacks are merged to BROOT
    // (`/`) so the target can build against them.
    let host_plan =
        root_closure::host_with_snapshot(&order, &closure_adapter, cross, broot_snapshot);
    order = host_plan.order;
    // Board-root topology (`--target T --root R`): the toolchain
    // sysroot is a separate merge destination from ROOT, so a
    // target package's DEPEND provider must also land there (PMS
    // table 8.2). Runs after depend_trim and after the
    // host_config_stage Target-only retain above, so it never
    // schedules an entry the plan doesn't hold, and its own entries
    // are never retained away.
    let base_plan = root_closure::base_with_cache(&order, &closure_adapter, roots, vdb_snapshots);
    order = base_plan.order;
    let mut closure_blockers = host_plan.blockers;
    closure_blockers.extend(base_plan.blockers);

    // Reverse-dependency constraints: a complete-graph check that emerge's
    // default targeted `-p` skips (e.g. upgrading docutils past an
    // installed package's `<` bound). Computed here (pure, no report yet)
    // so the `--complete-graph` repair loop can decide whether another
    // round is needed before anything is printed or written.
    let (proposed, dep_conflicts) = target_conflicts(&order, target_installed);

    Ok(RoundOutcome {
        provider,
        solution,
        order,
        edges,
        closure_blockers,
        package_use,
        applied_reqs,
        ceded,
        autounmask_candidates,
        widened_autounmask_candidates: widened_candidates,
        slot_op_cpns,
        dep_conflicts,
        proposed,
        exclude_omitted,
        resume_omitted,
        round_metrics,
    })
}
