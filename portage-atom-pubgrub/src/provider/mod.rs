use std::collections::{BTreeMap, HashMap, HashSet};

use crate::repository::IUseDefault;
use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Dep, Version};
use pubgrub::{Dependencies, SelectedDependencies};

use crate::convert;
use crate::package::{MergeRoot, PortagePackage};
use crate::repository::PackageRepository;
use crate::use_config::{UseConfig, UseFlagState};
use crate::version_set::PortageVersionSet;

/// Post-solve USE-requirement analysis
mod post_solve;
/// The PubGrub `DependencyProvider` impl (prioritise / choose_version /
/// get_dependencies)
mod solve;

/// Whether an installed package should be favored or locked during resolution
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstalledPolicy {
    /// Prefer the installed version when multiple candidates exist,
    /// but allow upgrades if required by dependencies
    Favor,
    /// The installed version must not change — only that exact version
    /// is acceptable
    Lock,
    /// Present in the VDB for action tags and post-solve checks, but must be
    /// rebuilt from the repository: never favored in version selection and
    /// always expanded with full build-time deps (`emerge --emptytree`)
    Rebuild,
    /// `package.provided`: the system claims this exists, with no real VDB
    /// record behind it. Same version-selection as `Favor`, but — unlike a
    /// genuinely installed package — its own dependencies are never explored
    /// when kept (no CONTENTS/PROVIDES to trust), only when a caller forces
    /// a real build of it (an explicit target, same as `Favor`'s reinstall
    /// path already allows).
    Provided,
}

/// All solver-relevant data for one package version
///
/// Previously this was eight parallel `BTreeMap<Version, _>` fields on
/// `PackageData`; collapsing them into one struct keeps a version's data
/// cohesive and removes the hand-synced map inserts.
pub(crate) struct VersionData {
    /// Merged deps for PubGrub's DependencyProvider trait
    pub(crate) merged: Dependencies<PortagePackage, PortageVersionSet, String>,
    /// Per-class converted deps with optional gating USE flag
    ///
    /// Index: 0=DEPEND, 1=RDEPEND, 2=BDEPEND, 3=PDEPEND, 4=IDEPEND
    pub(crate) by_class: Vec<Vec<convert::Req>>,
    pub(crate) blockers: Vec<Dep>,
    pub(crate) use_deps: Vec<convert::UseDepConstraint>,
    pub(crate) iuse: Vec<Interned<DefaultInterner>>,
    pub(crate) iuse_defaults: HashMap<Interned<DefaultInterner>, IUseDefault>,
    pub(crate) repo: Option<Interned<DefaultInterner>>,
    pub(crate) repo_constraints: Vec<convert::RepoConstraint>,
    pub(crate) slot_operator_deps: Vec<convert::SlotOperatorDep>,
    /// The resolved **desired** USE state for this version: `package.use` and
    /// global USE applied on top of the ebuild's IUSE defaults
    ///
    /// This is the single source of truth for "is flag F on for this
    /// version" during both branch conversion and the post-solve passes.
    pub(crate) desired: UseConfig,
    /// The repository offered this version only in widened (`--autounmask`)
    /// candidate-supply mode: it is outside keyword/mask/license acceptance.
    ///
    /// Also set on a virtual `Choice`/`SlotChoice` *branch* whose targets are
    /// all tagged, so `choose_version`'s tiering demotes it below any branch
    /// satisfiable without an unmask (a first-listed tagged branch would
    /// otherwise win `max()` silently — both branches being satisfiable means
    /// PubGrub never backtracks into the alternative).
    pub(crate) needs_unmask: bool,
    /// Live ebuild (`*9999` shape or `PROPERTIES=live`): never selected
    /// transitively under the widened-autounmask policy — only a root
    /// target may land on one.
    pub(crate) live: bool,
}

impl VersionData {
    /// `DEPEND` — build-time (target) deps
    ///
    /// Named accessors over the positional `by_class` layout (0=DEPEND
    /// 1=RDEPEND 2=BDEPEND 3=PDEPEND 4=IDEPEND) so the root-routing in
    /// `solve.rs` reads by name, not magic index.
    pub(crate) fn depend(&self) -> &[convert::Req] {
        &self.by_class[0]
    }
    /// `RDEPEND` — run-time deps
    pub(crate) fn rdepend(&self) -> &[convert::Req] {
        &self.by_class[1]
    }
    /// `BDEPEND` — build-host deps (EAPI 7+)
    pub(crate) fn bdepend(&self) -> &[convert::Req] {
        &self.by_class[2]
    }
    /// `PDEPEND` — post-merge deps
    pub(crate) fn pdepend(&self) -> &[convert::Req] {
        &self.by_class[3]
    }
    /// `IDEPEND` — install-time deps (EAPI 8+)
    pub(crate) fn idepend(&self) -> &[convert::Req] {
        &self.by_class[4]
    }

    /// Build a deps-only version (no blockers/use-deps/etc.), used for synthetic
    /// solver nodes: the root target set and OR-group / USE-decision branches
    ///
    /// `merged` is collected from a flattened view of `by_class` (flag stripped).
    fn from_by_class(by_class: Vec<Vec<convert::Req>>) -> Self {
        let merged = Dependencies::Available(
            by_class
                .iter()
                .flatten()
                .map(|(p, vs, _)| (p.clone(), vs.clone()))
                .collect(),
        );
        Self {
            merged,
            by_class,
            blockers: Vec::new(),
            use_deps: Vec::new(),
            iuse: Vec::new(),
            iuse_defaults: HashMap::new(),
            repo: None,
            repo_constraints: Vec::new(),
            slot_operator_deps: Vec::new(),
            desired: UseConfig::new(),
            needs_unmask: false,
            live: false,
        }
    }
}

pub(crate) struct PackageData {
    pub(crate) versions: BTreeMap<Version, VersionData>,
}

/// A package that is already installed, with its version and policy
#[derive(Debug, Clone)]
pub struct InstalledPackage {
    /// The installed package identity
    pub package: PortagePackage,
    /// The installed version
    pub version: Version,
    /// How to treat this package during resolution
    pub policy: InstalledPolicy,
    /// USE flags that were active (enabled) when this package was built
    ///
    /// Used to evaluate USE dep constraints on OR-group branches so the solver
    /// can prefer branches that are already satisfied without a rebuild.
    pub active_use: Vec<Interned<DefaultInterner>>,
    /// IUSE flags declared by this installed package (flag names without `+`/`-` prefix)
    ///
    /// Required because the repository may not carry the exact installed version
    /// any more (e.g. glib-2.84.4-r2 installed while the repo only has r5).  In
    /// that case `PackageData.iuse` has no entry for the installed version, so we
    /// fall back to the VDB-recorded IUSE to avoid false-positive reinstall reports.
    pub iuse: Vec<Interned<DefaultInterner>>,
}

/// A dependency that was dropped because no versions were available
///
/// Dropped deps are always alternatives inside an `||` dep group — a
/// successful resolution means the other branch was chosen instead.
#[derive(Debug, Clone)]
pub struct DroppedDep {
    /// The package that was dropped
    pub package: PortagePackage,
    /// The version range that was requested
    pub version_set: PortageVersionSet,
    /// Sibling branches in the same `||` group that are available (a real
    /// package, or a `SlotChoice`/`Choice` virtual when the sibling branch is
    /// multi-slot or itself a nested group)
    ///
    /// Empty when the dep was not inside a `||`, or when every sibling is
    /// also unavailable — only then is the dropped dep a genuine autounmask
    /// candidate.
    pub alternatives: Vec<PortagePackage>,
}

/// USE flag changes required on a package by the resolved dependency set
///
/// Produced by the post-solve validation pass in
/// [`PortageDependencyProvider::resolve_targets`].
///
/// For **installed** packages the required changes were not yet applied, so the
/// package must be rebuilt — this corresponds to portage's `R` action.
///
/// For **new** packages the required flags should be set when the package is
/// built.  Since our solver does not yet enforce USE dep constraints at build
/// time, these are reported as informational annotations.
#[derive(Debug, Clone)]
pub struct UseFlagRequirement {
    /// The package the requirements apply to
    pub package: PortagePackage,
    /// The currently-installed (or selected) version
    pub version: Version,
    /// If set, the package should be **upgraded** to this version rather than
    /// rebuilt at `version`
    ///
    /// Present when the installed version is superseded by a newer repo
    /// version whose constraints drove the requirement.
    pub upgrade_to: Option<Version>,
    /// USE flags that must be **enabled** — required by at least one constraint
    /// but not yet active (installed: violated now; new: may not be set by config)
    pub required_enabled: Vec<Interned<DefaultInterner>>,
    /// USE flags that must be **disabled** — forbidden by at least one constraint
    /// but currently active
    pub required_disabled: Vec<Interned<DefaultInterner>>,
    /// The package(s) that imposed the USE dep constraints (CPN strings)
    ///
    /// Used to generate `package.use` comments.
    pub required_by: Vec<String>,
}

/// A PubGrub `DependencyProvider` backed by a portage package repository
///
/// Pre-computes all dependency information at construction time, then serves
/// it to the PubGrub solver.
pub struct PortageDependencyProvider {
    /// Keyed by the `Target`-flavored identity only — a `Host`-flavored
    /// package's data lives under its alias (see `host_aliases`)
    ///
    /// Private so `package_data()`/`package_data_key()` are the only lookup
    /// path — a raw `.get()` on a possibly-Host-flavored key silently
    /// misses instead of resolving the alias (a recurring bug class, hit
    /// in `graph.rs`, `validate.rs`, and `post_solve.rs` alike).
    packages: HashMap<PortagePackage, PackageData>,
    pub(crate) installed: HashMap<PortagePackage, (Version, InstalledPolicy)>,
    pub(crate) installed_cpns: HashSet<Cpn>,
    pub(crate) installed_use: HashMap<PortagePackage, Vec<Interned<DefaultInterner>>>,
    pub(crate) installed_iuse: HashMap<PortagePackage, Vec<Interned<DefaultInterner>>>,
    /// Blocker atoms declared by installed packages (pre-USE-evaluated), so
    /// [`check_blockers`](Self::check_blockers) can report ones a retained
    /// installed owner points at the plan — the owner is never in the solve
    pub(crate) installed_blockers: HashMap<PortagePackage, Vec<Dep>>,
    /// Packages present on the **build host** (BROOT), used only to satisfy
    /// `BDEPEND` edges — a BDEPEND the host already provides is dropped in
    /// [`get_dependencies`](crate::DependencyProvider::get_dependencies), so
    /// an offset build (`--root <empty>`) doesn't pull host-provided build
    /// tools into the plan
    ///
    /// A flat atom set fed by the caller (policy layer); the solver is
    /// root-agnostic. Always "present" (Lock-equivalent): never re-chosen.
    pub(crate) host_installed: HashMap<PortagePackage, HostEntry>,
    /// Packages present in the cross **sysroot** (`ESYSROOT`), used to satisfy
    /// `DEPEND` edges for target-root instances when [`cross_active`](Self::cross_active)
    pub(crate) sysroot_installed: HashMap<PortagePackage, Version>,
    /// Dual-root solver mode: stamp dependency targets with [`MergeRoot`] and
    /// register host-side package instances
    ///
    /// Set for both a genuine foreign-arch cross build *and* a same-arch
    /// offset build (`--root <dir>`) — see [`is_cross_arch`](Self::is_cross_arch)
    /// for the distinction the two need at the `DEPEND`-filtering branch in
    /// `solve.rs`.
    pub(crate) cross_active: bool,
    /// Whether this is a genuine foreign-arch build (`CHOST != CBUILD`), not
    /// just a same-arch offset (`--root <dir>`) — `cross_active` alone
    /// doesn't distinguish them
    ///
    /// Only a genuine cross build keeps `DEPEND` pinned to the target
    /// sysroot unconditionally.
    ///
    /// A same-arch offset build's `DEPEND` is satisfied by whatever machine
    /// compiles, same as `BDEPEND` — the old fallback treated any non-host
    /// sysroot as foreign-arch, so `--root` never dropped it.
    pub(crate) is_cross_arch: bool,
    /// Host `@host` instances alias target package data (no duplicate ingest)
    pub(crate) host_aliases: HashMap<PortagePackage, PortagePackage>,
    pub(crate) dropped_deps: Vec<DroppedDep>,
    /// USE flag requirements collected by the post-solve validation pass
    ///
    /// Covers both reinstall cases (`R`: installed packages with violated
    /// constraints) and informational cases (`N`/`U`: new packages whose
    /// required flags may not be set by the current global config).
    pub(crate) use_flag_requirements: Vec<UseFlagRequirement>,
    /// Installed packages that a previous solve iteration decided to upgrade to a
    /// newer repo version (`upgrade_to`)
    ///
    /// On the next iteration the solver pins these to the new version so
    /// its full dependency closure is re-solved, instead of leaving the
    /// upgraded version's deps unaccounted for. Cleared at the start of
    /// every [`resolve_targets`](Self::resolve_targets) call.
    pub(crate) upgrade_pins: HashMap<PortagePackage, Version>,
    /// Explicitly requested target packages (set by `resolve_targets`), with
    /// the version set the caller requested them under
    ///
    /// `choose_version` does not favor the installed version for these: a
    /// named argument selects the best accepted version, as emerge does
    /// (installed-and-best still resolves to the installed version).
    pub(crate) root_targets: std::collections::HashMap<PortagePackage, PortageVersionSet>,
    /// Whether to include BDEPEND in the resolution (emerge's `--with-bdeps`)
    ///
    /// When false (default), BDEPEND are excluded from resolution for
    /// packages being built (assumed provided by BROOT). When true,
    /// BDEPEND are included but filtered by `host_installed`.
    pub(crate) with_bdeps: bool,
    /// `--emptytree`: do not prefer installed virtual/OR branches; full deep
    /// closure from repository candidates
    pub(crate) rebuild_tree: bool,
    /// `--deep` (and native `--emptytree`): for a `:*` any-slot dep
    /// (`SlotChoice`), bump to the newest slot instead of keeping the installed
    /// slot that already satisfies `>=MIN` — matching `emerge -uD`/`-e`
    ///
    /// `max()` over a `SlotChoice` picks the newest-*version* slot (slots
    /// are ranked by version, see `rank_slots_by_version`). Off by default
    /// so plain `-p`/`-up` stays minimal.
    pub(crate) prefer_newest_slot: bool,
    /// `--update --deep` (not emptytree): do not Favour installed versions for
    /// packages that appear in the solve — pick the newest accepted in-slot
    /// version in range (emerge `-uD` in-slot upgrades)
    ///
    /// Distinct from [`Self::prefer_newest_slot`] (cross-slot `:*` bumps)
    /// and from [`InstalledPolicy::Rebuild`] (emptytree full build-closure
    /// expansion).
    pub(crate) prefer_update: bool,
    /// Selective resolution without `--update`: an installed version that
    /// satisfies a root target is kept rather than replaced by the newest
    /// accepted one
    ///
    /// `--noreplace`/`--newuse`/`--changed-use` mean "leave a satisfied
    /// target alone"; `--update` means the opposite, so the caller clears
    /// this when it is set. Off by default, which keeps the plain
    /// `em <atom>` reinstall.
    pub(crate) selective_no_update: bool,

    /// `--root-deps=rdeps` (crossdev cross builds): discard a target package's
    /// `DEPEND` from the target-root graph
    ///
    /// Only `RDEPEND`/`PDEPEND` install into the sysroot; build-time deps
    /// resolve against the build host (`/`), where the cross toolchain
    /// lives. Off by default, and gated to true cross-arch invocations by
    /// the caller (never native offset/same-arch stage builds, which keep
    /// `DEPEND` → target ROOT).
    pub(crate) root_deps_rdeps: bool,
    /// `--nodeps` (emerge `-O`): merge only the explicitly named targets, with no
    /// dependency expansion
    ///
    /// Real packages report no dependencies, so the solve resolves the
    /// requested atoms to versions and nothing else. Used by the staged
    /// toolchain bootstrap to break the glibc-headers→newer-gcc cycle
    /// before a compiler exists. Off by default.
    pub(crate) nodeps: bool,
    /// Preferred version (`0`/`1`) for each `UseDecision` node, i.e. the value
    /// the caller's policy would have given the ceded flag
    ///
    /// `choose_version` biases toward it so a `SolverDecided` flag only
    /// flips when a constraint forces it (greedy keep-configured — see
    /// `docs/required-use-level-c.md`).
    pub(crate) use_decision_prefer: HashMap<PortagePackage, Version>,
    /// Reverse map from a `UseDecision` node to the `(cpn, flag)` it decides,
    /// so the chosen values can be reported back to the caller by name
    pub(crate) use_decision_meta: HashMap<PortagePackage, (Cpn, Interned<DefaultInterner>)>,
    /// The value the solver chose for each `UseDecision` node in the last solve
    /// (`true` = on)
    ///
    /// Captured before virtual nodes are stripped from the result.
    pub(crate) solved_use_decisions: HashMap<PortagePackage, bool>,
}

/// A USE flag the caller ceded to the solver, with the value the solver chose
#[derive(Debug, Clone)]
pub struct CededFlag {
    /// The package the flag belongs to
    pub cpn: Cpn,
    /// The ceded flag
    pub flag: Interned<DefaultInterner>,
    /// The value the solver chose (`true` = enabled)
    pub value: bool,
    /// `true` when the chosen value differs from the caller's preference, i.e
    /// the solver flipped it to satisfy a constraint
    pub flipped: bool,
}

/// A package present on the build host (BROOT)
///
/// Used to satisfy `BDEPEND`/`IDEPEND` edges without building them into
/// the plan. Carries the host instance's active USE and IUSE so a
/// host-satisfied edge can be checked against its atom USE-dependencies: a
/// `[flag]` the host lacks is **not** satisfied — the package must be
/// rebuilt, pulling its re-evaluated USE-conditional closure (PMS
/// §8.3/§8.2.2).
#[derive(Debug, Clone)]
pub(crate) struct HostEntry {
    /// Installed version on BROOT
    pub version: Version,
    /// The host instance's active USE flags (VDB `USE`)
    pub active_use: Vec<Interned<DefaultInterner>>,
    /// The host instance's `IUSE` (VDB `IUSE`), stripped of `+`/`-` defaults
    pub iuse: Vec<Interned<DefaultInterner>>,
}

/// Build the whole-repository slot map used by `convert_deps` to resolve
/// unslotted deps against multi-slot packages, from the cheap `slots_for`
/// projection (no dependency conversion) covering every CPN in the
/// repository — not just the resolve's seeded closure — so an unslotted dep
/// on a multi-slot package resolves correctly no matter which package
/// references it
///
/// Expensive to redo per solve: for a real Gentoo tree this filters
/// `slots_for`'s ~32k cached versions through keyword/mask/license
/// acceptance. Callers that rebuild the provider repeatedly against the
/// same repository view (USE-dep co-solve) should call this once and reuse
/// it via
/// [`PortageDependencyProvider::new_for_targets_with_bdeps_and_slot_map`].
pub fn build_slot_map<R: PackageRepository>(repo: &R) -> convert::SlotMap {
    let mut cpn_slots: HashMap<Cpn, Vec<Interned<DefaultInterner>>> = HashMap::new();
    for cpn in repo.all_packages() {
        let slots = repo.slots_for(&cpn);
        if !slots.is_empty() {
            cpn_slots.insert(cpn, slots);
        }
    }
    cpn_slots
        .iter()
        .map(|(&cpn, slots)| {
            let slot_packages = slots
                .iter()
                .map(|&s| (s, PortagePackage::slotted(cpn, s)))
                .collect();
            (cpn, slot_packages)
        })
        .collect()
}

impl PortageDependencyProvider {
    /// Build the provider from a repository
    ///
    /// All USE policy is the repository's concern: each version's effective
    /// desired USE is obtained via [`PackageRepository::desired_use`] (which
    /// folds global USE, `package.use`, and IUSE defaults).  The solver never
    /// resolves policy itself.  See `docs/use-and-solver-boundary.md`.
    pub fn new<R: PackageRepository>(repo: R) -> Self {
        let seeds = repo.all_packages();
        let slot_map = build_slot_map(&repo);
        Self::new_with_seeds(repo, seeds, false, &slot_map)
    }

    /// Like [`new`](Self::new), but converts only the packages *reachable*
    /// from `seeds` (typically the resolve targets plus the installed set)
    ///
    /// References are followed transitively, so after ingestion a missing
    /// referenced package is genuinely absent — dropped-dependency filtering
    /// stays sound. Converts a few hundred packages instead of the whole tree.
    pub fn new_for_targets<R: PackageRepository>(repo: R, seeds: Vec<Cpn>) -> Self {
        let slot_map = build_slot_map(&repo);
        Self::new_with_seeds(repo, seeds, false, &slot_map)
    }

    /// Like [`new_for_targets`](Self::new_for_targets), but with explicit
    /// control over whether BDEPEND are included in the resolution
    pub fn new_for_targets_with_bdeps<R: PackageRepository>(
        repo: R,
        seeds: Vec<Cpn>,
        with_bdeps: bool,
    ) -> Self {
        let slot_map = build_slot_map(&repo);
        Self::new_with_seeds(repo, seeds, with_bdeps, &slot_map)
    }

    /// Like [`new_for_targets_with_bdeps`](Self::new_for_targets_with_bdeps),
    /// but takes an already-computed slot map (see [`build_slot_map`])
    /// instead of recomputing it internally
    ///
    /// For callers that rebuild the provider repeatedly against the same
    /// repository view — e.g. the USE-dep co-solve fixpoint, which reruns
    /// the whole solve up to ~8× per invocation — recomputing `slots_for`
    /// every rebuild is the single largest redundant cost (a real Gentoo
    /// tree has ~20k CPNs / ~32k cached versions to re-filter). Building the
    /// slot map once and reusing it here removes that cost after the first.
    pub fn new_for_targets_with_bdeps_and_slot_map<R: PackageRepository>(
        repo: R,
        seeds: Vec<Cpn>,
        with_bdeps: bool,
        slot_map: &convert::SlotMap,
    ) -> Self {
        Self::new_with_seeds(repo, seeds, with_bdeps, slot_map)
    }

    fn new_with_seeds<R: PackageRepository>(
        repo: R,
        seeds: Vec<Cpn>,
        with_bdeps: bool,
        slot_map: &convert::SlotMap,
    ) -> Self {
        let mut packages = HashMap::new();
        let mut use_decision_prefer: HashMap<PortagePackage, Version> = HashMap::new();
        let mut use_decision_meta: HashMap<PortagePackage, (Cpn, Interned<DefaultInterner>)> =
            HashMap::new();

        // Second pass: register versions and convert deps for the closure of
        // `seeds` — every package referenced by a converted dependency (or a
        // virtual choice branch) is queued in turn.
        let mut queue: std::collections::VecDeque<Cpn> = seeds.into_iter().collect();
        let mut seen: HashSet<Cpn> = queue.iter().copied().collect();
        while let Some(cpn) = queue.pop_front() {
            let versions_data = repo.versions_for(&cpn);
            // Loop-invariant per CPN (not per version) — hoisted out of the
            // version loop below, which previously re-formatted the same
            // string for every accepted version of this package.
            let cpn_str = format!("{}/{}", cpn.category, cpn.package);

            for (cpv, meta) in versions_data {
                let pkg = match &meta.slot {
                    Some(slot) => PortagePackage::slotted(cpn, *slot),
                    None => {
                        if let Some([(_, sole_pkg)]) = slot_map.get(&cpn).map(|v| v.as_slice()) {
                            sole_pkg.clone()
                        } else {
                            PortagePackage::unslotted(cpn)
                        }
                    }
                };

                let dep_classes: [&[portage_atom::DepEntry]; 5] = [
                    &meta.deps.depend,
                    &meta.deps.rdepend,
                    &meta.deps.bdepend,
                    &meta.deps.pdepend,
                    &meta.deps.idepend,
                ];

                // The resolved desired USE for this version (caller's policy:
                // package.use + global USE folded over IUSE defaults).  This is
                // the single authoritative set every later reader consults.
                let cpv_use_cfg = repo.desired_use(&cpv);

                // Record the preferred value of every ceded (`SolverDecided`)
                // flag so `choose_version` can bias its `UseDecision` node toward
                // the caller's configured value (greedy keep-configured).
                for flag in cpv_use_cfg.solver_decided_flags() {
                    if let UseFlagState::SolverDecided { prefer } = cpv_use_cfg.get(flag) {
                        let node = convert::use_decision_package(&cpn_str, &flag);
                        let ver = Version::new(&[u64::from(prefer)]);
                        use_decision_prefer.insert(node.clone(), ver);
                        use_decision_meta.insert(node, (cpn, flag));
                    }
                }

                let class_results: [convert::ConversionResult; 5] = dep_classes.map(|entries| {
                    convert::convert_deps_groups(
                        entries,
                        &cpn_str,
                        &cpv_use_cfg,
                        slot_map,
                        meta.empty_any_of_matches,
                    )
                });

                let mut all_blockers = Vec::new();
                let mut all_use_deps = Vec::new();
                let mut all_repo_constraints = Vec::new();
                let mut all_virtual_choices = Vec::new();
                let mut all_slot_operator_deps = Vec::new();
                let mut by_class: Vec<Vec<convert::Req>> = Vec::with_capacity(5);

                for result in class_results {
                    all_blockers.extend(result.blockers);
                    all_use_deps.extend(result.use_deps);
                    all_repo_constraints.extend(result.repo_constraints);
                    all_virtual_choices.extend(result.virtual_choices);
                    all_slot_operator_deps.extend(result.slot_operator_deps);
                    by_class.push(result.requirements);
                }

                // Level-C: encode REQUIRED_USE over the package's UseDecision
                // nodes (only ceded flags produce constraints; with everything
                // fixed this is inert). The pull/force/choice requirements ride
                // in the DEPEND class (index 0); they reference virtual nodes,
                // which are stripped from the install order.
                if let Some(ru) = &meta.required_use {
                    let enc = convert::encode_required_use(&cpn_str, ru, &cpv_use_cfg);
                    by_class[0].extend(enc.requirements);
                    all_virtual_choices.extend(enc.virtual_choices);
                }

                // Queue every real package this version references (direct
                // requirements and virtual-choice branches) for ingestion.
                for class in &by_class {
                    for (target, _, _) in class {
                        if !target.is_virtual() {
                            let c = *target.cpn();
                            if seen.insert(c) {
                                queue.push_back(c);
                            }
                        }
                    }
                }
                for vc in &all_virtual_choices {
                    for (_, deps) in &vc.versions {
                        for (target, _, _) in deps {
                            if !target.is_virtual() {
                                let c = *target.cpn();
                                if seen.insert(c) {
                                    queue.push_back(c);
                                }
                            }
                        }
                    }
                }

                let mut version_data = VersionData::from_by_class(by_class);
                version_data.blockers = all_blockers;
                version_data.use_deps = all_use_deps;
                version_data.iuse = meta.iuse;
                version_data.iuse_defaults = meta.iuse_defaults;
                version_data.repo = meta.repo;
                version_data.repo_constraints = all_repo_constraints;
                version_data.slot_operator_deps = all_slot_operator_deps;
                version_data.desired = cpv_use_cfg;
                version_data.needs_unmask = meta.needs_unmask;
                version_data.live = meta.live;

                let entry = packages.entry(pkg).or_insert_with(|| PackageData {
                    versions: BTreeMap::new(),
                });
                entry.versions.insert(cpv.version.clone(), version_data);

                register_virtual_choices(&mut packages, all_virtual_choices);
            }
        }

        propagate_needs_unmask(&mut packages);

        // Post-process: remove dependencies on packages not present in the
        // repository.  Without this filtering, PubGrub will encounter
        // `NoVersions` for any missing package and immediately declare the
        // problem unsolvable.
        let known: HashSet<PortagePackage> = packages.keys().cloned().collect();

        // Build a map from each branch of a || group (Choice node) to its sibling
        // branches.  Used to populate DroppedDep::alternatives so a dropped branch
        // with an available sibling is not reported by autounmask.  Virtual
        // siblings are kept: a multi-slot branch (e.g. `>=sys-devel/gcc-6.2` over
        // gcc's many slots) is represented as a `SlotChoice` node, and its
        // presence in `known` is exactly the "an alternative is available" signal
        // — dropping it left a single-version sibling (e.g. `llvm-runtimes/libgcc`,
        // masked for this arch) looking alternative-less and falsely reported.
        let mut or_alternatives: HashMap<PortagePackage, Vec<PortagePackage>> = HashMap::new();
        for (pkg, data) in packages.iter_mut() {
            if !matches!(pkg, PortagePackage::Choice { .. }) {
                continue;
            }
            let mut branch_deps: Vec<PortagePackage> = Vec::new();
            for vd in data.versions.values_mut() {
                if let Dependencies::Available(constraints) = &mut vd.merged {
                    let taken = std::mem::take(constraints);
                    let items: Vec<_> = taken.into_iter().collect();
                    for (dep, _) in &items {
                        branch_deps.push(dep.clone());
                    }
                    *constraints = items.into_iter().collect();
                }
            }
            for i in 0..branch_deps.len() {
                let others: Vec<_> = branch_deps
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != i)
                    .map(|(_, d)| d.clone())
                    .collect();
                or_alternatives
                    .entry(branch_deps[i].clone())
                    .or_default()
                    .extend(others);
            }
        }

        let mut dropped_deps = Vec::new();
        for data in packages.values_mut() {
            for vd in data.versions.values_mut() {
                if let Dependencies::Available(constraints) = &mut vd.merged {
                    let taken = std::mem::take(constraints);
                    let (kept, dropped): (Vec<_>, Vec<_>) =
                        taken.into_iter().partition(|(pkg, _)| known.contains(pkg));
                    dropped_deps.extend(dropped.into_iter().map(|(pkg, vs)| {
                        let alternatives = or_alternatives
                            .get(&pkg)
                            .map(|alts| {
                                alts.iter().filter(|a| known.contains(a)).cloned().collect()
                            })
                            .unwrap_or_default();
                        DroppedDep {
                            package: pkg,
                            version_set: vs,
                            alternatives,
                        }
                    }));
                    *constraints = kept.into_iter().collect();
                }
                for class in &mut vd.by_class {
                    class.retain(|(pkg, _, _)| known.contains(pkg));
                }
            }
        }

        Self {
            packages,
            installed: HashMap::new(),
            installed_cpns: HashSet::new(),
            installed_use: HashMap::new(),
            installed_blockers: HashMap::new(),
            installed_iuse: HashMap::new(),
            host_installed: HashMap::new(),
            sysroot_installed: HashMap::new(),
            cross_active: false,
            is_cross_arch: false,
            host_aliases: HashMap::new(),
            dropped_deps,
            use_flag_requirements: Vec::new(),
            upgrade_pins: HashMap::new(),
            root_targets: std::collections::HashMap::new(),
            with_bdeps,
            rebuild_tree: false,
            prefer_newest_slot: false,
            prefer_update: false,
            selective_no_update: false,
            root_deps_rdeps: false,
            nodeps: false,
            use_decision_prefer,
            use_decision_meta,
            solved_use_decisions: HashMap::new(),
        }
    }

    /// Record an installed package's pre-evaluated blocker atoms for
    /// [`check_blockers`](Self::check_blockers)' reciprocal pass
    ///
    /// No-op when empty.
    pub fn add_installed_blockers(&mut self, package: &PortagePackage, blockers: &[Dep]) {
        if !blockers.is_empty() {
            self.installed_blockers
                .insert(package.clone(), blockers.to_vec());
        }
    }

    /// Register an installed package
    ///
    /// **Favored** packages are preferred during version selection but may be
    /// upgraded if a dependency requires it. **Locked** packages are pinned to
    /// their exact installed version.
    ///
    /// If the installed version is not present in the repository (e.g. a
    /// revbump superseded it, or an older version was removed), it's
    /// registered with empty dependencies so PubGrub can select it — without
    /// this, PubGrub would call `get_dependencies`, receive `Unavailable`,
    /// and fall back to the newest repository version.
    ///
    /// Under `Favor` (non-update) `choose_version` keeps this installed stub
    /// when it satisfies the constraint, matching emerge (a revbump is not
    /// pulled without `--update`).
    pub fn add_installed(&mut self, installed: InstalledPackage) {
        self.installed_cpns.insert(*installed.package.cpn());

        // Ensure the installed version exists in self.packages so PubGrub can
        // use it even when it has been removed from the repository tree.
        if let Some(pkg_data) = self.packages.get_mut(&installed.package)
            && !pkg_data.versions.contains_key(&installed.version)
        {
            let vd = VersionData::from_by_class(vec![vec![], vec![], vec![], vec![], vec![]]);
            pkg_data.versions.insert(installed.version.clone(), vd);
        }

        if !installed.active_use.is_empty() {
            self.installed_use
                .insert(installed.package.clone(), installed.active_use);
        }
        if !installed.iuse.is_empty() {
            self.installed_iuse
                .insert(installed.package.clone(), installed.iuse);
        }
        self.installed
            .insert(installed.package, (installed.version, installed.policy));
    }

    /// Record a package as present on the build host (BROOT), so host-routed
    /// `BDEPEND` and `IDEPEND` edges can be satisfied without building it into
    /// the plan
    ///
    /// Always "present" — there is no policy to re-choose it; this only
    /// feeds the host-satisfaction check in `get_dependencies`.
    ///
    /// `active_use` / `iuse` are the host instance's VDB USE / IUSE, used to
    /// check an edge's atom USE-deps: a `[flag]` the host lacks is unsatisfied
    /// and the edge is kept (rebuilt). Pass empty vecs when USE-dep awareness
    /// is irrelevant (e.g. tests of plain version-satisfaction).
    pub fn add_host_installed(
        &mut self,
        package: PortagePackage,
        version: Version,
        active_use: Vec<Interned<DefaultInterner>>,
        iuse: Vec<Interned<DefaultInterner>>,
    ) {
        self.host_installed.insert(
            package.at_merge_root(MergeRoot::Host),
            HostEntry {
                version,
                active_use,
                iuse,
            },
        );
    }

    /// Record a package as present in the cross sysroot (`ESYSROOT`) for `DEPEND`
    /// satisfaction when [`set_cross_active`](Self::set_cross_active) is on
    pub fn add_sysroot_installed(&mut self, package: PortagePackage, version: Version) {
        self.sysroot_installed
            .insert(package.at_merge_root(MergeRoot::Target), version);
    }

    /// Enable dual-root `(package, merge_root)` solver nodes for crossdev
    pub fn set_cross_active(&mut self, active: bool) {
        self.cross_active = active;
        if active {
            self.ensure_host_instances();
        }
    }

    /// Mark this as a genuine foreign-arch build (`CHOST != CBUILD`), not
    /// just a same-arch offset (`--root <dir>`) — see the `is_cross_arch` field
    /// documentation for why `solve.rs` needs this distinct from
    /// `cross_active`
    pub fn set_is_cross_arch(&mut self, is_cross_arch: bool) {
        self.is_cross_arch = is_cross_arch;
    }

    /// `--emptytree`: rebuild the full deep closure; skip installed-branch
    /// heuristics and never favor target VDB versions during selection
    pub fn set_rebuild_tree(&mut self, active: bool) {
        self.rebuild_tree = active;
    }

    /// `--deep` / native `--emptytree`: bump `:*` any-slot deps (`SlotChoice`)
    /// to the newest slot rather than keeping a satisfying installed slot
    pub fn set_prefer_newest_slot(&mut self, active: bool) {
        self.prefer_newest_slot = active;
    }

    /// `--update --deep`: prefer newest in-slot versions for every package in
    /// the solve (disable the Favor early-return for non-root packages)
    pub fn set_prefer_update(&mut self, active: bool) {
        self.prefer_update = active;
    }

    /// Selective resolution without `--update`: keep an installed version that
    /// already satisfies a root target instead of pulling the newest accepted
    /// one
    pub fn set_selective_no_update(&mut self, active: bool) {
        self.selective_no_update = active;
    }

    /// Policy registered for an installed package, if any (`-N`/`-U` mark USE
    /// drift as [`InstalledPolicy::Rebuild`])
    pub fn installed_policy(&self, package: &PortagePackage) -> Option<InstalledPolicy> {
        self.installed.get(package).map(|(_, p)| *p)
    }

    /// `--root-deps=rdeps`: drop a target package's `DEPEND` from the sysroot
    /// graph (crossdev cross-build semantics). The caller gates this to genuine
    /// cross-arch builds; same-arch offset/stage builds leave it off
    pub fn set_root_deps_rdeps(&mut self, active: bool) {
        self.root_deps_rdeps = active;
    }

    /// `--nodeps` (emerge `-O`): merge only the named targets, no dependency
    /// expansion. A real package then reports no dependencies, so the solve
    /// resolves the requested atoms to versions and nothing else
    pub fn set_nodeps(&mut self, active: bool) {
        self.nodeps = active;
    }

    fn ensure_host_instances(&mut self) {
        let targets: Vec<PortagePackage> = self
            .packages
            .keys()
            .filter(|p| !p.is_virtual() && p.merge_root() == MergeRoot::Target)
            .cloned()
            .collect();
        for pkg in targets {
            let host = pkg.at_merge_root(MergeRoot::Host);
            self.host_aliases.entry(host).or_insert(pkg);
        }
    }

    pub(crate) fn package_data_key<'a>(
        &'a self,
        package: &'a PortagePackage,
    ) -> &'a PortagePackage {
        self.host_aliases.get(package).unwrap_or(package)
    }

    pub(crate) fn package_data(&self, package: &PortagePackage) -> Option<&PackageData> {
        self.packages.get(self.package_data_key(package))
    }

    /// Whether the solved `version` of `package` came from widened
    /// candidate supply (outside keyword/mask/license acceptance)
    ///
    /// `false` for anything a strict solve could have selected, and for
    /// unknown package/version pairs.
    pub fn selection_needs_unmask(&self, package: &PortagePackage, version: &Version) -> bool {
        self.package_data(package)
            .is_some_and(|d| d.versions.get(version).is_some_and(|vd| vd.needs_unmask))
    }

    /// Whether the solved `version` of `package` is a live ebuild
    /// (`*9999` shape or `PROPERTIES=live`)
    ///
    /// `false` for unknown package/version pairs.
    pub fn selection_is_live(&self, package: &PortagePackage, version: &Version) -> bool {
        self.package_data(package)
            .is_some_and(|d| d.versions.get(version).is_some_and(|vd| vd.live))
    }

    /// Set whether to include BDEPEND in the resolution
    ///
    /// When `false` (default), BDEPEND are excluded from resolution entirely,
    /// matching emerge's `--with-bdeps=n` default. When `true`, BDEPEND are
    /// included but filtered by `host_installed`.
    pub fn set_with_bdeps(&mut self, with_bdeps: bool) {
        self.with_bdeps = with_bdeps;
    }

    /// Returns the list of dependencies that were dropped during construction
    /// because their target package was not present in the repository
    ///
    /// Each entry is the `(package, version_set)` that could not be resolved.
    /// Callers should inspect this list to detect typos or genuinely missing
    /// packages rather than silently accepting an incomplete solution.
    pub fn dropped_deps(&self) -> &[DroppedDep] {
        &self.dropped_deps
    }

    /// Return all real (non-virtual, non-synthetic) packages in the provider
    /// whose CPN matches `cpn`
    ///
    /// For packages with a single slot this returns one entry; for multi-slot
    /// packages (e.g. `dev-lang/python:3.11`, `dev-lang/python:3.12`) it
    /// returns one entry per slot.  Returns an empty vec if the CPN is not
    /// present in the repository.
    pub fn packages_for_cpn(&self, cpn: &portage_atom::Cpn) -> Vec<PortagePackage> {
        self.packages
            .keys()
            .filter(|p| !p.is_virtual() && p.cpn() == cpn)
            .cloned()
            .collect()
    }

    /// Return all versions registered for a given package, sorted ascending
    pub fn versions_for_pkg(&self, pkg: &PortagePackage) -> Vec<Version> {
        self.package_data(pkg)
            .map(|d| d.versions.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Return the merged dependency requirements for a specific package version,
    /// or `None` if the package/version is not registered
    pub fn deps_for(
        &self,
        pkg: &PortagePackage,
        ver: &Version,
    ) -> Option<Vec<(PortagePackage, PortageVersionSet)>> {
        let data = self.package_data(pkg)?;
        let vd = data.versions.get(ver)?;
        if let Dependencies::Available(reqs) = &vd.merged {
            Some(reqs.iter().cloned().collect::<Vec<_>>())
        } else {
            None
        }
    }

    /// Resolve a set of target packages using PubGrub
    ///
    /// Creates an `__internal__/root` node whose dependencies are the given
    /// `targets`, runs the solver, then strips all `__internal__/` bookkeeping
    /// nodes (root + any USE-flag decision nodes) before returning.
    /// Callers only receive real Gentoo packages.  See `package.rs` for the
    /// full description of the `__internal__/` convention.
    ///
    /// Each target is a `(PortagePackage, PortageVersionSet)` pair, e.g. the
    /// package `dev-libs/openssl` with the version set `>=3.0`.
    #[allow(clippy::result_large_err)]
    pub fn resolve_targets(
        &mut self,
        targets: Vec<(PortagePackage, PortageVersionSet)>,
    ) -> std::result::Result<
        SelectedDependencies<PortagePackage, Version>,
        pubgrub::PubGrubError<Self>,
    > {
        let root = PortagePackage::synthetic_root();
        let root_ver =
            Version::parse("0").expect("version string \"0\" should always parse successfully");

        self.root_targets = targets.iter().cloned().collect();

        // Root targets have no gating flag; merged is derived from by_class.
        let targets_with_flag: Vec<(
            PortagePackage,
            PortageVersionSet,
            Option<Interned<DefaultInterner>>,
        )> = targets.into_iter().map(|(p, vs)| (p, vs, None)).collect();
        let vd =
            VersionData::from_by_class(vec![targets_with_flag, vec![], vec![], vec![], vec![]]);
        let entry = self
            .packages
            .entry(root.clone())
            .or_insert_with(|| PackageData {
                versions: BTreeMap::new(),
            });
        entry.versions.insert(root_ver.clone(), vd);

        // Re-solve to a fixpoint so that any installed package the post-solve
        // pass decides to upgrade (`upgrade_to`) has its *new* version's full
        // dependency closure solved, not just the installed version's runtime
        // deps.  Each iteration pins the upgrades discovered so far (see
        // `upgrade_pins` / `choose_version`) and solves again; new upgrades
        // surfaced by the richer graph feed the next round.  Bounded so a
        // pathological oscillation can't loop forever — on the rare re-solve
        // failure or the bound, we keep the last good solution (the previous
        // sound approximation).
        self.upgrade_pins.clear();
        const MAX_RESOLVE_ITERS: usize = 4;
        let mut solution = pubgrub::resolve(self, root.clone(), root_ver.clone())?;
        // Post-solve: collect USE flag requirements for all packages.  Must run
        // before filtering virtuals because per-branch constraints live in
        // Choice/SlotChoice nodes.
        self.use_flag_requirements = self.compute_use_flag_requirements(&solution);

        for _ in 1..MAX_RESOLVE_ITERS {
            // Pin every upgrade discovered so far; stop once nothing new appears.
            let mut added_pin = false;
            let pins: Vec<(PortagePackage, Version)> = self
                .use_flag_requirements
                .iter()
                .filter_map(|r| r.upgrade_to.clone().map(|v| (r.package.clone(), v)))
                .collect();
            for (pkg, ver) in pins {
                if self.upgrade_pins.get(&pkg) != Some(&ver) {
                    self.upgrade_pins.insert(pkg, ver);
                    added_pin = true;
                }
            }
            if !added_pin {
                break;
            }

            // Re-solve with the new pins.  If it fails (e.g. the upgraded
            // version's deps can't be satisfied), keep the last good solution
            // rather than turning an advisory situation into a hard error.
            match pubgrub::resolve(self, root.clone(), root_ver.clone()) {
                Ok(next) => {
                    solution = next;
                    self.use_flag_requirements = self.compute_use_flag_requirements(&solution);
                }
                Err(_) => break,
            }
        }

        self.packages.remove(&root);

        // Capture the solver's choice for every ceded flag before the virtual
        // UseDecision nodes are stripped from the returned solution.
        self.solved_use_decisions = solution
            .iter()
            .filter(|(p, _)| matches!(p, PortagePackage::UseDecision { .. }))
            .map(|(p, v)| (p.clone(), *v == Version::new(&[1])))
            .collect();

        Ok(solution
            .into_iter()
            .filter(|(p, _)| !p.is_virtual())
            .collect())
    }

    /// The flags the caller ceded to the solver, with the values it chose
    ///
    /// Empty unless the caller emitted `SolverDecided` flags (Level-C). Lets the
    /// caller fold the chosen values back into its effective-USE display and
    /// report any the solver flipped. See `docs/required-use-level-c.md`.
    pub fn solved_use_decisions(&self) -> Vec<CededFlag> {
        let mut out: Vec<CededFlag> = self
            .solved_use_decisions
            .iter()
            .filter_map(|(node, &value)| {
                let (cpn, flag) = self.use_decision_meta.get(node)?;
                let preferred = self
                    .use_decision_prefer
                    .get(node)
                    .map(|v| *v == Version::new(&[1]));
                Some(CededFlag {
                    cpn: *cpn,
                    flag: *flag,
                    value,
                    flipped: preferred != Some(value),
                })
            })
            .collect();
        out.sort_by(|a, b| {
            a.cpn
                .cmp(&b.cpn)
                .then_with(|| a.flag.as_str().cmp(b.flag.as_str()))
        });
        out
    }

    /// Returns true if the deps of `vd` transitively reach any installed CPN,
    /// descending up to `depth` levels through `__internal__/*` virtual packages
    pub(crate) fn deps_reach_installed(&self, vd: &VersionData, depth: u8) -> bool {
        let Dependencies::Available(ref constraints) = vd.merged else {
            return false;
        };
        for (dep_pkg, _) in constraints.iter() {
            if dep_pkg.is_virtual() {
                if depth > 0
                    && let Some(dep_data) = self.packages.get(dep_pkg)
                {
                    for dep_vd in dep_data.versions.values() {
                        if self.deps_reach_installed(dep_vd, depth - 1) {
                            return true;
                        }
                    }
                }
            } else if self.installed_cpns.contains(dep_pkg.cpn()) {
                return true;
            }
        }
        false
    }

    /// Newest installed version reachable one level out of a single version's
    /// merged constraints: a direct dep is looked up in `self.installed`, a
    /// nested `Choice`/`SlotChoice` branch (e.g. a `:*` SlotChoice) is resolved
    /// via [`Self::branch_best_installed`]. `None` when nothing is installed
    ///
    /// Only recurses into `Choice`/`SlotChoice` virtuals — real `||`/`:*`
    /// provider alternatives, which form an acyclic tree down to concrete
    /// packages. Other virtuals (`UseDecision`, `Root`) are *not* recursed
    /// into: `UseDecision` nodes encode `REQUIRED_USE` Level-C constraints
    /// that reference each other symmetrically — recursing into them
    /// overflowed the stack. Only provider alternatives belong here.
    ///
    /// `depth` bounds recursion (see [`Self::branch_best_installed`]) so an
    /// unexpected deep/cyclic shape degrades to "no preference".
    fn branch_installed_ver(&self, vd: &VersionData, depth: u8) -> Option<Version> {
        let Dependencies::Available(ref cs) = vd.merged else {
            return None;
        };
        cs.iter()
            .filter_map(|(p, _)| {
                if matches!(
                    p,
                    PortagePackage::Choice { .. } | PortagePackage::SlotChoice { .. }
                ) {
                    self.branch_best_installed(p, depth)
                } else if p.is_virtual() {
                    None
                } else {
                    self.installed.get(p).map(|(v, _)| v.clone())
                }
            })
            .max()
    }

    /// The newest installed version reachable (one level) through a virtual
    /// `||`-Choice branch — the newest version of the branch's target
    /// package present in `self.installed`, `None` if none is
    ///
    /// Breaks ties when every branch of a provider `||` group is
    /// installed: the branch with the newer version wins (matching
    /// emerge's `dep_zapdeps`).
    ///
    /// `depth` is the remaining recursion budget shared with
    /// [`Self::branch_installed_ver`]; the public entry point
    /// (`newest_installed_choice_branch`) starts it at [`Self::BRANCH_DEPTH_LIMIT`].
    pub(crate) fn branch_best_installed(&self, pkg: &PortagePackage, depth: u8) -> Option<Version> {
        let depth = depth.checked_sub(1)?;
        let data = self.package_data(pkg)?;
        data.versions
            .values()
            .filter_map(|vd| self.branch_installed_ver(vd, depth))
            .max()
    }

    /// Recursion budget for [`Self::branch_installed_ver`]/
    /// [`Self::branch_best_installed`]
    ///
    /// Real `||`/`:*` provider trees are a handful of levels deep at most;
    /// this is generous headroom for that while still bounding a
    /// pathological or unexpectedly cyclic shape.
    const BRANCH_DEPTH_LIMIT: u8 = 16;

    /// For an all-branches-installed provider `||` Choice, return the
    /// candidate branch whose reachable installed version is newest —
    /// emerge's `dep_zapdeps` tie-break (e.g. `|| ( rust-bin:* rust:* )`
    /// with installed source rust-1.95.0 > rust-bin-1.93.1 keeps source
    /// rust)
    ///
    /// Branches may be nested `:*` SlotChoice virtuals or direct real
    /// packages.
    ///
    /// Returns `None` when no candidate exposes an installed version, so
    /// the caller falls back to the default `max()` (= first-listed) pick.
    pub(crate) fn newest_installed_choice_branch<'a>(
        &self,
        data: &PackageData,
        candidates: &[&'a Version],
    ) -> Option<&'a Version> {
        let mut best: Option<&'a Version> = None;
        let mut best_inst_ver: Option<Version> = None;
        for &ver in candidates {
            let Some(vd) = data.versions.get(ver) else {
                continue;
            };
            if let Some(iv) = self.branch_installed_ver(vd, Self::BRANCH_DEPTH_LIMIT) {
                // Prefer a strictly-newer reachable installed version; on a tie
                // keep the higher choice version (= first-listed alternative),
                // matching emerge's `dep_zapdeps`, which only re-picks the
                // provider when the other branch is genuinely newer.
                let better = match best_inst_ver {
                    None => true,
                    Some(ref b) => iv > *b || (iv == *b && best.is_some_and(|bv| ver > bv)),
                };
                if better {
                    best_inst_ver = Some(iv);
                    best = Some(ver);
                }
            }
        }
        best
    }
}

/// Evaluate a single USE dep given the dep's effective state and the parent's
/// flag state (for Conditional/Equal kinds)
///
/// Returns `Some(requires_enabled)` when the constraint fires and is violated,
/// `None` when it is satisfied or the condition does not apply.
fn register_virtual_choices(
    packages: &mut HashMap<PortagePackage, PackageData>,
    choices: Vec<convert::VirtualChoice>,
) {
    for vc in choices {
        let entry = packages.entry(vc.package).or_insert_with(|| PackageData {
            versions: BTreeMap::new(),
        });
        for (ver, deps) in vc.versions {
            // Merge, don't overwrite: a UseDecision node can be produced by both
            // the conditional-dep path and the REQUIRED_USE encoder for the same
            // (cpn, flag). Selecting a version must enforce *all* of its deps.
            match entry.versions.get_mut(&ver) {
                Some(existing) => {
                    existing.by_class[0].extend(deps);
                    existing.merged = Dependencies::Available(
                        existing
                            .by_class
                            .iter()
                            .flatten()
                            .map(|(p, vs, _)| (p.clone(), vs.clone()))
                            .collect(),
                    );
                }
                None => {
                    let vd = VersionData::from_by_class(vec![deps, vec![], vec![], vec![], vec![]]);
                    entry.versions.insert(ver, vd);
                }
            }
        }
        // Attach per-branch USE dep constraints so choose_version() can evaluate
        // which OR-group branch is already satisfied without rebuilds.
        for (ver, udeps) in vc.branch_use_deps {
            if !udeps.is_empty()
                && let Some(vd) = entry.versions.get_mut(&ver)
            {
                vd.use_deps = udeps;
            }
        }
        // Attach per-branch blockers so check_blockers only fires those of the
        // selected OR / UseDecision alternative.
        for (ver, blockers) in vc.branch_blockers {
            if !blockers.is_empty()
                && let Some(vd) = entry.versions.get_mut(&ver)
            {
                vd.blockers.extend(blockers);
            }
        }
    }
}

/// Mark a virtual `Choice`/`SlotChoice` branch as needing an unmask when
/// every way of satisfying it does
///
/// A branch is tagged when any of its constraint targets has no untagged
/// candidate in range (a real package whose accepted set is empty there, or a
/// nested virtual that is itself fully tagged). Iterated to a fixpoint so the
/// tag propagates outward through nested `||`-over-slot-choice shapes. Without
/// this, a first-listed branch satisfiable only via masked versions wins
/// `choose_version`'s `max()` silently — both branches are satisfiable, so
/// PubGrub never backtracks into the untagged one.
type VirtualBranch = (
    PortagePackage,
    Version,
    Vec<(PortagePackage, PortageVersionSet)>,
);

fn propagate_needs_unmask(packages: &mut HashMap<PortagePackage, PackageData>) {
    let mut branches: Vec<VirtualBranch> = Vec::new();
    for (pkg, data) in packages.iter() {
        if !matches!(
            pkg,
            PortagePackage::Choice { .. } | PortagePackage::SlotChoice { .. }
        ) {
            continue;
        }
        for (ver, vd) in &data.versions {
            if let Dependencies::Available(cs) = &vd.merged {
                let targets: Vec<_> = cs.iter().map(|(p, vs)| (p.clone(), vs.clone())).collect();
                // The allow-none branch (no deps) always stays untagged.
                if !targets.is_empty() {
                    branches.push((pkg.clone(), ver.clone(), targets));
                }
            }
        }
    }

    loop {
        let mut changed = false;
        for (node, ver, targets) in &branches {
            let tagged = targets.iter().any(|(tpkg, tvs)| {
                packages.get(tpkg).is_none_or(|d| {
                    !d.versions
                        .iter()
                        .any(|(v, vd)| tvs.contains(v) && !vd.needs_unmask)
                })
            });
            if let Some(data) = packages.get_mut(node)
                && let Some(vd) = data.versions.get_mut(ver)
                && vd.needs_unmask != tagged
            {
                vd.needs_unmask = tagged;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

#[cfg(test)]
mod tests;
