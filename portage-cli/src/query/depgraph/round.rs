//! One solve-and-plan round of [`super::depgraph`].

mod plan;
mod solve;

use std::collections::HashSet;
use std::sync::Arc;

use portage_atom::{Cpn, Cpv, Dep, Version};
use portage_atom_pubgrub::{
    CededFlag, DepEdge, MergeRoot, PortageDependencyProvider, PortagePackage, PortageVersionSet,
    SlotMap, UseFlagRequirement, UseOverride,
};
use portage_resolve::{conflicts, installed, repo, root_aware};

use super::ResolveMetrics;
use super::provider_cache::{ProviderCache, ProviderKey};

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
    pub(super) root_use_deps: &'a [super::RootUseDeps],
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
    pub(super) providers: &'a ProviderCache,
}

/// One solve round's provider, solution, and filtered install order.
///
/// The `--complete-graph` repair loop re-runs this and keeps the last good
/// attempt. Nothing is printed until the caller settles on a round.
pub(super) struct RoundOutcome {
    pub(super) provider: PortageDependencyProvider,
    pub(super) provider_key: ProviderKey,
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

impl RoundCtx<'_> {
    /// A repository view under this round's policy and `package_use`
    fn adapter<'b>(
        &'b self,
        package_use: &'b [(Dep, Vec<UseOverride>)],
        autosolve_use: bool,
        widened: bool,
    ) -> repo::Adapter<'b> {
        repo::Adapter {
            data: self.data,
            accept_keywords: self.accept_keywords,
            package_mask: self.package_mask,
            package_unmask: self.package_unmask,
            accept_licenses: self.accept_licenses,
            accept_properties: self.accept_properties,
            accept_restrict: self.accept_restrict,
            defaults: self.defaults,
            conf: self.conf,
            env_use: self.env_use,
            package_use,
            profile_package_use: self.profile_package_use,
            force_mask: self.force_mask,
            facts: Some(self.policy_facts.generation_for(package_use)),
            installed_cpvs: self.solver_installed_cpvs,
            rebuilding_cpvs: self.rebuilding_installed_cpvs,
            autosolve_use,
            autounmask_widen: widened,
        }
    }
}

/// Solve `targets`, plus the installed packages in `repairs` the plan must keep satisfied,
/// and return this round's merge plan
pub(super) fn solve_round(
    ctx: &RoundCtx<'_>,
    targets: Vec<(PortagePackage, PortageVersionSet)>,
    repairs: &[(PortagePackage, PortageVersionSet)],
) -> anyhow::Result<RoundOutcome> {
    let solved = ctx.solve(&targets, repairs)?;
    Ok(ctx.shape_plan(&targets, repairs, solved))
}
