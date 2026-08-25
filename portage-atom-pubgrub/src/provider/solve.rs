//! The PubGrub `DependencyProvider` implementation: version prioritisation,
//! version choice (installed-preference heuristics), and dependency lookup

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use portage_atom::{UseDefault, Version};
use pubgrub::{
    Dependencies, DependencyConstraints, DependencyProvider, PackageResolutionStatistics,
};

use crate::error::Error;
use crate::package::{MergeRoot, PortagePackage};
use crate::use_config::UseFlagState;
use crate::version_set::PortageVersionSet;

use super::post_solve::eval_violated_use_dep;
use super::{HostEntry, InstalledPolicy, PortageDependencyProvider, VersionData};

impl DependencyProvider for PortageDependencyProvider {
    type P = PortagePackage;
    type V = Version;
    type VS = PortageVersionSet;
    type M = String;
    type Err = Error;
    // `(conflict_count, is_root_target, Reverse(version_count))`: conflict
    // count still dominates pubgrub's own backtracking signal, but among
    // ties a root target is decided before its dependencies — matching
    // emerge's argument semantics (a named atom pulls the best version,
    // deps bend around it).
    type Priority = (u32, bool, Reverse<usize>);

    fn prioritize(
        &self,
        package: &Self::P,
        range: &Self::VS,
        stats: &PackageResolutionStatistics,
    ) -> Self::Priority {
        let count = self
            .package_data(package)
            .map(|d| d.versions.keys().filter(|v| range.contains(v)).count())
            .unwrap_or(0);
        if count == 0 {
            return (u32::MAX, true, Reverse(0));
        }
        (
            stats.conflict_count(),
            self.root_targets.contains_key(package),
            Reverse(count),
        )
    }

    fn choose_version(
        &self,
        package: &Self::P,
        range: &Self::VS,
    ) -> std::result::Result<Option<Self::V>, Self::Err> {
        let Some(data) = self.package_data(package) else {
            return Ok(None);
        };

        let candidates: Vec<&Version> =
            data.versions.keys().filter(|v| range.contains(v)).collect();

        if candidates.is_empty() {
            return Ok(None);
        }

        // Widened candidate-supply tiering (`--autounmask`-style solves):
        // untagged candidates always beat tagged ones, and among tagged ones a
        // release ebuild beats a live `.9999` (real portage prefers live under
        // `**`; em deliberately doesn't — crossdev's own accept_keywords pins
        // already keep live ebuilds out where that matters). Filtering here,
        // before every heuristic below, means the installed-preference and
        // OR-branch paths all operate on the preferred tier for free.
        let candidates = filter_to_preferred_tier(data, candidates);

        // A prior solve iteration decided to upgrade this installed package to a
        // newer version (`upgrade_to`).  Pin it so the solver actually selects
        // that version — and therefore re-solves its dependency closure — rather
        // than favouring the installed version again.  If the pinned version is
        // out of range for this particular constraint, fall through to the
        // normal logic.
        if let Some(pin) = self.upgrade_pins.get(package)
            && range.contains(pin)
        {
            return Ok(Some(pin.clone()));
        }

        // Ceded USE flags: bias a `UseDecision` node toward the caller's
        // preferred value, so a `SolverDecided` flag keeps its configured value
        // unless a `REQUIRED_USE` constraint narrows `range` away from it. When
        // the preference is out of range the constraint has forced a flip; fall
        // through to the normal pick (the other version).
        if matches!(package, PortagePackage::UseDecision { .. })
            && let Some(pref) = self.use_decision_prefer.get(package)
            && range.contains(pref)
        {
            return Ok(Some(pref.clone()));
        }

        if let Some((installed_ver, policy)) = self.installed.get(package) {
            match policy {
                InstalledPolicy::Lock => {
                    if range.contains(installed_ver) {
                        return Ok(Some(installed_ver.clone()));
                    }
                    return Ok(None);
                }
                InstalledPolicy::Favor | InstalledPolicy::Provided => {
                    // Explicit targets are not favored: a named argument pulls
                    // the best accepted version (emerge argument semantics).
                    // Otherwise keep the installed version whenever it satisfies
                    // the constraint — including when its exact cpv was pruned
                    // from the tree (e.g. a revbump `4.3.3` -> `4.3.3-r1`). Under
                    // Favor (non-update) emerge keeps such an installed dep
                    // rather than pulling the newer build; the empty-deps
                    // installed stub is fine since the package is satisfying a
                    // dep, not being rebuilt.
                    //
                    // `prefer_update` (`-uD`): fall through to newest in-range
                    // for the whole solve (transitive in-slot upgrades).
                    // `Rebuild` / root targets likewise take the newest via
                    // fall-through — unless the resolve is selective without
                    // `--update`, where a satisfied target keeps what it has.
                    if !self.prefer_update
                        && (self.selective_no_update || !self.root_targets.contains_key(package))
                        && range.contains(installed_ver)
                    {
                        return Ok(Some(installed_ver.clone()));
                    }
                }
                InstalledPolicy::Rebuild => {
                    // Emptytree / `-uD`: fall through to newest in-range.
                    // `-N`/`-U` alone: same-CPV reinstall when the installed
                    // version is still available (emerge `[R]`); only pick a
                    // newer CPV if the installed one left the tree.
                    if !self.rebuild_tree && !self.prefer_update && range.contains(installed_ver) {
                        return Ok(Some(installed_ver.clone()));
                    }
                }
            }
        }

        // `--deep` / native emptytree: for a `:*` any-slot dep (`SlotChoice`),
        // bump to the newest slot instead of keeping a satisfying installed slot
        // — matching `emerge -uD`/`-e` (e.g. firefox pulling the newest
        // `dev-lang/rust-bin` slot). Slots are version-ranked, so the `max()`
        // pick below is the newest-*version* slot (never an older compat slot
        // like `app-shells/bash:5.1`). Scoped to `SlotChoice` only: `Choice`
        // (provider OR-groups) keeps the installed-branch / USE-dep preference so
        // we don't gratuitously re-pick providers (e.g. rust-bin vs source rust).
        let bump_slot =
            self.prefer_newest_slot && matches!(package, PortagePackage::SlotChoice { .. });

        // For OR-group / slot-choice packages, prefer branches that lead to
        // an already-installed package.  Independent of `rebuild_tree`: emptytree
        // rebuilds every listed package but `gcc:*` must still bind to the
        // installed/newest slot.  SlotChoice nodes number slots i+1 (newest slot
        // last/highest); Choice nodes use n-i (first-listed highest).
        if !bump_slot && package.is_virtual() && !self.installed_cpns.is_empty() {
            // Check each candidate directly against self.installed.
            // deps_reach_installed only checks CPNs, which produces false positives
            // for multi-slot packages (every slot appears "installed" if any slot
            // is), causing the heuristic to never fire and the solver to fall
            // back to the default max() pick.
            let direct_installed: Vec<bool> = candidates
                .iter()
                .map(|&ver| {
                    data.versions.get(ver).is_some_and(|vd| {
                        if let Dependencies::Available(ref cs) = vd.merged {
                            cs.iter().any(|(pkg, _)| self.installed.contains_key(pkg))
                        } else {
                            false
                        }
                    })
                })
                .collect();
            let directly_installed_count = direct_installed.iter().filter(|&&x| x).count();

            if directly_installed_count > 0 {
                // For OR-group Choice packages: among the installed branches, prefer
                // those that already satisfy all USE dep constraints.  A branch that
                // is installed AND use-satisfied avoids unnecessary package rebuilds.
                //
                // Example: librsvg BDEPEND has || ( (python:3.14 docutils[p3.14(-)]) ... )
                // Both python:3.14 and python:3.13 are installed, so both branches pass
                // the simple installed-preference check and we'd fall through to max()
                // (= first listed, python:3.14).  But if docutils only has p3.13 enabled,
                // the p3.14 branch's USE dep is unsatisfied — we should pick p3.13 instead.
                if matches!(package, PortagePackage::Choice { .. }) {
                    let installed_and_use_sat: Vec<bool> = candidates
                        .iter()
                        .zip(direct_installed.iter())
                        .map(|(&ver, &inst)| {
                            inst && data
                                .versions
                                .get(ver)
                                .map(|vd| self.use_dep_branch_satisfied(&vd.use_deps))
                                .unwrap_or(true)
                        })
                        .collect();
                    let sat_count = installed_and_use_sat.iter().filter(|&&s| s).count();
                    // Only intervene when some (not all) installed branches satisfy USE
                    // deps — if none satisfy, we can't do better so fall through.
                    if sat_count > 0 && sat_count < candidates.len() {
                        let best = candidates
                            .iter()
                            .copied()
                            .zip(installed_and_use_sat.iter().copied())
                            .filter(|(_, s)| *s)
                            .map(|(v, _)| v)
                            .max()
                            .cloned();
                        return Ok(best);
                    }
                }

                if directly_installed_count < candidates.len() {
                    // OR group with some (not all) branches installed: prefer
                    // the highest-version installed branch (= first listed).
                    let best = candidates
                        .into_iter()
                        .zip(direct_installed)
                        .filter(|(_, has)| *has)
                        .map(|(v, _)| v)
                        .max()
                        .cloned();
                    return Ok(best);
                }
                // All direct (non-virtual) branches installed: prefer the branch
                // whose installed version is newest (emerge `dep_zapdeps`
                // tie-break) before falling to the default max() (= first listed).
                if matches!(package, PortagePackage::Choice { .. })
                    && let Some(ver) = self.newest_installed_choice_branch(data, &candidates)
                {
                    return Ok(Some(ver.clone()));
                }
                // Fall through to default max() pick (= first listed alternative).
            }

            // No directly-installed branch found; fall back to CPN-level
            // heuristic for nested OR groups with non-direct install paths.
            let has_installed: Vec<bool> = candidates
                .iter()
                .map(|&ver| {
                    data.versions
                        .get(ver)
                        .is_some_and(|vd| self.deps_reach_installed(vd, 2))
                })
                .collect();
            let installed_count = has_installed.iter().filter(|&&x| x).count();
            if installed_count > 0 {
                if installed_count < candidates.len() {
                    let best = candidates
                        .iter()
                        .copied()
                        .zip(has_installed)
                        .filter(|(_, has)| *has)
                        .map(|(v, _)| v)
                        .max()
                        .cloned();
                    return Ok(best);
                }
                // All branches reach an installed package (e.g. the host has both
                // rust and rust-bin, so `|| ( rust-bin:* rust:* )` — a Choice over
                // nested `:*` SlotChoice virtuals — has every branch installed).
                // Don't fall to blind max() (= first listed → rust-bin [NS]); use
                // emerge's `dep_zapdeps` version-aware tie-break and keep the
                // branch reaching the newer installed version (source rust-1.95.0).
                if matches!(package, PortagePackage::Choice { .. })
                    && let Some(ver) = self.newest_installed_choice_branch(data, &candidates)
                {
                    return Ok(Some(ver.clone()));
                }
            }
        }

        let version = candidates.into_iter().max().cloned();
        Ok(version)
    }

    fn get_dependencies(
        &self,
        package: &Self::P,
        version: &Self::V,
    ) -> std::result::Result<Dependencies<Self::P, Self::VS, Self::M>, Self::Err> {
        Ok(self.compute_dependencies(package, version))
    }
}

impl PortageDependencyProvider {
    fn compute_dependencies(
        &self,
        package: &PortagePackage,
        version: &Version,
    ) -> Dependencies<PortagePackage, PortageVersionSet, String> {
        let Some(data) = self.package_data(package) else {
            return Dependencies::Unavailable(format!("package not found: {}", package));
        };
        let Some(vd) = data.versions.get(version) else {
            return Dependencies::Unavailable(format!(
                "version not found: {}@{}",
                package, version
            ));
        };

        // `--nodeps`: a real package reports no dependencies, so only the
        // explicitly named targets (the synthetic root's deps) enter the plan.
        // The root is virtual, so its target list is untouched.
        if self.nodeps && !package.is_virtual() {
            return Dependencies::Available(DependencyConstraints::default());
        }

        // For installed packages at their installed version, skip build-time
        // deps (DEPEND = index 0, BDEPEND = index 2).  The package is already
        // built; re-solving its build deps would drag in bootstrap toolchain
        // packages (old gcc to build new gcc, etc.) that portage never shows.
        // Only RDEPEND (1), PDEPEND (3), and IDEPEND (4) matter at install time.
        // `--emptytree` (`InstalledPolicy::Rebuild`) always expands the full
        // build-time closure even when the selected version matches the VDB.
        if let Some((inst, policy)) = self.installed.get(package)
            && inst == version
            && !matches!(policy, InstalledPolicy::Rebuild)
        {
            // `package.provided`: no real VDB record backs this, so unlike a
            // genuinely installed package there is nothing to trust past "the
            // system claims this exists" — stop here, not even RDEPEND.
            if matches!(policy, InstalledPolicy::Provided) {
                return Dependencies::Available(DependencyConstraints::default());
            }
            if self.cross_active && package.merge_root() == MergeRoot::Target {
                // Already installed and kept (not rebuilt): mirror the native
                // equivalent below (`runtime`), which likewise omits BDEPEND
                // for this case — only RDEPEND/PDEPEND/IDEPEND matter once a
                // package is already built and staying that way.
                return Dependencies::Available(cross_target_runtime_deps(
                    self,
                    vd,
                    &self.sysroot_installed,
                    target_drops_depend(self.root_deps_rdeps, package),
                    false,
                ));
            }
            let runtime: DependencyConstraints<PortagePackage, PortageVersionSet> = vd
                .rdepend()
                .iter()
                .chain(vd.pdepend())
                .chain(vd.idepend())
                .map(|(p, vs, _)| (p.clone(), vs.clone()))
                .collect();
            return Dependencies::Available(runtime);
        }

        // Native `--emptytree` (`rebuild_tree`): list the **full deep closure** with
        // real edges. Do NOT broot-prune host-satisfied build deps — under emptytree
        // the host (BROOT) seed is for bootstrap version choice, ordering/cycle-break
        // and action tags, never for membership. `emptytree_native` is `!cross.active`,
        // so this precedes the cross paths.
        if self.rebuild_tree {
            return vd.merged.clone();
        }

        // A package being *built* (not at its installed version):
        //
        // `self.is_cross_arch`, not just `cross_active`: `cross_active` is
        // also on for a same-arch offset build (`--root <dir>`), which needs
        // the dual-root `(package, merge_root)` bookkeeping but NOT this
        // branch's "keep DEPEND unconditionally" treatment (that's only
        // correct when DEPEND genuinely means "the *target* sysroot's own
        // headers/libs", i.e. real cross-compilation). A same-arch offset
        // build falls through to `broot_filtered` below instead, which drops
        // host-satisfied DEPEND the same way BDEPEND/IDEPEND already are.
        if self.cross_active && self.is_cross_arch && package.merge_root() == MergeRoot::Target {
            // A built package's BDEPEND is strictly required to build it, so
            // (mirroring `broot_filtered`'s native equivalent) `--with-bdeps`
            // does not gate it here — only the *installed-and-kept* branch
            // above does. Cross `-p` never expands BDEPEND *onto* ROOT (the
            // edge always stamps a Host-root node, never merges into the
            // target sysroot); unsatisfied BDEPEND schedules there instead.
            return Dependencies::Available(cross_target_runtime_deps(
                self,
                vd,
                &self.sysroot_installed,
                target_drops_depend(self.root_deps_rdeps, package),
                true,
            ));
        }
        if self.cross_active && package.merge_root() == MergeRoot::Host && self.with_bdeps {
            return Dependencies::Available(host_native_deps(self, vd));
        }

        // A package being *built* always pulls its BDEPEND/IDEPEND, minus
        // edges already satisfied on BROOT (the host) — build deps are
        // strictly required, so `--with-bdeps` doesn't gate them (that flag
        // only governs installed-and-*kept* packages' BDEPEND, already
        // excluded by the runtime-only branch above). DEPEND/RDEPEND
        // unaffected.
        //
        // `!package.is_virtual()`: the synthetic solver root also flows
        // through here and carries the user's requested target atoms in its
        // own "DEPEND" slot (see `target_drops_depend`'s doc comment on the
        // same footgun for the cross path) — host-satisfaction filtering
        // there would drop a requested atom whenever the host already has
        // it (`em --root <dir> sys-devel/gcc` on a host with gcc already
        // installed), collapsing the plan to 0 packages instead of adding it.
        if !package.is_virtual() && !self.host_installed.is_empty() {
            return Dependencies::Available(broot_filtered(self, vd));
        }
        vd.merged.clone()
    }
}

/// Rebuild a version's merged constraints with host-satisfied BDEPEND edges
/// dropped
///
/// `host_installed` maps a package to a present-on-BROOT version; a
/// BDEPEND edge `(pkg, vset)` is dropped when `pkg` is present and `vset`
/// accepts that version. Per-edge (not per-package): a package that is both a
/// BDEPEND of A and an RDEPEND of B is still built when B needs it.
fn stamp_root(p: &PortagePackage, root: MergeRoot) -> PortagePackage {
    p.at_merge_root(root)
}

/// Whether this Target node drops its `DEPEND` under `--root-deps=rdeps`
///
/// Guards the footgun: the synthetic solver root also reports
/// [`MergeRoot::Target`] (the enum default for non-real nodes) and carries the
/// requested target seeds in its `DEPEND` slot — so dropping `DEPEND` on a
/// *virtual* node would discard the user's targets and collapse the solve. Real
/// target packages drop `DEPEND`; the root never does.
fn target_drops_depend(rdeps: bool, package: &PortagePackage) -> bool {
    rdeps && !package.is_virtual()
}

/// How [`stamped_deps`] routes a package's dependency classes
///
/// The two axes the stamp routes ([`cross_target_runtime_deps`] /
/// [`host_native_deps`]) differ along, kept in one place so they can't
/// drift apart on the next IDEPEND/BDEPEND shift. [`broot_filtered`] is a
/// host-satisfaction *filter*, not a stamp, and deliberately does not
/// share this.
struct DepStampPolicy {
    /// Stamp applied to DEPEND/RDEPEND/PDEPEND (the runtime classes)
    runtime_stamp: MergeRoot,
    /// Stamp applied to an *unsatisfied* BDEPEND/IDEPEND edge — it merges there
    broot_unsatisfied_stamp: MergeRoot,
    /// Include DEPEND at all (cross `--root-deps=rdeps` drops it)
    include_depend: bool,
    /// Include BDEPEND (cross gates it on the caller's `--with-bdeps`-shaped flag)
    include_bdepend: bool,
}

/// Shared body of the two "stamp every runtime dep + schedule unsatisfied
/// build/install deps onto BROOT" routes
///
/// Runtime classes (DEPEND/RDEPEND/PDEPEND) stamp to
/// [`DepStampPolicy::runtime_stamp`]; an unsatisfied BDEPEND (when
/// `include_bdepend`) or IDEPEND merges to
/// [`DepStampPolicy::broot_unsatisfied_stamp`].
///
/// BDEPEND resolves on BROOT (the host), never the target sysroot — kept
/// central here after a live bug where one route omitted it entirely, so a
/// target package's unsatisfied BDEPEND (e.g. systemd-utils needing jinja2
/// built for a python target the host's installed jinja2 lacked) never
/// scheduled a rebuild and the package's own configure/build then failed.
fn stamped_deps(
    provider: &PortageDependencyProvider,
    vd: &VersionData,
    policy: DepStampPolicy,
) -> DependencyConstraints<PortagePackage, PortageVersionSet> {
    let depend = policy
        .include_depend
        .then(|| vd.depend().iter())
        .into_iter()
        .flatten();
    let mut out: Vec<(PortagePackage, PortageVersionSet)> = depend
        .chain(vd.rdepend())
        .chain(vd.pdepend())
        .map(|(p, vs, _)| (stamp_root(p, policy.runtime_stamp), vs.clone()))
        .collect();
    if policy.include_bdepend {
        append_unsatisfied_broot(
            &mut out,
            vd.bdepend(),
            provider,
            vd,
            policy.broot_unsatisfied_stamp,
        );
    }
    append_unsatisfied_broot(
        &mut out,
        vd.idepend(),
        provider,
        vd,
        policy.broot_unsatisfied_stamp,
    );
    out.into_iter().collect()
}

/// Cross-arch target build: runtime deps stamp to the target sysroot;
/// `--root-deps=rdeps` drops DEPEND; unsatisfied BDEPEND/IDEPEND schedule onto
/// the host (BROOT)
///
/// See [`stamped_deps`] for the BDEPEND-on-BROOT rationale.
fn cross_target_runtime_deps(
    provider: &PortageDependencyProvider,
    vd: &VersionData,
    _sysroot_installed: &HashMap<PortagePackage, Version>,
    root_deps_rdeps: bool,
    include_bdepend: bool,
) -> DependencyConstraints<PortagePackage, PortageVersionSet> {
    stamped_deps(
        provider,
        vd,
        DepStampPolicy {
            runtime_stamp: MergeRoot::Target,
            broot_unsatisfied_stamp: MergeRoot::Host,
            include_depend: !root_deps_rdeps,
            include_bdepend,
        },
    )
}

/// Host-root native build (BDEPEND front-matter): all deps target the host
/// instance; unsatisfied BDEPEND/IDEPEND also schedule onto the host
fn host_native_deps(
    provider: &PortageDependencyProvider,
    vd: &VersionData,
) -> DependencyConstraints<PortagePackage, PortageVersionSet> {
    stamped_deps(
        provider,
        vd,
        DepStampPolicy {
            runtime_stamp: MergeRoot::Host,
            broot_unsatisfied_stamp: MergeRoot::Host,
            include_depend: true,
            include_bdepend: true,
        },
    )
}

/// Native build: keep RDEPEND/PDEPEND; drop host-satisfied DEPEND, BDEPEND,
/// and IDEPEND — for a native (same-arch) build there's no build sysroot
/// distinct from the host when `CBUILD==CHOST`, so DEPEND is satisfied by
/// whatever machine does the actual compiling, same as BDEPEND
///
/// See [the gcc→perl→rsync explosion this
/// closed](../../docs/design/root-topology.md) for why DEPEND had to join
/// BDEPEND/IDEPEND's host-satisfied filtering.
fn broot_filtered(
    provider: &PortageDependencyProvider,
    vd: &VersionData,
) -> DependencyConstraints<PortagePackage, PortageVersionSet> {
    let mut out: Vec<(PortagePackage, PortageVersionSet)> = vd
        .rdepend()
        .iter()
        .chain(vd.pdepend())
        .map(|(p, vs, _)| (p.clone(), vs.clone()))
        .collect();
    // `-uD` (`prefer_update`): keep *all* host-satisfied build edges so in-slot
    // upgrades can select them. `--newuse` alone does **not** re-inject
    // host-satisfied BDEPEND (Portage leaves those off the merge list; USE-deps
    // on atoms still fail host satisfaction when an impl is missing).
    if provider.prefer_update {
        for (p, vs, _) in vd.depend().iter().chain(vd.bdepend()).chain(vd.idepend()) {
            out.push((p.clone(), vs.clone()));
        }
    } else {
        append_unsatisfied_broot(&mut out, vd.depend(), provider, vd, MergeRoot::Target);
        append_unsatisfied_broot(&mut out, vd.bdepend(), provider, vd, MergeRoot::Target);
        append_unsatisfied_broot(&mut out, vd.idepend(), provider, vd, MergeRoot::Target);
    }
    out.into_iter().collect()
}

/// Whether the host (BROOT) satisfies a dependency edge `(p, vs)`: the host
/// instance must accept the version **and** its current USE must satisfy
/// every atom USE-dependency on that edge
///
/// A `[flag]` the host lacks is not satisfied — portage rebuilds the
/// package with the new USE, pulling its re-evaluated USE-conditional
/// closure (PMS §8.3 atom USE-deps). The parent (`vd`) supplies the
/// parent-flag state for `[flag?]`/`[flag=]` kinds.
///
/// `p` a `Choice`/`SlotChoice` virtual node (an `||`/`^^`/`??` OR-group or a
/// `:*` slot-star group) delegates to [`virtual_satisfied_on_broot`]: the
/// edge is satisfied when *any* branch is. Before this, a virtual target was
/// never a `host_installed` key, so every such edge became an unconditional
/// constraint, bypassing host-satisfaction — ~123 stray packages reached
/// only through a Choice/SlotChoice node whose own edge was never checked.
///
/// `Root`/`UseDecision` are excluded (never virtual-satisfiable): they
/// aren't a real installable alternative, and REQUIRED_USE/ceding machinery
/// must keep deciding them, not have them silently treated as "the host
/// already has it".
fn host_satisfied_on_broot(
    provider: &PortageDependencyProvider,
    vd: &VersionData,
    p: &PortagePackage,
    vs: &PortageVersionSet,
) -> bool {
    let mut seen = HashSet::new();
    host_satisfied_on_broot_inner(provider, vd, p, vs, &mut seen)
}

fn host_satisfied_on_broot_inner(
    provider: &PortageDependencyProvider,
    vd: &VersionData,
    p: &PortagePackage,
    vs: &PortageVersionSet,
    seen: &mut HashSet<PortagePackage>,
) -> bool {
    if matches!(
        p,
        PortagePackage::Choice { .. } | PortagePackage::SlotChoice { .. }
    ) {
        return virtual_satisfied_on_broot(provider, p, seen);
    }
    if p.is_virtual() {
        // Root/UseDecision.
        return false;
    }
    let hp = stamp_root(p, MergeRoot::Host);
    let Some(entry) = provider.host_installed.get(&hp) else {
        return false;
    };
    if !vs.contains(&entry.version) {
        return false;
    }
    vd.use_deps
        .iter()
        .filter(|c| c.target.0 == *p)
        .flat_map(|c| c.use_deps.iter())
        .all(|ud| host_use_dep_satisfied(vd, entry, ud))
}

/// Whether *some* branch of a `Choice`/`SlotChoice` virtual node is fully
/// host-satisfied: every one of that branch's own dependency edges (all
/// classes collapse into one list, see `register_virtual_choices`) is
/// itself `host_satisfied_on_broot_inner`, recursing for a nested
/// Choice/SlotChoice
///
/// `seen` guards against a pathological self-referential choice graph — a
/// revisited node is conservatively unsatisfied.
fn virtual_satisfied_on_broot(
    provider: &PortageDependencyProvider,
    choice: &PortagePackage,
    seen: &mut HashSet<PortagePackage>,
) -> bool {
    if !seen.insert(choice.clone()) {
        return false;
    }
    let satisfied = provider.package_data(choice).is_some_and(|data| {
        data.versions.values().any(|branch_vd| {
            branch_vd.depend().iter().all(|(bp, bvs, _)| {
                host_satisfied_on_broot_inner(provider, branch_vd, bp, bvs, seen)
            })
        })
    });
    seen.remove(choice);
    satisfied
}

/// Whether a single atom USE-dep is satisfied by the host instance's current
/// USE (the host is not rebuilt, so only its active USE — plus the atom's
/// `(+)`/`(-)` default for flags absent from IUSE — counts)
///
/// Reuses the solver's own violation predicate so the host check matches
/// post-solve validation.
fn host_use_dep_satisfied(vd: &VersionData, entry: &HostEntry, ud: &portage_atom::UseDep) -> bool {
    let flag_in_host_iuse = entry.iuse.contains(&ud.flag);
    let dep_effective_enabled = if flag_in_host_iuse {
        entry.active_use.contains(&ud.flag)
    } else {
        // Flag absent from host IUSE: honour the atom's (+)/(-) default; with no
        // default a `[flag]` is a PMS error — treat as unmet so the edge is kept.
        matches!(ud.default, Some(UseDefault::Enabled))
    };
    let parent_flag_enabled = matches!(vd.desired.get(ud.flag), UseFlagState::Enabled);
    eval_violated_use_dep(ud.kind, dep_effective_enabled, parent_flag_enabled).is_none()
}

fn append_unsatisfied_broot(
    out: &mut Vec<(PortagePackage, PortageVersionSet)>,
    edges: &[crate::convert::Req],
    provider: &PortageDependencyProvider,
    vd: &VersionData,
    unsatisfied_root: MergeRoot,
) {
    for (p, vs, _) in edges {
        if !host_satisfied_on_broot(provider, vd, p, vs) {
            out.push((stamp_root(p, unsatisfied_root), vs.clone()));
        }
    }
}

/// Restrict `candidates` to the preferred acceptance tier (see
/// `choose_version`): untagged versions when any exist, else tagged release
/// versions over tagged live ones.
fn filter_to_preferred_tier<'a>(
    data: &super::PackageData,
    candidates: Vec<&'a Version>,
) -> Vec<&'a Version> {
    let untagged: Vec<&Version> = candidates
        .iter()
        .copied()
        .filter(|v| !data.versions[*v].needs_unmask)
        .collect();
    if !untagged.is_empty() {
        return if untagged.len() == candidates.len() {
            candidates
        } else {
            untagged
        };
    }
    let release_only: Vec<&Version> = candidates.iter().copied().filter(|v| !is_live(v)).collect();
    if release_only.is_empty() || release_only.len() == candidates.len() {
        candidates
    } else {
        release_only
    }
}

/// Portage's live-ebuild convention: a `.9999` final version component
fn is_live(v: &Version) -> bool {
    v.numbers.last() == Some(&9999)
}
