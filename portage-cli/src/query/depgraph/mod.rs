mod assemble;
mod autounmask;
mod prepare;
mod round;

#[cfg(test)]
mod tests;

pub use portage_atom_pubgrub::MergeRoot;
pub(crate) mod output;
mod package_use;
mod targets;

pub use targets::{TargetAtom, TargetOrigin};

use portage_resolve::{
    conflicts, effective_use, installed, repo, required_use, root_aware, subslot, use_env,
};
// Not referenced directly here (only via a `force_mask::ForceMask` value
// returned from `use_env`), but `c7.rs`/`root_closure.rs`'s own tests still
// reach it through `super::force_mask`/`super::super::force_mask` — keep the
// binding alive for them.
#[allow(unused_imports)]
use portage_resolve::force_mask;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use camino::Utf8Path;
use gentoo_core::Arch;
use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, Dep, Operator, Version};
use portage_atom_pubgrub::{
    PortageDependencyProvider, PortagePackage, PortageVersionSet, UseFlagRequirement, UseOverride,
};

use crate::cli::DepgraphFormat;

/// One entry of the resolved merge list, in install order — everything the
/// build loop needs to emerge it.
pub struct PlannedMerge {
    /// Where this package is merged (`BROOT` host vs target `ROOT`)
    pub merge_root: MergeRoot,
    /// The identity to build/register under (display + work-dir naming +
    /// VDB category) — for a cross-derived package this is the *virtual*
    /// cpv (`cross-<tuple>/gcc-...`), which may differ from the real cpn
    /// `ebuild_path` was resolved through. Kept as a real `Cpv`, not a
    /// formatted string, so nothing downstream has to re-derive it by
    /// parsing a path or string.
    pub cpv: Cpv,
    /// Absolute path to the ebuild
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
    /// This cpv is already installed yet the resolver kept it in the plan — an
    /// explicitly-requested target (emerge reinstalls these by default) or a
    /// same-version USE rebuild. The merge loop must build it rather than treat
    /// the VDB entry as a resume-skip.
    pub reinstall: bool,
}

#[derive(Default)]
struct ResolveMetrics {
    phases: HashMap<&'static str, Duration>,
    solve_rounds: usize,
    graph_builds: usize,
    graph_nodes: usize,
    virtual_expansions: usize,
    graph_edges: usize,
    scc_count: usize,
    largest_scc: usize,
    repair_reachability_calls: usize,
    repair_reachability_nodes: usize,
    depend_trimmed: usize,
    bdepend_trimmed: usize,
}

impl ResolveMetrics {
    fn record(&mut self, phase: &'static str, elapsed: Duration) {
        *self.phases.entry(phase).or_default() += elapsed;
    }

    fn merge(&mut self, other: &Self) {
        for (phase, elapsed) in &other.phases {
            *self.phases.entry(*phase).or_default() += *elapsed;
        }
        self.graph_builds += other.graph_builds;
        self.graph_nodes += other.graph_nodes;
        self.virtual_expansions += other.virtual_expansions;
        self.graph_edges += other.graph_edges;
        self.scc_count += other.scc_count;
        self.largest_scc = self.largest_scc.max(other.largest_scc);
        self.repair_reachability_calls += other.repair_reachability_calls;
        self.repair_reachability_nodes += other.repair_reachability_nodes;
        self.depend_trimmed += other.depend_trimmed;
        self.bdepend_trimmed += other.bdepend_trimmed;
    }

    fn log(&self, resolve_secs: f64) {
        tracing::debug!(
            resolve_secs,
            solve_rounds = self.solve_rounds,
            graph_builds = self.graph_builds,
            graph_nodes = self.graph_nodes,
            virtual_expansions = self.virtual_expansions,
            graph_edges = self.graph_edges,
            scc_count = self.scc_count,
            largest_scc = self.largest_scc,
            repair_reachability_calls = self.repair_reachability_calls,
            repair_reachability_nodes = self.repair_reachability_nodes,
            depend_trimmed = self.depend_trimmed,
            bdepend_trimmed = self.bdepend_trimmed,
            phases = ?self.phases,
            "resolve phase timings"
        );
    }
}

/// What [`depgraph`] resolved
pub struct DepgraphOutcome {
    /// Process exit code: `1` when the displayed plan is not directly
    /// installable (USE/mask/license changes, or a PMS 8.3.2 hard blocker
    /// conflict), matching `emerge -p`. `0` otherwise.
    pub exit_code: i32,
    /// The merge list in install order
    pub plan: Vec<PlannedMerge>,
    /// For each `plan` entry, the indices of earlier entries that must finish
    /// building before it can build — in-plan `DEPEND`/`BDEPEND` **and**
    /// `RDEPEND` edges. RDEPEND is included because Gentoo `virtual/*`
    /// packages put real providers only in RDEPEND: e.g. `sed[acl]` DEPEND
    /// on `virtual/acl`, which RDEPEND on `sys-apps/acl`. Blocking only on
    /// the virtual lets `--jobs` start sed's configure while acl still builds.
    ///
    /// Restricted to earlier indices, so it is always acyclic
    /// (`install_order` already linearised soft RDEPEND cycles). The
    /// `--jobs` scheduler uses this to parallelise builds while respecting
    /// order. Empty entry ⇒ no in-plan deps that constrain start.
    pub build_blockers: Vec<Vec<usize>>,
    /// `(dependent, dependency)` pairs where a hard (`DEPEND`/`BDEPEND`) edge
    /// is scheduled backwards — proof of a genuine irreducible dependency
    /// cycle, not a solver bug. Lets a pre-flight failure name the cycle
    /// instead of just "needs: X".
    pub hard_cycle_edges: Vec<(Cpv, Cpv)>,
    /// `package.provided` CPVs the system supplies, each with the repo slot it
    /// maps onto (derived from the version's slot series). The pre-flight build
    /// check seeds these as present so a build dep on an externally-provided
    /// package (e.g. the host interpreter, `dev-lang/python:3.14`) is not
    /// reported missing — the solver already treats it as satisfied.
    pub provided: Vec<(Cpv, Option<String>)>,
    /// Raw BROOT/prefix rows retained until the real-merge preflight check consumes them.
    pub(crate) broot_snapshot: Option<installed::BrootSnapshot>,
    /// VDB root snapshots shared by resolver, closure, and preflight adapters.
    pub(crate) vdb_snapshots: Option<Arc<installed::VdbSnapshotCache>>,
    /// Installed packages the merge will unmerge to satisfy blockers (PMS 8.3.2).
    /// Strong `!!` entries run before the merge loop; weak `!` after.
    pub unmerges: Vec<portage_resolve::conflicts::PlannedUnmerge>,
    /// Installed packages the plan leaves in place although policy masks them,
    /// in the order they were reported. The same list is printed before `Total:`.
    #[cfg(test)]
    pub masked_installed: Vec<(Cpv, String)>,
}

/// Whether pending autounmask changes (keyword/mask/license/…) may be
/// persisted to disk
///
/// Invocation-mode-gated, not flag-gated: `-p` is [`Self::Never`], `-a` is
/// [`Self::Ask`] (write on confirm), a real merge is [`Self::Always`] — the
/// same policy for every subcommand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutounmaskPersist {
    /// Report only; never touch disk (`-p`, read-only queries)
    Never,
    /// Preview + confirm prompt; write only on yes (`-a`)
    Ask,
    /// Persist unconditionally (real merge run)
    Always,
}

pub struct DepgraphOpts<'a> {
    /// The full priority-ordered repo set for this invocation: `main` plus every `repos.conf`
    /// overlay
    ///
    /// Built **once** by the caller and shared with the atom-resolution step that runs *before*
    /// `depgraph()` — so the solver builds its plan against the exact same repo world
    /// `resolve_atom` picked atoms from.
    ///
    /// Previously this function took a bare `repo_path` and rebuilt the set
    /// internally, which double-opened every repo per merge and could
    /// diverge from the caller's set when an overlay's open failed
    /// transiently in one build and succeeded in the other.
    ///
    /// Caller-supplied aliases (e.g. `crossdev --setup -p`'s in-memory target)
    /// must already be prepended onto `set` before it is passed in.
    pub set: portage_repo::RepoSet,
    /// The root targets, each carrying the provenance that decides whether an
    /// unsatisfiable one aborts the run or just warns (see [`TargetOrigin`]).
    pub atoms: &'a [TargetAtom],
    /// The atoms this invocation would record in the world file — real
    /// emerge's `favorites` ∩ `create_world_atom`, gated on `--oneshot`
    /// *only*. Bolds those rows on top of the ones already in `@selected`:
    /// a plain `em -p newpkg` bolds `newpkg` because dropping the `-p`
    /// would add it to world, while `em -1p newpkg` leaves it plain.
    ///
    /// Notably *not* gated on `--pretend` (nor `--buildpkgonly`/`--fetchonly`/
    /// `--onlydeps`, which only suppress the on-disk write): a preview must
    /// show what the real run would look like. Empty for callers that never
    /// touch the world file at all (`equery depgraph`, the internal
    /// `crossdev` gcc-version probe) — same rendering as `--oneshot`.
    pub world_additions: &'a [Dep],
    pub arch: &'a Arch,
    pub format: DepgraphFormat,
    /// `-v`/`-vv`: `>= 1` adds the `:slot/subslot::repo` suffix, download
    /// size, and full (not just changed) USE flags to each `-p`/`--tree` row;
    /// `>= 2` additionally tints every root target (`PrettyCtx::requested`)
    /// purple. Distinct from the top-level `-v`/`-vv` build-phase logging
    /// verbosity (`Cli::verbose`, see `diag.rs`) — same counter, different
    /// consumer.
    pub verbose: u8,
    pub empty: bool,
    pub autounmask_write: bool,
    /// Whether pending autounmask changes (keyword/mask/license/…) may be
    /// persisted to disk, independent of invocation mode: `-p` never writes,
    /// `-a` writes only after the confirm prompt says yes, a real merge
    /// always writes. Same policy for every subcommand — the caller just
    /// translates its own invocation mode.
    pub autounmask_persist: AutounmaskPersist,
    /// `--ask`, already gated by the caller so it's only `true` for an
    /// interactive real merge (never `--pretend`, never a read-only query
    /// command). When USE changes are required and `--autounmask-write`
    /// wasn't also given, this prompts to write them to `package.use`
    /// instead — a deliberate divergence from real emerge, which only
    /// offers that non-interactively via `--autounmask-write`.
    pub ask: bool,
    pub autosolve_use: bool,
    /// Widened candidate supply (`--autounmask`-style): masked/unkeyworded
    /// versions stay visible to the solver, tagged and strictly tiered below
    /// accepted ones. Two-phase internally — the solve first runs strict and
    /// only retries widened when phase 1 fails — so a `false` here is
    /// byte-identical to strict filtering.
    ///
    /// Set by crossdev flows, whose job is pushing past upstream keywording
    /// gaps; deliberately not wired to the user-facing `--autounmask` flag.
    pub autounmask_widen: bool,
    /// The resolved root set (config / base / target / BROOT)
    ///
    /// See docs/user/root-model.md. `roots.satisfaction_root(DepClass::Bdepend)` answers the
    /// Host-routed BDEPEND/IDEPEND question directly — `roots` carries BROOT correctly even
    /// under an active `--target` sysroot substitution, so a separate `host_roots` field is no
    /// longer needed (see `Cli::roots`'s doc comment).
    pub roots: &'a portage_resolve::Roots,
    /// Where a `MergeRoot::Host` plan entry actually merges — `Cli::host_roots()`'s
    /// `merge_root()`
    ///
    /// Passed separately from `roots` because `roots` can be `--target`-substituted (its
    /// `eprefix`/`is_overlay()` cleared), which would make the `-p` display fall back to the
    /// real host even under an unprivileged `--prefix` overlay; `Cli::host_roots()` is computed
    /// from `base_roots()` and stays overlay-aware regardless of `--target`.
    pub host_merge_root: &'a Utf8Path,
    /// `--onlydeps`: drop the explicitly-requested targets from the plan,
    /// keeping only their dependencies (emerge's `--onlydeps`).
    pub onlydeps: bool,
    /// Include BDEPEND in resolution (emerge's `--with-bdeps`)
    ///
    /// Default false (exclude BDEPEND) to match emerge's default.
    pub with_bdeps: bool,
    /// emerge's `--root-deps[=rdeps]`: only RDEPEND (not DEPEND) is required to be satisfiable
    /// in the merge target
    ///
    /// Caller-supplied rather than auto-derived from cross-arch detection: it's a property of
    /// *which operation* is running (`crossdev --setup` bootstrapping a still-empty target
    /// always needs it; `stages --stage1` against a working toolchain should not), not of
    /// CHOST/CBUILD alone.
    pub root_deps_rdeps: bool,
    /// `--deep`: re-examine transitive deps
    ///
    /// With [`Self::update`], enables in-slot upgrades for packages in the graph (emerge
    /// `-uD`). Alone, bumps `:*` any-slot deps to the newest slot rather than keeping a
    /// satisfying installed slot.
    pub deep: bool,
    /// `--update`: prefer newest accepted versions
    ///
    /// Combined with [`Self::deep`] for transitive in-slot upgrades; alone only affects atom
    /// disambiguation at the CLI and root-target selection (roots already take best in-slot).
    pub update: bool,
    /// `--newuse` / `-N`: rebuild installed packages in the graph when planned
    /// USE or IUSE differs from the VDB.
    pub newuse: bool,
    /// `--changed-use` / `-U`: like `newuse` but only for enabled-flag flips
    /// among shared IUSE (ignore pure IUSE add/drop).
    pub changed_use: bool,
    /// `--noreplace` / `-n`: leave a named target alone when an installed
    /// version already satisfies it, rather than reinstalling it.
    pub noreplace: bool,
    /// `--nodeps` (emerge `-O`): merge only the named atoms, no dependency expansion
    ///
    /// Used by the staged toolchain bootstrap.
    pub nodeps: bool,
    /// A transient conf-layer USE override for this resolve, e.g. `em stages
    /// --stage1`'s `USE="-* build ${BOOTSTRAP_USE}"` (catalyst's own
    /// recipe). Folded at the conf layer (after real `make.conf`, before
    /// `package.use`/env) — NOT the process environment, which would sit
    /// above `package.use` and incorrectly wipe it. See
    /// `resolve_use_flags`'s `extra_use_override` doc.
    pub extra_use_override: Option<&'a str>,
    /// In-memory `package.use` lines for this resolve only, never written
    ///
    /// Appended after the on-disk host/overlay files so they win. Empty for
    /// every ordinary emerge; `em toolchain --setup` uses this for
    /// `util-linux -pam -su`.
    pub extra_package_use: &'a [(Dep, Vec<UseOverride>)],
    /// In-memory config for a `--target` sysroot whose on-disk `make.conf`/
    /// `make.profile` haven't been written yet (e.g. `em crossdev --setup
    /// -p` on a never-initialized target) — see
    /// [`portage_resolve::use_env::SysrootOverride`]. `None` for every
    /// ordinary resolve, including a `--target` sysroot that already exists.
    pub sysroot_override: Option<&'a use_env::SysrootOverride<'a>>,
    /// See `output::PrettyCtx::binpkg_index`'s doc — passed straight through
    /// to the `Pretty` printer so `-p` can show `[binary ...]`.
    pub binpkg_index: Option<&'a portage_binpkg::BinpkgIndex>,
    /// `-X`/`--exclude`: package atoms to never install (emerge's own
    /// wording — "won't install any ebuild or binary package that matches
    /// any of the given atoms"). Applied as a post-solve filter on `order`,
    /// before *any* consumer — the `-p`/`--tree`/`--json` preview and the
    /// final `PlannedMerge` merge-loop plan alike — so every display and
    /// the actual merge agree.
    ///
    /// Not integrated into the pubgrub solve itself: a deliberate
    /// simplification — if an excluded package is a genuine hard dependency
    /// of something else still in the plan, that other package's own
    /// preflight/build fails with a clear missing-dependency error rather
    /// than the solver reporting the conflict up front.
    pub exclude: &'a [String],
    /// Packages already finished in a prior attempt of a `-r`/`--resume` job
    /// (`maint::resume::completed_keys`)
    ///
    /// Dropped from `order` the same way `--exclude` is — so `-p` and the merge plan omit
    /// completed work, which is required for correct `--emptytree` resume (VDB presence is not
    /// a completion marker there). Empty for every non-resume call.
    pub resume_completed: HashSet<(MergeRoot, String)>,
    /// `--complete-graph`: when a deep update (`-uD`) moves a `~`-pinned
    /// family but leaves a retained installed dependent behind (whose pin
    /// the move now breaks), pull that dependent into the plan too rather
    /// than stopping the chain halfway. Gated behind an explicit flag
    /// because there is no emerge parity to validate against, and the
    /// policy can revert an upgrade a dependent has no satisfying version for.
    pub complete_graph: bool,
    /// Suppress the Pretty/JSON/Tree plan preview — for an internal probe
    /// that only wants a field off [`DepgraphOutcome`] (e.g.
    /// `resolve_gcc_version`'s single-atom `sys-devel/gcc` resolve), not a
    /// user-facing display. Advisory reports (conflicts, autounmask, …)
    /// are unaffected — pass `nodeps`/`autounmask_persist: Never`/
    /// `ask: false` too so those paths have nothing to report.
    pub quiet: bool,
}

/// The settled solve: last good round, plus `--complete-graph` repair notes.
struct Resolved {
    outcome: round::RoundOutcome,
    repair_completed: Vec<Cpn>,
    repair_incomplete: Vec<Cpn>,
    resolve_secs: f64,
}

pub async fn depgraph(opts: DepgraphOpts<'_>) -> anyhow::Result<DepgraphOutcome> {
    let mut metrics = ResolveMetrics::default();
    let prepared = prepare::prepare(opts, &mut metrics).await?;
    let resolved = resolve(&prepared, &mut metrics)?;
    assemble::assemble(prepared, resolved, &metrics)
}

/// Solve the classified roots, then repair retained-owner conflicts when
/// `--complete-graph` is set.
fn resolve(
    prepared: &prepare::Prepared<'_>,
    metrics: &mut ResolveMetrics,
) -> anyhow::Result<Resolved> {
    // Same `package_use` prepare already cached, so this keeps the slot map's
    // policy-fact generation.
    let target_policy = prepared
        .resolved
        .as_policy()
        .with_package_use(&prepared.policy_facts, &prepared.resolved.package_use);
    let empty_solver_cpvs = HashSet::new();
    let solver_installed_cpvs = if prepared.emptytree_native {
        &empty_solver_cpvs
    } else {
        &prepared.target_installed_cpvs
    };
    let round_ctx = round::RoundCtx {
        data: &prepared.data,
        accept_keywords: target_policy.accept_keywords,
        package_mask: target_policy.package_mask,
        package_unmask: target_policy.package_unmask,
        accept_licenses: target_policy.accept_licenses,
        accept_properties: target_policy.accept_properties,
        accept_restrict: target_policy.accept_restrict,
        defaults: target_policy.defaults,
        conf: target_policy.conf,
        env_use: target_policy.env_use,
        profile_package_use: target_policy.profile_package_use,
        force_mask: target_policy.force_mask,
        policy_facts: &prepared.policy_facts,
        solver_installed_cpvs,
        rebuilding_installed_cpvs: &prepared.rebuilding_installed_cpvs,
        slot_map: &prepared.slot_map,
        widened_slot_map: &prepared.widened_slot_map,
        target_policy: &target_policy,
        package_use: &prepared.resolved.package_use,
        target_installed: &prepared.target_installed,
        installed_blockers: &prepared.installed_blockers,
        sysroot_installed: &prepared.sysroot_installed,
        host_installed: &prepared.host_installed,
        provided: &prepared.provided,
        provided_avail: &prepared.provided_avail,
        root_pkgs: &prepared.root_pkgs,
        root_cpns: &prepared.root_cpns,
        exclude_atoms: &prepared.exclude_atoms,
        resume_completed: &prepared.resume_completed,
        cross: &prepared.cross,
        roots: prepared.roots,
        broot_snapshot: &prepared.broot_snapshot,
        vdb_snapshots: &prepared.vdb_snapshots,
        host_installed_cpvs: &prepared.host_installed_cpvs,
        base_installed_cpvs: &prepared.base_installed_cpvs,
        target_installed_cpvs: &prepared.target_installed_cpvs,
        provided_cpvs: &prepared.provided_cpvs,
        emptytree_native: prepared.emptytree_native,
        solve_with_bdeps: prepared.solve_with_bdeps,
        empty: prepared.empty,
        selective: prepared.selective,
        update: prepared.update,
        deep: prepared.deep,
        nodeps: prepared.nodeps,
        root_deps_rdeps: prepared.root_deps_rdeps,
        onlydeps: prepared.onlydeps,
        autosolve_use: prepared.autosolve_use,
        host_config_stage: prepared.host_config_stage,
        autounmask_widen: prepared.autounmask_widen,
        use_reinstall_mode: prepared.use_reinstall_mode,
    };
    let root_deps = &prepared.root_deps;
    let complete_graph = prepared.complete_graph;
    let update = prepared.update;
    let empty = prepared.empty;
    // Time the initial solve plus any `--complete-graph` repair rounds.
    // The plan preview prints this where emerge prints its resolution time.
    let resolve_start = std::time::Instant::now();
    let mut solve_targets: Vec<(PortagePackage, PortageVersionSet)> = root_deps.clone();
    let mut repaired: HashSet<Cpn> = HashSet::new();
    metrics.solve_rounds += 1;
    let mut outcome = round::solve_round(&round_ctx, solve_targets.clone())?;
    metrics.merge(&outcome.round_metrics);
    let mut repair_completed: Vec<Cpn> = Vec::new();
    let mut repair_incomplete: Vec<Cpn> = Vec::new();
    if complete_graph && update && !empty {
        const MAX_REPAIR_ROUNDS: usize = 3;
        for _ in 0..MAX_REPAIR_ROUNDS {
            // Retained-owner conflicts (`owner_replaced_by.is_none()`) are the
            // ones genuine chain repair can fix: an installed package whose
            // pin the plan just broke, not itself replaced this run. Each one
            // is re-targeted as a root dep by its own cpn/slot so the solver
            // picks a version compatible with the rest of the plan.
            let candidates: Vec<(PortagePackage, PortageVersionSet)> =
                conflicts::retained_owners(&outcome.dep_conflicts)
                    .filter(|c| repaired.insert(c.installed_cpn))
                    .map(|c| {
                        let pkg = match c.slot {
                            Some(s) => PortagePackage::slotted(c.installed_cpn, s),
                            None => PortagePackage::unslotted(c.installed_cpn),
                        };
                        (pkg, PortageVersionSet::any())
                    })
                    .collect();
            if candidates.is_empty() {
                break;
            }
            let mut next_targets = solve_targets.clone();
            next_targets.extend(candidates.iter().cloned());
            metrics.solve_rounds += 1;
            match round::solve_round(&round_ctx, next_targets.clone()) {
                Ok(next) => {
                    metrics.merge(&next.round_metrics);
                    repair_completed.extend(candidates.iter().map(|(pkg, _)| *pkg.cpn()));
                    solve_targets = next_targets;
                    outcome = next;
                }
                Err(_) => {
                    // Discard this round; keep the last good outcome rather than
                    // turning an advisory chain-completion attempt into a hard
                    // resolution failure.
                    repair_incomplete.extend(candidates.iter().map(|(pkg, _)| *pkg.cpn()));
                    break;
                }
            }
        }
    }
    let resolve_secs = resolve_start.elapsed().as_secs_f64();
    Ok(Resolved {
        outcome,
        repair_completed,
        repair_incomplete,
        resolve_secs,
    })
}

/// Narrow the round's autounmask candidates to the ones still actionable.
///
/// A candidate survives when the solve did not already satisfy its CPN and the
/// plan still needs it. A widened selection then supersedes any exact-pin
/// advisory for the same cpn+slot: the bounded grant replaces the
/// everything-grant set, and keeping both would write two conflicting shapes for
/// one package. Keyed on slot too — a widened `clang:21` must not suppress a
/// real dropped-dep pin for `clang:16`; the two slots are independent packages.
fn actionable_autounmask_candidates(
    candidates: Vec<repo::AutounmaskCandidate>,
    widened: &[repo::AutounmaskCandidate],
    solution: &pubgrub::SelectedDependencies<PortagePackage, Version>,
    order: &[(PortagePackage, Version)],
    data: &repo::RepoData,
) -> Vec<repo::AutounmaskCandidate> {
    let solution_cpns: HashSet<Cpn> = solution
        .iter()
        .filter(|(p, _)| !p.is_virtual())
        .map(|(p, _)| *p.cpn())
        .collect();
    let new_needed_cpns: HashSet<Cpn> = order
        .iter()
        .filter(|(pkg, _)| !pkg.is_virtual())
        .flat_map(|(pkg, ver)| repo::cpns_for(data, pkg.cpn(), ver))
        .collect();

    let mut dropped: Vec<_> = candidates
        .into_iter()
        .filter(|c| !solution_cpns.contains(&c.cpv.cpn) && new_needed_cpns.contains(&c.cpv.cpn))
        .collect();

    if !widened.is_empty() {
        let widened_cpn_slots: HashSet<(Cpn, Option<_>)> =
            widened.iter().map(|c| (c.cpv.cpn, c.slot)).collect();
        dropped.retain(|c| !widened_cpn_slots.contains(&(c.cpv.cpn, c.slot)));
    }
    dropped
}

/// The target-routed packages this round proposes, and the reverse-dependency
/// conflicts they would cause against what is installed.
///
/// A complete-graph check that emerge's default targeted `-p` skips (e.g.
/// upgrading docutils past an installed package's `<` bound). Computed during
/// the round, before anything is printed or written, so the `--complete-graph`
/// repair loop can decide whether another round is needed.
///
/// Target-routed entries only: `order` also carries BROOT build entries from
/// [`root_closure::host`], and those install into the host, not the VDB
/// `target_installed` was read from. Counting one as replacing a target package
/// would hide a real conflict on that name.
fn target_conflicts(
    order: &[(PortagePackage, Version)],
    target_installed: &[installed::VdbEntry],
) -> (Vec<conflicts::ProposedPkg>, Vec<conflicts::Conflict>) {
    let proposed: Vec<conflicts::ProposedPkg> = order
        .iter()
        .filter(|(pkg, _)| !pkg.is_virtual() && pkg.merge_root() == MergeRoot::Target)
        .map(|(pkg, ver)| conflicts::ProposedPkg {
            cpn: *pkg.cpn(),
            slot: pkg.slot(),
            version: ver.clone(),
        })
        .collect();
    let conflicts = conflicts::find_conflicts(target_installed, &proposed);
    (proposed, conflicts)
}

/// Re-append reinstalls the solver never routed through `install_order`, then
/// drop what the invocation asked to skip. Returns the filtered order and how
/// many entries `--exclude` and `--resume` each removed.
///
/// The exclusions run against `order` itself, before any display path
/// (Pretty/JSON/Tree) or the final `PlannedMerge` list is built from it, so
/// every consumer agrees on what will happen — not just the merge loop.
fn apply_order_filters(
    mut order: Vec<(PortagePackage, Version)>,
    reinstall_deps: &[&UseFlagRequirement],
    exclude_atoms: &[Dep],
    resume_completed: &std::collections::HashSet<(MergeRoot, String)>,
    cross_arch_host_stage: bool,
) -> (Vec<(PortagePackage, Version)>, usize, usize) {
    // Fallback: any reinstall the solver didn't route through install_order
    // (rare) is appended so it is not silently dropped.
    let in_order: std::collections::HashSet<Cpn> =
        order.iter().map(|(pkg, _)| *pkg.cpn()).collect();
    let to_reinstall: Vec<(PortagePackage, Version)> = reinstall_deps
        .iter()
        .filter(|r| !in_order.contains(r.package.cpn()))
        .map(|r| {
            let ver = r.upgrade_to.as_ref().unwrap_or(&r.version).clone();
            (r.package.clone(), ver)
        })
        .collect();
    order.extend(to_reinstall);

    // `-X`/`--exclude` (see `DepgraphOpts::exclude`'s doc).
    let mut exclude_omitted = 0usize;
    if !exclude_atoms.is_empty() {
        let before = order.len();
        order.retain(|(pkg, ver)| {
            let cpv = Cpv::new(*pkg.cpn(), ver.clone());
            let slot = pkg.slot().map(portage_atom::Slot::from_name);
            !exclude_atoms
                .iter()
                .any(|d| d.matches_cpv(&cpv, slot.as_ref()))
        });
        // Counted here but printed once after the repair loop settles (an
        // intermediate round's count would otherwise be reported and superseded).
        exclude_omitted = before.saturating_sub(order.len());
    }

    // `-r` completion progress: same post-solve drop, so the preview and merge
    // omit work already finished in a prior attempt.
    let mut resume_omitted = 0usize;
    if !resume_completed.is_empty() {
        let before = order.len();
        order.retain(|(pkg, ver)| {
            let cpv = Cpv::new(*pkg.cpn(), ver.clone()).to_string();
            !resume_completed.contains(&(pkg.merge_root(), cpv))
        });
        resume_omitted = before.saturating_sub(order.len());
    }

    // Cross-arch host-config stage: pretend output lists target ROOT merges only
    // (emerge -p). A native offset instead keeps the Host build-dep merges (the
    // host-side installs needed to build the target packages), matching emerge.
    if cross_arch_host_stage {
        order.retain(|(pkg, _)| pkg.merge_root() == MergeRoot::Target);
    }

    (order, exclude_omitted, resume_omitted)
}

/// Installed versions the solution keeps although policy filters them out,
/// each with the reason, sorted by package.
///
/// A version the plan still merges (same slot and merge root, any version)
/// is not listed. A kept version whose ebuild is gone, or whose slot has no
/// filtered ebuild, is not listed either.
fn masked_installed_kept(
    solution: &pubgrub::SelectedDependencies<PortagePackage, Version>,
    order: &[(PortagePackage, Version)],
    dropped_roots: &[(Cpv, Option<Interned<DefaultInterner>>, String)],
    provider: &PortageDependencyProvider,
    data: &repo::RepoData,
    final_policy: &repo::ResolvePolicy,
) -> Vec<(Cpv, String)> {
    let planned: HashSet<(Cpn, Option<Interned<DefaultInterner>>, MergeRoot)> = order
        .iter()
        .map(|(pkg, _)| (*pkg.cpn(), pkg.slot(), pkg.merge_root()))
        .collect();
    let mut pairs: Vec<(PortagePackage, Version)> = solution
        .iter()
        .filter(|(pkg, ver)| !pkg.is_virtual() && provider.selection_is_installed_only(pkg, ver))
        .filter(|(pkg, _)| !planned.contains(&(*pkg.cpn(), pkg.slot(), pkg.merge_root())))
        .map(|(pkg, ver)| (pkg.clone(), ver.clone()))
        .collect();
    // With no visible version at all the package never enters the solve: a
    // dependency on it is dropped, a root target is left out (`dropped_roots`),
    // and the installed version satisfies either silently. Collect the dropped
    // deps first: `installed_versions` borrows the provider those deps borrow.
    let dropped: Vec<_> = provider.dropped_deps_of(solution).cloned().collect();
    for dep in &dropped {
        if planned.contains(&(
            *dep.package.cpn(),
            dep.package.slot(),
            dep.package.merge_root(),
        )) {
            continue;
        }
        for ver in provider.installed_versions(&dep.package) {
            if dep.version_set.contains(ver) {
                pairs.push((dep.package.clone(), ver.clone()));
            }
        }
    }
    let mut kept: Vec<(Cpv, String)> = pairs
        .into_iter()
        .filter_map(|(pkg, ver)| {
            let only = PortageVersionSet::from_operator(Operator::Equal, false, ver.clone());
            let filtered = repo::filter_reasons_for(data, pkg.cpn(), &only, final_policy)
                .into_iter()
                .find(|c| {
                    c.cpv.version == ver && pkg.slot().is_none_or(|slot| c.slot == Some(slot))
                })?;
            let why = repo::filter_reason_text(&filtered.reasons);
            Some((filtered.cpv, why))
        })
        .chain(dropped_roots.iter().filter_map(|(cpv, slot, why)| {
            if planned.contains(&(cpv.cpn, *slot, MergeRoot::Target)) {
                None
            } else {
                Some((cpv.clone(), why.clone()))
            }
        }))
        .collect();
    kept.sort_by_key(|(cpv, _)| cpv.to_string());
    kept.dedup();
    kept
}

/// Widened phase-2 selections, as autounmask candidates.
///
/// A chosen version *outside* acceptance is the hard-solve-failure case the
/// [`repo::find_autounmask_candidates`] dropped-dep path never sees — the solve
/// failed there instead of dropping the dep gracefully. Each selected tagged cpv
/// becomes a regular candidate through the same reason machinery.
///
/// The result is deliberately kept separate from the dropped-dep candidates
/// until after the actionable-set post-filter, whose
/// `!solution_cpns.contains` guard would otherwise discard every one of these
/// by construction.
fn widened_autounmask_candidates(
    widened: bool,
    solution: &pubgrub::SelectedDependencies<PortagePackage, Version>,
    provider: &PortageDependencyProvider,
    data: &repo::RepoData,
    final_policy: &repo::ResolvePolicy,
    existing: &[repo::AutounmaskCandidate],
) -> Vec<repo::AutounmaskCandidate> {
    if !widened {
        return Vec::new();
    }
    let mut selections: Vec<(PortagePackage, Version)> = solution
        .iter()
        .filter(|(pkg, ver)| !pkg.is_virtual() && provider.selection_needs_unmask(pkg, ver))
        .map(|(pkg, ver)| (pkg.clone(), ver.clone()))
        .collect();
    selections.sort_by_key(|(pkg, _)| pkg.to_string());
    let mut cands = Vec::new();
    for (pkg, ver) in selections {
        let vs = PortageVersionSet::from_operator(Operator::Equal, false, ver.clone());
        let is_live = provider.selection_is_live(&pkg, &ver);
        // Bound persisted slot grants below the slot's lowest live version, so
        // accepting the slot never invites a `.9999` pick on the next resolve
        // (newest-wins applies to accepted candidates, where no tiering runs). A
        // live selection gets an unbounded grant — nothing to protect. Lowest
        // live version in the same slot *above* the selection; anything else
        // would produce a grant excluding the very version being selected.
        let live_upper_bound = match (is_live, pkg.slot()) {
            (false, Some(sel_slot)) => repo::live_upper_bound(
                data.versions
                    .get(pkg.cpn())
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
                &ver,
                Some(&sel_slot),
            )
            .filter(|bound| bound > &ver),
            _ => None,
        };
        cands.extend(
            repo::filter_reasons_for(data, pkg.cpn(), &vs, final_policy)
                .into_iter()
                .map(|mut c| {
                    c.widened = true;
                    c.live_upper_bound = live_upper_bound.clone();
                    c.is_live = is_live;
                    c
                }),
        );
    }
    let mut seen: HashSet<String> = existing.iter().map(|c| c.cpv.to_string()).collect();
    cands.retain(|c| seen.insert(c.cpv.to_string()));
    cands
}

/// Map each `package.provided` CPV onto the repo slot(s) a `:slot` dep would
/// reference, so both the solver's host seed and the pre-flight check treat it
/// as present at that slot.
///
/// A CPV with no matching repo version is recorded slotless (`None`).
fn provided_availability(provided: &[Cpv], data: &repo::RepoData) -> Vec<(Cpv, Option<String>)> {
    provided
        .iter()
        .flat_map(|cpv| {
            let mut slots: Vec<String> = Vec::new();
            if let Some(entries) = data.versions.get(&cpv.cpn) {
                for (rcpv, ce) in entries {
                    if same_slot_series(&rcpv.version, &cpv.version) {
                        let s = ce.metadata.slot.slot.to_string();
                        if !slots.contains(&s) {
                            slots.push(s);
                        }
                    }
                }
            }
            if slots.is_empty() {
                vec![(cpv.clone(), None)]
            } else {
                slots.into_iter().map(|s| (cpv.clone(), Some(s))).collect()
            }
        })
        .collect()
}

/// The requested atoms after classification: what the solver is given, which
/// CPNs were asked for, and what could not be satisfied.
struct RootTargets {
    /// One solver root dep per acceptable atom
    deps: Vec<(PortagePackage, PortageVersionSet)>,
    /// CPNs that became solver roots. Unsatisfiable atoms are not inserted.
    cpns: std::collections::HashSet<Cpn>,
    /// Atoms dropped with a warning, reported after the plan
    unsatisfiable: Vec<output::UnsatisfiableTarget>,
    /// Installed versions that satisfied a silently dropped atom, with why
    /// policy filters them. The slot is the filtered ebuild's slot.
    masked_kept: Vec<(Cpv, Option<Interned<DefaultInterner>>, String)>,
}

/// Classify each requested atom into a solver root dep, a reported
/// unsatisfiable target, or a silent drop.
fn classify_root_targets(
    atoms: &[TargetAtom],
    data: &repo::RepoData,
    target_policy: &repo::ResolvePolicy,
    installed: &HashMap<Cpn, HashMap<Interned<DefaultInterner>, Version>>,
    empty: bool,
    selective: bool,
    multi_repo: bool,
) -> anyhow::Result<RootTargets> {
    let mut deps = Vec::new();
    let mut cpns: std::collections::HashSet<Cpn> = std::collections::HashSet::new();
    // Root targets with no acceptable candidate, dropped from the solve and
    // reported after the plan (world-family provenance only — anything else is
    // fatal below).
    let mut unsatisfiable: Vec<output::UnsatisfiableTarget> = Vec::new();
    let mut masked_kept: Vec<(Cpv, Option<Interned<DefaultInterner>>, String)> = Vec::new();
    for target in atoms {
        let atom = &target.atom;
        let dep = Dep::parse(atom).map_err(|e| anyhow::anyhow!("bad atom '{atom}': {e}"))?;
        let pkg = repo::target_package(data, &dep, target_policy);
        let vs = match &dep.version {
            Some(v) => {
                let op = dep.op.unwrap_or(Operator::GreaterOrEqual);
                PortageVersionSet::from_operator(op, dep.glob, v.clone())
            }
            None => PortageVersionSet::any(),
        };
        // `target_package` hands back an unslotted package when nothing the atom
        // matches survives keyword/mask/license filtering — an identity the
        // provider never registers, so leaving it in `deps` turns into an
        // opaque solver failure. Classify it here instead, the way portage does
        // in argument processing.
        if pkg.slot().is_none() {
            let reasons = repo::filter_reasons_for_atom(data, &dep, &vs, target_policy);
            let problem = if reasons.is_empty() {
                targets::TargetProblem::NoEbuilds
            } else {
                targets::TargetProblem::AllFiltered
            };
            let unsat = output::UnsatisfiableTarget {
                atom: atom.clone(),
                origin: target.origin.clone(),
                problem,
                reasons,
            };
            // `--emptytree` deliberately ignores what is installed, so a
            // satisfying VDB entry must not silence the atom there.
            let satisfied = !empty && targets::installed_satisfies(&dep, installed);
            match targets::classify_root_target(&target.origin, satisfied, selective) {
                targets::RootTargetDecision::DropWithWarning => {
                    unsatisfiable.push(unsat);
                    continue;
                }
                targets::RootTargetDecision::DropSilently => {
                    // The installed slot must be this ebuild's slot. Another
                    // slot of the same version is a different package.
                    masked_kept.extend(unsat.reasons.iter().filter_map(|c| {
                        let slot = c.slot?;
                        let installed_here = installed.get(&c.cpv.cpn).is_some_and(|slots| {
                            slots.get(&slot).is_some_and(|v| *v == c.cpv.version)
                        });
                        installed_here
                            .then(|| (c.cpv.clone(), c.slot, repo::filter_reason_text(&c.reasons)))
                    }));
                    continue;
                }
                targets::RootTargetDecision::Fatal => {
                    anyhow::bail!(output::unsatisfiable_target_message(
                        &unsat, data, multi_repo
                    ));
                }
            }
        }
        cpns.insert(dep.cpn);
        deps.push((pkg, vs));
    }
    Ok(RootTargets {
        deps,
        cpns,
        unsatisfiable,
        masked_kept,
    })
}

/// Prefer a newer accepted same-slot version for a slot-operator rebuild.
///
/// Uses it only when that version's raw DEPEND and RDEPEND still accept every
/// trigger. Otherwise returns `rb.version`.
fn best_rebuild_version(
    data: &repo::RepoData,
    policy: &repo::ResolvePolicy,
    rb: &subslot::SubslotRebuild,
    planned_slots: &HashMap<Cpn, Vec<(Version, portage_atom::Slot)>>,
) -> Version {
    let Some(entries) = data.versions.get(&rb.cpn) else {
        return rb.version.clone();
    };
    let mut candidates: Vec<_> = entries
        .iter()
        .filter(|(cpv, cache)| {
            cpv.version > rb.version
                && rb.slot.is_none_or(|s| cache.metadata.slot.slot == s)
                && policy.accept_keywords.accepts(
                    &cache.metadata.keywords,
                    cpv,
                    Some(cache.metadata.slot.slot),
                )
                && !repo::is_masked(
                    policy.package_mask,
                    policy.package_unmask,
                    cpv,
                    &cache.metadata.slot,
                    repo::repo_name_of(data, cpv),
                )
        })
        .collect();
    candidates.sort_by(|a, b| b.0.version.cmp(&a.0.version));
    candidates
        .into_iter()
        .find(|(_, cache)| {
            rb.triggers
                .iter()
                .all(|trig| trigger_still_satisfied(cache, *trig, planned_slots))
        })
        .map_or_else(|| rb.version.clone(), |(cpv, _)| cpv.version.clone())
}

/// Whether `cache`'s own DEPEND/RDEPEND still binds `trigger` to a version
/// range its planned version satisfies (see [`best_rebuild_version`])
fn trigger_still_satisfied(
    cache: &portage_metadata::CacheEntry,
    trigger: Cpn,
    planned_slots: &HashMap<Cpn, Vec<(Version, portage_atom::Slot)>>,
) -> bool {
    fn collect<'a>(entries: &'a [portage_atom::DepEntry], trigger: Cpn, out: &mut Vec<&'a Dep>) {
        for entry in entries {
            match entry {
                portage_atom::DepEntry::Atom(dep)
                    if dep.blocker.is_none() && dep.cpn == trigger =>
                {
                    out.push(dep);
                }
                portage_atom::DepEntry::UseConditional { children, .. }
                | portage_atom::DepEntry::AllOf(children)
                | portage_atom::DepEntry::AnyOf(children)
                | portage_atom::DepEntry::ExactlyOneOf(children)
                | portage_atom::DepEntry::AtMostOneOf(children) => collect(children, trigger, out),
                portage_atom::DepEntry::Atom(_) => {}
            }
        }
    }

    let Some(planned) = planned_slots.get(&trigger) else {
        return true;
    };
    let mut atoms = Vec::new();
    collect(cache.metadata.depend.list(), trigger, &mut atoms);
    collect(cache.metadata.rdepend.list(), trigger, &mut atoms);
    if atoms.is_empty() {
        return false;
    }
    atoms.iter().any(|dep| {
        let vs = conflicts::dep_to_version_set(dep);
        planned.iter().any(|(ver, _)| vs.contains(ver))
    })
}

/// Whether an installed VDB entry must rebuild under `-N`/`-U` for USE/IUSE
/// drift relative to the planned fold for its CPV (or the newest same-slot
/// repo version when the exact CPV left the tree).
fn package_needs_use_reinstall(
    mode: portage_resolve::use_reinstall::UseReinstallMode,
    e: &installed::VdbEntry,
    pkg: &PortagePackage,
    data: &repo::RepoData,
    policy: &repo::ResolvePolicy,
) -> bool {
    use portage_atom_pubgrub::UseFlagState;
    use portage_resolve::use_reinstall::needs_use_reinstall;
    use std::collections::HashSet;

    // Prefer the installed CPV's cache entry; fall back to newest same-slot
    // version when the exact CPV left the tree.
    let (plan_ver, cache) = if let Some(c) = repo::find_cache(data, pkg, &e.version) {
        (e.version.clone(), c)
    } else {
        let Some((cpv, c)) = data.versions.get(&e.cpn).and_then(|vers| {
            vers.iter().rev().find(|(_, entry)| {
                let got = entry.metadata.slot.slot.as_str();
                match e.slot.as_ref().map(|s| s.as_str()) {
                    Some(want) => got == want || got.split('/').next() == Some(want),
                    None => true,
                }
            })
        }) else {
            return false;
        };
        (cpv.version.clone(), c)
    };
    // Stable-keyword decision is approximate here (any stable token); force
    // mask's stable sets rarely change reinstall detection vs the main USE fold.
    let stable = true;
    let cfg = effective_use::effective_use(policy, pkg, &plan_ver, cache, stable, &[]);
    let cur_iuse = effective_use::iuse_set(cache, &policy.force_mask.iuse_injection);
    let cur_enabled: HashSet<_> = cur_iuse
        .iter()
        .copied()
        .filter(|f| matches!(cfg.get(*f), UseFlagState::Enabled))
        .collect();
    let cpv = Cpv::new(e.cpn, plan_ver);
    let slot = e.slot.map(portage_atom::Slot::from_name);
    let (forced, masked) = policy
        .force_mask
        .effective(&cpv, slot.as_ref(), stable, &cur_iuse);
    let forced: HashSet<_> = forced.into_iter().chain(masked).collect();
    // Real Portage diffs `pkg.iuse.all` on *both* sides (`_reinstall_for_flags`
    // callers pass `iuses = pkg.iuse.all` for the installed package too) —
    // implicit IUSE injection (ARCH/ELIBC/KERNEL, PMS 11.1.1) included, under
    // that package's own EAPI. Comparing against the raw VDB `IUSE` file
    // (declared flags only) here would make every package with sparse/empty
    // declared IUSE look like it gained the whole implicit set, spuriously
    // flagging it for reinstall.
    let orig_iuse: Vec<_> = portage_resolve::force_mask::iuse_effective_set(
        e.eapi,
        e.iuse.iter().copied(),
        &policy.force_mask.iuse_injection,
    )
    .into_iter()
    .collect();
    needs_use_reinstall(
        mode,
        &forced,
        &e.active_use,
        &orig_iuse,
        &cur_enabled,
        &cur_iuse,
    )
}

/// Whether two versions plausibly belong to the same slot, used to map a
/// `package.provided` CPV onto the repo slot a `:slot` dep would reference.
///
/// Compares the leading numeric components up to the shorter version's length
/// (`3.14.0` vs `3.14.6` → same; `3.14.0` vs `3.15.9999` → different). Slots in
/// the tree are cut from a version prefix (`python` → `3.14`, `gcc` → `14`), so
/// a shared prefix is a good proxy without hard-coding any package's slot rule.
fn same_slot_series(a: &Version, b: &Version) -> bool {
    let n = a.numbers.len().min(b.numbers.len()).min(2);
    n > 0 && a.numbers[..n] == b.numbers[..n]
}
