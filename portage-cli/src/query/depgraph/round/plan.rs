//! The plan half of a round: from a solution to the filtered, trimmed and ordered merge list

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use portage_atom::interner::Interned;
use portage_atom::{Cpn, Cpv, Dep, Version};
use portage_atom_pubgrub::{
    CededFlag, DepEdge, GraphStats, InstallOrderResult, InstalledPolicy, MergeRoot,
    PortageDependencyProvider, PortagePackage, PortageVersionSet, UseOverride,
};
use portage_resolve::{bdepend_trim, depend_trim, repo, root_closure, subslot};

use super::solve::Solved;
use super::{RoundCtx, RoundOutcome};
use crate::query::depgraph::{
    ResolveMetrics, apply_order_filters, best_rebuild_version, installed_as_solved,
    target_conflicts, widened_autounmask_candidates,
};

type Order = Vec<(PortagePackage, Version)>;
type Targets = [(PortagePackage, PortageVersionSet)];

impl RoundCtx<'_> {
    /// Turn a round's solve into its merge plan
    pub(super) fn shape_plan(
        &self,
        targets: &Targets,
        repairs: &Targets,
        solved: Solved,
    ) -> RoundOutcome {
        let Solved {
            key,
            provider,
            solution,
            package_use,
            applied_reqs,
            matched_flags,
            use_conflicts,
            widened,
        } = solved;
        let mut metrics = ResolveMetrics::default();

        let ceded = provider.solved_use_decisions();
        let package_use = fold_ceded_use(package_use, &ceded, &solution);
        let adapter = self.adapter(&package_use, false, false);
        let policy = adapter.policy();

        let autounmask_candidates =
            repo::find_autounmask_candidates(self.data, provider.dropped_deps(), &policy);
        let widened_candidates = widened_autounmask_candidates(
            widened,
            &solution,
            &provider,
            self.data,
            &policy,
            &autounmask_candidates,
        );
        let (reinstall_cpns, upgrades) = rebuild_sets(&provider);

        let install_order_start = Instant::now();
        let InstallOrderResult {
            order: graph_order,
            edges,
            stats,
        } = provider.install_order_with_stats(&solution);
        // Every selected real package, `virtual/*` included: the trims need the RDEPEND of
        // packages the display then drops as already installed.
        let full_order: Order = graph_order
            .into_iter()
            .filter(|(pkg, _)| !pkg.is_virtual())
            .collect();
        metrics.record("install_order", install_order_start.elapsed());
        metrics.record("dependency_graph", stats.graph_build_time);

        let order = self.needing_merge(&full_order, &provider, &reinstall_cpns, &upgrades);
        let (order, exclude_omitted, resume_omitted) = apply_order_filters(
            order,
            &provider.reinstall_deps(),
            self.exclude_atoms,
            self.resume_completed,
            self.host_config_stage && self.cross.is_cross_arch(),
        );
        let solve_cpns: HashSet<Cpn> = self
            .root_cpns
            .iter()
            .copied()
            .chain(targets.iter().chain(repairs).map(|(pkg, _)| *pkg.cpn()))
            .collect();
        let trim_ctx = bdepend_trim::TrimCtx {
            broot_snapshot: self.broot_snapshot,
            data: self.data,
            policy,
            root_cpns: &solve_cpns,
            reinstall_cpns: &reinstall_cpns,
        };
        let order = self.trim(order, &full_order, &trim_ctx, &mut metrics);

        // Solver-internal nodes leave the edges; `virtual/*` packages are real and keep theirs.
        let edges: Vec<DepEdge> = edges
            .into_iter()
            .filter(|e| !e.from.0.is_virtual() && !e.to.0.is_virtual())
            .collect();
        record_graph_stats(&mut metrics, &stats, edges.len());

        let mut order = self.targets_last(order, &edges);
        if self.onlydeps {
            order.retain(|(pkg, _)| !self.root_cpns.contains(pkg.cpn()));
        }
        let slot_op_cpns = self.slot_operator_rebuilds(&mut order);
        let (order, closure_blockers) = self.root_closures(order, &adapter);

        // Decided before anything is printed, so the repair loop can run another round.
        let (proposed, dep_conflicts) = target_conflicts(
            &order,
            &installed_as_solved(self.target_installed, &provider, self.data),
            self.data,
        );

        RoundOutcome {
            provider,
            provider_key: key,
            solution,
            order,
            edges,
            closure_blockers,
            package_use,
            applied_reqs,
            matched_flags,
            use_conflicts,
            ceded,
            autounmask_candidates,
            widened_autounmask_candidates: widened_candidates,
            slot_op_cpns,
            dep_conflicts,
            proposed,
            exclude_omitted,
            resume_omitted,
            round_metrics: metrics,
        }
    }

    /// The selected packages that need merging, at their upgrade version when one was recorded
    ///
    /// Already installed means installed on that package's own merge root. Same-version USE
    /// rebuilds stay, and so do root targets (emerge reinstalls them as `[R]`) unless selective.
    fn needing_merge(
        &self,
        full_order: &[(PortagePackage, Version)],
        provider: &PortageDependencyProvider,
        reinstall_cpns: &HashSet<Cpn>,
        upgrades: &HashMap<Cpn, Version>,
    ) -> Order {
        full_order
            .iter()
            .filter(|(pkg, ver)| {
                let cpv = Cpv::new(*pkg.cpn(), ver.clone());
                let already_installed = match pkg.merge_root() {
                    MergeRoot::Host => self.host_installed_cpvs.contains(&cpv),
                    MergeRoot::Base => self.base_installed_cpvs.contains(&cpv),
                    MergeRoot::Target => {
                        self.target_installed_cpvs.contains(&cpv)
                            || self.provided_cpvs.contains(&cpv)
                    }
                };
                let use_rebuild = provider
                    .installed_policy(pkg)
                    .is_some_and(|p| matches!(p, InstalledPolicy::Rebuild));
                // A target matches by slot: a sibling slot pulled in as a dep is not re-listed.
                let root_target = !self.selective
                    && self
                        .root_pkgs
                        .iter()
                        .any(|r| r.cpn() == pkg.cpn() && r.slot() == pkg.slot());
                !already_installed
                    || reinstall_cpns.contains(pkg.cpn())
                    || use_rebuild
                    || root_target
                    || self.emptytree_native
            })
            .map(|(pkg, ver)| {
                let ver = upgrades
                    .get(pkg.cpn())
                    .cloned()
                    .unwrap_or_else(|| ver.clone());
                (pkg.clone(), ver)
            })
            .collect()
    }

    /// Drop DEPEND the build sysroot already satisfies, and BDEPEND the build host does
    fn trim(
        &self,
        mut order: Order,
        full_order: &[(PortagePackage, Version)],
        trim_ctx: &bdepend_trim::TrimCtx<'_>,
        metrics: &mut ResolveMetrics,
    ) -> Order {
        if self.host_config_stage {
            // With no distinct build sysroot (`--root`, where SYSROOT is ROOT) this trims nothing.
            let before = order.len();
            let start = Instant::now();
            order = depend_trim::trim_sysroot_satisfied_depend_with_cache(
                order,
                self.roots
                    .build_sysroot()
                    .or(Some(self.cross.target.as_path())),
                self.cross.target.as_path(),
                trim_ctx,
                self.vdb_snapshots,
            );
            metrics.depend_trimmed += before.saturating_sub(order.len());
            metrics.record("depend_trim", start.elapsed());
        }
        // A deep `-u`/`-N`/`-U` walks build tools on purpose, and `--emptytree` lists the whole closure.
        let walks_build_deps = self.deep && (self.update || self.use_reinstall_mode.is_some());
        if !self.emptytree_native && !walks_build_deps {
            let before = order.len();
            let start = Instant::now();
            order = bdepend_trim::trim_within_run_bdepend(order, full_order, true, trim_ctx);
            metrics.bdepend_trimmed += before.saturating_sub(order.len());
            metrics.record("bdepend_trim", start.elapsed());
        }
        order
    }

    /// Move root targets nothing else depends on to the end, as emerge lists them
    fn targets_last(&self, order: Order, edges: &[DepEdge]) -> Order {
        let depended_upon: HashSet<Cpn> = edges.iter().map(|e| *e.to.0.cpn()).collect();
        let (targets, mut rest): (Order, Order) = order.into_iter().partition(|(pkg, _)| {
            self.root_cpns.contains(pkg.cpn()) && !depended_upon.contains(pkg.cpn())
        });
        rest.extend(targets);
        rest
    }

    /// Pull in installed `:=` consumers whose recorded subslot a planned package invalidates
    ///
    /// Each lands right after its trigger, as emerge's `__auto_slot_operator_replace_installed__`
    /// set. Returns both ends, which display the forced-rebuild marker.
    fn slot_operator_rebuilds(&self, order: &mut Order) -> HashSet<Cpn> {
        let mut marked = HashSet::new();
        if self.empty {
            return marked;
        }
        let mut planned_slots: HashMap<Cpn, Vec<(Version, portage_atom::Slot)>> = HashMap::new();
        for (pkg, ver) in order.iter() {
            if let Some(cache) = repo::find_cache(self.data, pkg, ver) {
                planned_slots
                    .entry(*pkg.cpn())
                    .or_default()
                    .push((ver.clone(), cache.metadata.slot));
            }
        }
        let in_plan: HashSet<Cpn> = order.iter().map(|(pkg, _)| *pkg.cpn()).collect();
        for rb in subslot::find_rebuilds(self.target_installed, &planned_slots, &in_plan) {
            let pos = order
                .iter()
                .rposition(|(pkg, _)| rb.triggers.contains(pkg.cpn()))
                .map_or(order.len(), |i| i + 1);
            let pkg = match rb.slot.as_deref().filter(|s| !s.is_empty()) {
                Some(s) => PortagePackage::slotted(rb.cpn, Interned::intern(s)),
                None => PortagePackage::unslotted(rb.cpn),
            };
            let ver = best_rebuild_version(self.data, self.target_policy, &rb, &planned_slots);
            order.insert(pos, (pkg, ver));
            marked.insert(rb.cpn);
            marked.extend(rb.triggers.iter().copied());
        }
        marked
    }

    /// Add the build edges the host lacks (to BROOT) and the toolchain sysroot's DEPEND providers
    ///
    /// Returns the order and the scheduling-only blockers between the added entries.
    fn root_closures(
        &self,
        order: Order,
        adapter: &repo::Adapter<'_>,
    ) -> (Order, Vec<(PortagePackage, PortagePackage)>) {
        let host_plan =
            root_closure::host_with_snapshot(&order, adapter, self.cross, self.broot_snapshot);
        let base_plan = root_closure::base_with_cache(
            &host_plan.order,
            adapter,
            self.roots,
            self.vdb_snapshots,
        );
        let mut blockers = host_plan.blockers;
        blockers.extend(base_plan.blockers);
        (base_plan.order, blockers)
    }
}

/// `package.use` plus the values the solver chose for ceded `REQUIRED_USE` flags
fn fold_ceded_use(
    package_use: Vec<(Dep, Vec<UseOverride>)>,
    ceded: &[CededFlag],
    solution: &pubgrub::SelectedDependencies<PortagePackage, Version>,
) -> Vec<(Dep, Vec<UseOverride>)> {
    if ceded.is_empty() {
        return package_use;
    }
    let mut by_cpn: HashMap<Cpn, Vec<&CededFlag>> = HashMap::new();
    for c in ceded {
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
}

/// Packages rebuilt at their installed version for a USE change, and the newer version a forced
/// rebuild takes instead when the tree has one
fn rebuild_sets(provider: &PortageDependencyProvider) -> (HashSet<Cpn>, HashMap<Cpn, Version>) {
    let reinstall_deps = provider.reinstall_deps();
    let reinstall = reinstall_deps.iter().map(|r| *r.package.cpn()).collect();
    let upgrades = reinstall_deps
        .iter()
        .filter_map(|r| r.upgrade_to.as_ref().map(|v| (*r.package.cpn(), v.clone())))
        .collect();
    (reinstall, upgrades)
}

fn record_graph_stats(metrics: &mut ResolveMetrics, stats: &GraphStats, edges: usize) {
    metrics.graph_builds += stats.graph_builds;
    metrics.graph_nodes += stats.graph_nodes;
    metrics.virtual_expansions += stats.virtual_expansions;
    metrics.graph_edges += edges;
    metrics.scc_count += stats.scc_count;
    metrics.largest_scc = metrics.largest_scc.max(stats.largest_scc);
    metrics.repair_reachability_calls += stats.repair_reachability_calls;
    metrics.repair_reachability_nodes += stats.repair_reachability_nodes;
}
