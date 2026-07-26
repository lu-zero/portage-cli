//! The PubGrub `DependencyProvider` implementation: version prioritisation,
//! version choice (installed-preference heuristics), and dependency lookup.

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
    type Priority = (u32, Reverse<usize>);

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
        (stats.conflict_count(), Reverse(count))
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
                InstalledPolicy::Favor => {
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
                        && (self.selective_no_update || !self.root_targets.contains(package))
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
        let deps = self.compute_dependencies(package, version);
        // Drop edges the system provides externally (`package.provided`) so the
        // provided package is neither built nor reported as a dropped dep. No-op
        // (and no allocation) when nothing is provided.
        if self.provided.is_empty() {
            return Ok(deps);
        }
        Ok(match deps {
            Dependencies::Available(cs) => Dependencies::Available(
                cs.into_iter()
                    .filter(|(pkg, vs)| !self.edge_is_provided(pkg, vs))
                    .collect(),
            ),
            unavailable => unavailable,
        })
    }
}

impl PortageDependencyProvider {
    /// The unfiltered dependency computation for a `(package, version)` node.
    /// [`get_dependencies`](DependencyProvider::get_dependencies) wraps this to
    /// drop `package.provided` edges.
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
        if self.installed.get(package).is_some_and(|(inst, policy)| {
            inst == version && !matches!(policy, InstalledPolicy::Rebuild)
        }) {
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
        // so this precedes the cross paths. See todo/em-emptytree.md "AGREED REDESIGN".
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
        // Found 2026-07-11: without this, `em --root <dir> sys-devel/gcc`
        // pulled 127 packages (perl, portage, gnupg, eselect, rsync, ...)
        // where real `ROOT=<dir> emerge sys-devel/gcc` pulls 16 — see
        // `todo/root-topology-refactor.md`.
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

        // A package being *built* always pulls its BDEPEND/IDEPEND, minus the
        // edges already satisfied on BROOT (the host). Its build deps are
        // strictly required to build it, so `--with-bdeps` does not gate them:
        // emerge likewise pulls a built package's BDEPEND even under
        // `--with-bdeps=n` (that flag governs only the BDEPEND of installed-and-
        // *kept* packages, which the runtime-only branch above already excludes).
        // Host-satisfied edges are dropped so an offset build (`--root <empty>`)
        // does not re-pull host-provided build/install tools. DEPEND/RDEPEND are
        // unaffected.
        // `!package.is_virtual()`: the synthetic solver root also flows
        // through here (nothing else catches it once cross/emptytree/
        // installed-and-kept are ruled out) and carries the user's requested
        // target atoms in its own "DEPEND" slot (see `target_drops_depend`'s
        // doc comment on the same footgun for the cross path). Applying
        // host-satisfaction filtering there would drop a requested atom
        // outright whenever the *host* happens to already have it installed
        // (e.g. `em --root <dir> sys-devel/gcc` on a host that already has
        // gcc) — collapsing the plan to 0 packages instead of adding gcc to
        // the target. Found 2026-07-11, see `todo/root-topology-refactor.md`.
        if !package.is_virtual() && !self.host_installed.is_empty() {
            return Dependencies::Available(broot_filtered(self, vd));
        }
        vd.merged.clone()
    }
}

/// Rebuild a version's merged constraints with host-satisfied BDEPEND edges
/// dropped. `host_installed` maps a package to a present-on-BROOT version; a
/// BDEPEND edge `(pkg, vset)` is dropped when `pkg` is present and `vset`
/// accepts that version. Per-edge (not per-package): a package that is both a
/// BDEPEND of A and an RDEPEND of B is still built when B needs it.
fn stamp_root(p: &PortagePackage, root: MergeRoot) -> PortagePackage {
    p.at_merge_root(root)
}

/// Whether this Target node drops its `DEPEND` under `--root-deps=rdeps`.
///
/// Guards the footgun: the synthetic solver root also reports
/// [`MergeRoot::Target`] (the enum default for non-real nodes) and carries the
/// requested target seeds in its `DEPEND` slot — so dropping `DEPEND` on a
/// *virtual* node would discard the user's targets and collapse the solve. Real
/// target packages drop `DEPEND`; the root never does.
fn target_drops_depend(rdeps: bool, package: &PortagePackage) -> bool {
    rdeps && !package.is_virtual()
}

fn cross_target_runtime_deps(
    provider: &PortageDependencyProvider,
    vd: &VersionData,
    _sysroot_installed: &HashMap<PortagePackage, Version>,
    root_deps_rdeps: bool,
    include_bdepend: bool,
) -> DependencyConstraints<PortagePackage, PortageVersionSet> {
    // `--root-deps=rdeps` (cross-arch): discard `DEPEND` (class 0) from the
    // sysroot graph entirely — the cross toolchain + the `RDEPEND` libraries
    // already in the sysroot cover build-time needs, and a target build dep
    // cannot install onto the host (wrong arch). Default (offset/same-arch):
    // keep `DEPEND` → target ROOT.
    let depend = (!root_deps_rdeps)
        .then(|| vd.depend().iter())
        .into_iter()
        .flatten();
    let mut out: Vec<(PortagePackage, PortageVersionSet)> = depend
        .chain(vd.rdepend())
        .chain(vd.pdepend())
        .map(|(p, vs, _)| (stamp_root(p, MergeRoot::Target), vs.clone()))
        .collect();
    // BDEPEND resolves on BROOT (the host), never the target sysroot — found
    // live: this call omitted it entirely, so a target package's unsatisfied
    // BDEPEND (e.g. sys-apps/systemd-utils needing dev-python/jinja2 built for
    // a python target the host's installed jinja2 lacked) never scheduled a
    // rebuild; the package's own configure/build then failed instead. See
    // todo/stage-build-shakeout.md.
    if include_bdepend {
        append_unsatisfied_broot(&mut out, vd.bdepend(), provider, vd, MergeRoot::Host);
    }
    append_unsatisfied_broot(&mut out, vd.idepend(), provider, vd, MergeRoot::Host);
    out.into_iter().collect()
}

/// Host-root native build (BDEPEND front-matter): all deps target the host instance.
fn host_native_deps(
    provider: &PortageDependencyProvider,
    vd: &VersionData,
) -> DependencyConstraints<PortagePackage, PortageVersionSet> {
    let mut out: Vec<(PortagePackage, PortageVersionSet)> = vd
        .depend()
        .iter()
        .chain(vd.rdepend())
        .chain(vd.pdepend())
        .map(|(p, vs, _)| (stamp_root(p, MergeRoot::Host), vs.clone()))
        .collect();
    append_unsatisfied_broot(&mut out, vd.bdepend(), provider, vd, MergeRoot::Host);
    append_unsatisfied_broot(&mut out, vd.idepend(), provider, vd, MergeRoot::Host);
    out.into_iter().collect()
}

/// Native build: keep RDEPEND/PDEPEND; drop host-satisfied DEPEND, BDEPEND,
/// and IDEPEND.
///
/// DEPEND joined BDEPEND/IDEPEND's host-satisfied filtering 2026-07-11: for a
/// native (same-arch) build there's no separate build sysroot distinct from
/// the host when `CBUILD==CHOST` — DEPEND is satisfied by whatever machine
/// does the actual compiling, same as BDEPEND. Confirmed empirically against
/// real portage: `ROOT=X emerge sys-devel/gcc` against a genuinely empty `X`
/// does not need `os-headers`/`perl`/`sys-apps/portage`/etc. built fresh into
/// `X` — glibc's and gcc's own DEPEND is satisfied by the host. Before this
/// fix, DEPEND was included unconditionally with no host check at all, so a
/// self-contained `--root` build of a single package (e.g. `sys-devel/gcc`)
/// pulled in perl's own `!minimal?` PDEPEND tail (`perl-cleaner`,
/// `sys-apps/portage`, `app-crypt/gnupg`, `app-admin/eselect`,
/// `net-misc/rsync`) transitively via `virtual/os-headers` →
/// `sys-kernel/linux-headers` → `dev-lang/perl` — a 127-package plan for a
/// real `emerge`'s 16. See `todo/root-topology-refactor.md`.
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
/// every atom USE-dependency on that edge. A `[flag]` the host lacks is not
/// satisfied — portage rebuilds the package with the new USE, pulling its
/// re-evaluated USE-conditional closure (PMS §8.3 atom USE-deps). The parent
/// (`vd`) supplies the parent-flag state for `[flag?]`/`[flag=]` kinds.
///
/// `p` a `Choice`/`SlotChoice` virtual node (an `||`/`^^`/`??` OR-group or a
/// `:*` slot-star group) delegates to [`virtual_satisfied_on_broot`]: the
/// edge is satisfied when *any* branch is. Before this, a virtual target was
/// never a `host_installed` key, so this always returned `false` — every
/// DEPEND/BDEPEND/IDEPEND edge routed through an OR-group or a plain
/// unslotted dep on a multi-slot package (gcc, python, ...) became an
/// unconditional constraint, bypassing host-satisfaction entirely. Found
/// 2026-07-11 live-tracing why `em --root <dir> sys-devel/gcc` still pulled
/// ~123 packages after `broot_filtered` started filtering DEPEND: every
/// exploding package (`perl`, `os-headers`, `linux-headers`, `elt-patches`,
/// ...) checked `satisfied=true` on its *direct* edge, yet still ended up in
/// the plan — reached only through a Choice/SlotChoice node whose own edge
/// was never checked at all. `Root`/`UseDecision` are excluded (never
/// virtual-satisfiable): they aren't a real installable alternative, and
/// REQUIRED_USE/ceding machinery must keep deciding them, not have them
/// silently treated as "the host already has it". See
/// `todo/root-topology-refactor.md`.
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
/// classes collapse into a single list for a synthetic choice branch — see
/// `register_virtual_choices`) is itself `host_satisfied_on_broot_inner`,
/// recursing for a nested Choice/SlotChoice (e.g. an `||` group with a `:*`
/// member). `seen` guards against a pathological self-referential choice
/// graph; a revisited node is conservatively treated as unsatisfied rather
/// than looping.
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
/// `(+)`/`(-)` default for flags absent from IUSE — counts). Reuses the solver's
/// own violation predicate so the host check matches post-solve validation.
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
