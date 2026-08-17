//! Post-solve USE analysis: from a solved graph, compute the USE flag changes
//! the solution implies (the "needed" set) — autounmask suggestions for new
//! packages and rebuild requirements for installed ones.

use std::collections::{BTreeSet, HashMap};

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{UseDefault, UseDepKind, Version};
use pubgrub::SelectedDependencies;

use crate::convert;
use crate::package::PortagePackage;
use crate::use_config::UseFlagState;

use super::{PortageDependencyProvider, UseFlagRequirement};

/// Per-target accumulator in the post-solve fixpoint: the resolved version, the
/// flags that must be enabled / disabled, and the requiring packages' CPNs.
type TargetAcc = (
    Version,
    BTreeSet<Interned<DefaultInterner>>,
    BTreeSet<Interned<DefaultInterner>>,
    BTreeSet<Interned<DefaultInterner>>,
);

pub(crate) fn eval_violated_use_dep(
    kind: UseDepKind,
    dep_effective_enabled: bool,
    parent_flag_enabled: bool,
) -> Option<bool> {
    match kind {
        UseDepKind::Enabled => (!dep_effective_enabled).then_some(true),
        UseDepKind::Disabled => dep_effective_enabled.then_some(false),
        // [flag?]: if parent has flag → dep must have flag
        UseDepKind::Conditional => (parent_flag_enabled && !dep_effective_enabled).then_some(true),
        // [!flag?] = `!flag? ( dep[-flag] )` (PMS 8.2.6.4): if parent lacks the
        // flag, the dep must have it *disabled*; violated when it's enabled.
        UseDepKind::ConditionalInverse => {
            (!parent_flag_enabled && dep_effective_enabled).then_some(false)
        }
        // [flag=]: dep must match parent
        UseDepKind::Equal => {
            (dep_effective_enabled != parent_flag_enabled).then_some(parent_flag_enabled)
        }
        // [!flag=]: dep must be opposite of parent
        UseDepKind::EqualInverse => {
            let required = !parent_flag_enabled;
            (dep_effective_enabled == parent_flag_enabled).then_some(required)
        }
    }
}

impl PortageDependencyProvider {
    /// Walk the full PubGrub solution (including virtual choice packages) and
    /// collect USE flag requirements for every package that has at least one
    /// violated or unsatisfied USE dep constraint.
    ///
    /// **Installed packages** are compared against their VDB-recorded active USE
    /// flags; only violated constraints are collected (the flag needs to change).
    ///
    /// **Non-installed packages** (being freshly built) are compared against the
    /// global `use_config`; requirements where the flag might not be set by the
    /// current configuration are collected as informational annotations.
    ///
    /// The full solution (with virtual nodes) is required so that per-branch
    /// USE dep constraints from OR-group choices are also checked.
    /// Evaluate one parent's USE-dep constraints against the current solution and
    /// accumulate any violations into `by_target`. Shared by the two passes of the
    /// [`compute_use_flag_requirements`] fixpoint: the main-solution pass (parent
    /// = a solved package at its solution version) and the upgrade-expansion pass
    /// (parent = an installed package at its pending upgrade version). `parent_ver`
    /// is the version whose constraints are being expanded.
    fn accumulate_use_dep_violations(
        &self,
        parent: &PortagePackage,
        parent_ver: &Version,
        parent_is_installed: bool,
        udeps: &[convert::UseDepConstraint],
        solution: &SelectedDependencies<PortagePackage, Version>,
        by_target: &mut HashMap<PortagePackage, TargetAcc>,
    ) {
        for constraint in udeps {
            let (target_pkg, vs) = &constraint.target;
            if target_pkg.is_virtual() {
                continue;
            }

            // Resolve target version and whether it is installed.
            let (target_ver, is_installed) =
                if let Some((inst_ver, _)) = self.installed.get(target_pkg) {
                    if vs.contains(inst_ver) {
                        (inst_ver, true)
                    } else {
                        continue;
                    }
                } else if let Some(sol_ver) = solution.get(target_pkg) {
                    if vs.contains(sol_ver) {
                        (sol_ver, false)
                    } else {
                        continue;
                    }
                } else {
                    continue;
                };

            for ud in &constraint.use_deps {
                // Parent's flag state: currently active OR will be enabled after
                // this build run (already recorded in by_target.required_enabled).
                let parent_flag_enabled = if parent_is_installed {
                    self.installed_use
                        .get(parent)
                        .is_some_and(|u| u.contains(&ud.flag))
                        || by_target
                            .get(parent)
                            .is_some_and(|(_, e, _, _)| e.contains(&ud.flag))
                } else {
                    self.effective_flag_new(parent, parent_ver, ud.flag, None)
                };

                let dep_effective_enabled = if is_installed {
                    let active = self
                        .installed_use
                        .get(target_pkg)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);
                    let iuse = self
                        .packages
                        .get(target_pkg)
                        .and_then(|d| d.versions.get(target_ver))
                        .map(|vd| vd.iuse.as_slice())
                        // Empty == absent: a synthetic installed entry (or a repo
                        // version with no IUSE) must fall back to the VDB-recorded
                        // IUSE, matching pre-refactor behaviour.
                        .filter(|s| !s.is_empty())
                        .or_else(|| self.installed_iuse.get(target_pkg).map(Vec::as_slice))
                        .unwrap_or(&[]);
                    let in_iuse = iuse.contains(&ud.flag);
                    if in_iuse {
                        active.contains(&ud.flag)
                    } else {
                        matches!(ud.default, Some(UseDefault::Enabled))
                    }
                } else {
                    self.effective_flag_new(target_pkg, target_ver, ud.flag, ud.default)
                };

                // Account for a decision already made for this target in this pass:
                // a pending enable/disable overrides the static state so a second
                // requirer demanding the opposite value sees the conflict and
                // records its (opposite) requirement too, instead of being silently
                // satisfied. This surfaces `dev/foo[bar]` vs `dev/foo[-bar]` as both
                // sides recorded rather than one being lost.
                let dep_effective_enabled = match by_target.get(target_pkg) {
                    Some((_, en, _, _)) if en.contains(&ud.flag) => true,
                    Some((_, _, di, _)) if di.contains(&ud.flag) => false,
                    _ => dep_effective_enabled,
                };

                if let Some(requires_enabled) =
                    eval_violated_use_dep(ud.kind, dep_effective_enabled, parent_flag_enabled)
                {
                    let entry = by_target.entry(target_pkg.clone()).or_insert_with(|| {
                        (
                            target_ver.clone(),
                            BTreeSet::new(),
                            BTreeSet::new(),
                            BTreeSet::new(),
                        )
                    });
                    if requires_enabled {
                        entry.1.insert(ud.flag);
                    } else {
                        entry.2.insert(ud.flag);
                    }
                    if !parent.is_virtual() {
                        entry.3.insert(constraint.parent_cpn_str);
                    }
                }
            }
        }
    }

    pub(crate) fn compute_use_flag_requirements(
        &self,
        solution: &SelectedDependencies<PortagePackage, Version>,
    ) -> Vec<UseFlagRequirement> {
        // Accumulate per target: (version, enable_set, disable_set, requirers).
        let mut by_target: HashMap<PortagePackage, TargetAcc> = HashMap::new();
        // Installed packages that should be upgraded to a newer repo version
        // rather than rebuilt at the installed version.  Keyed by the installed
        // package; value is the newer version to build instead.
        let mut upgrade_to: HashMap<PortagePackage, Version> = HashMap::new();

        // Iterate to fixpoint:
        // 1. Conditional deps cascade — when package A needs flag X, A's own
        //    `B[X(-)?]` deps fire, requiring B to have X as well.
        // 2. When an installed package gains a violation, check if a newer repo
        //    version exists whose constraints should also be expanded (upgrade
        //    the package rather than rebuilding the old version).
        loop {
            let prev_total: usize = by_target
                .values()
                .map(|(_, e, d, _)| e.len() + d.len())
                .sum();
            let prev_upgrades = upgrade_to.len();

            // -- main solution packages --
            for (pkg, ver) in solution.iter() {
                let Some(vd) = self.package_data(pkg).and_then(|d| d.versions.get(ver)) else {
                    continue;
                };
                // "Installed" for parent-flag evaluation means *selected at its
                // installed version*: a package being upgraded gets its NEW
                // version's flags from the desired config, not the old build's
                // active USE (which would silently drop `[flag?]` conditionals
                // the upgrade newly enables).
                let at_installed_ver = self.installed.get(pkg).is_some_and(|(iv, _)| iv == ver);
                self.accumulate_use_dep_violations(
                    pkg,
                    ver,
                    at_installed_ver,
                    &vd.use_deps,
                    solution,
                    &mut by_target,
                );
            }

            // -- upgrade expansion --
            // For each installed package with violations, check whether a newer
            // repo version exists.  If so, record the upgrade and process the
            // newer version's USE dep constraints in the next fixpoint iteration.
            let installed_with_violations: Vec<(PortagePackage, Version)> = by_target
                .iter()
                .filter(|(pkg, _)| self.installed.contains_key(pkg))
                .filter(|(pkg, _)| !upgrade_to.contains_key(*pkg))
                .filter_map(|(pkg, (inst_ver, _, _, _))| {
                    self.package_data(pkg)
                        .and_then(|d| d.versions.keys().filter(|v| v > &inst_ver).max())
                        .map(|new_ver| (pkg.clone(), new_ver.clone()))
                })
                .collect();

            for (pkg, new_ver) in installed_with_violations {
                upgrade_to.insert(pkg.clone(), new_ver.clone());

                // Expand the newer version's USE dep constraints. The "parent" is
                // the upgraded package itself at its new version.
                let Some(vd) = self
                    .package_data(&pkg)
                    .and_then(|d| d.versions.get(&new_ver))
                else {
                    continue;
                };
                // The upgraded version is by definition not the installed build.
                self.accumulate_use_dep_violations(
                    &pkg,
                    &new_ver,
                    false,
                    &vd.use_deps,
                    solution,
                    &mut by_target,
                );
            }

            let new_total: usize = by_target
                .values()
                .map(|(_, e, d, _)| e.len() + d.len())
                .sum();
            if new_total == prev_total && upgrade_to.len() == prev_upgrades {
                break;
            }
        }

        let mut reqs: Vec<UseFlagRequirement> = by_target
            .into_iter()
            .map(
                |(pkg, (ver, enable, disable, requirers))| UseFlagRequirement {
                    package: pkg.clone(),
                    version: ver,
                    upgrade_to: upgrade_to.remove(&pkg),
                    required_enabled: enable.into_iter().collect(),
                    required_disabled: disable.into_iter().collect(),
                    required_by: requirers.into_iter().map(|s| s.to_string()).collect(),
                },
            )
            .collect();
        // `by_target` is a HashMap, so collect order is nondeterministic; sort by
        // (package, version) so use_flag_requirements — and everything derived
        // from it (reinstall_deps → the appended merge-order tail, and the
        // autounmask report order) — is reproducible across runs.
        reqs.sort_by(|a, b| {
            a.package
                .cmp(&b.package)
                .then_with(|| a.version.cmp(&b.version))
        });
        reqs
    }

    /// Return all USE flag requirements collected by the post-solve validation pass.
    ///
    /// Includes both reinstall candidates (`R`) and informational annotations
    /// for newly-installed packages.  Populated by `resolve_targets`.
    pub fn use_flag_requirements(&self) -> &[UseFlagRequirement] {
        &self.use_flag_requirements
    }

    /// Return only the requirements that correspond to reinstalls: installed
    /// packages whose current USE flags violate at least one constraint from the
    /// resolved set.
    pub fn reinstall_deps(&self) -> Vec<&UseFlagRequirement> {
        self.use_flag_requirements
            .iter()
            .filter(|r| self.installed.contains_key(&r.package))
            .collect()
    }

    /// Check whether all USE dep constraints for an OR-group branch are
    /// consistent with the desired final state of the installed packages.
    ///
    /// A flag is treated as "effectively enabled" when it is either:
    /// - currently active in the installed VDB, OR
    /// - in the package's IUSE and enabled in the global `use_config`
    ///   (i.e. the profile / make.conf wants it enabled after the next build).
    ///
    /// This means branch selection picks branches that are consistent with the
    /// *desired* state, not just the *current* installed state.  Branches that
    /// conflict with the global config are de-prioritised, allowing the
    /// post-solve violation pass to then flag the specific flags that need to
    /// change.
    ///
    /// Returns `true` when every constraint is either satisfied (under the
    /// above definition) or refers to a package not yet installed.
    /// Effective state of `flag` on a non-installed package version that will be
    /// freshly built.  Mirrors what the build will actually see: `package.use`
    /// and global USE applied on top of the ebuild's IUSE defaults.  For a flag
    /// outside the package's IUSE, only the dep's own `(+)`/`(-)` default applies
    /// — except at the package's already-installed version, where the repo may
    /// carry no metadata at all for that exact cpv (revbumped or pruned from the
    /// tree while still installed): there `installed_iuse`/`installed_use` (the
    /// VDB's own record, kept for exactly this gap) are the ground truth instead
    /// of the dep's default.
    pub(crate) fn effective_flag_new(
        &self,
        pkg: &PortagePackage,
        ver: &Version,
        flag: Interned<DefaultInterner>,
        dep_default: Option<UseDefault>,
    ) -> bool {
        let vd = self.package_data(pkg).and_then(|d| d.versions.get(ver));
        let in_iuse = vd.is_some_and(|v| v.iuse.contains(&flag));
        if in_iuse {
            // `desired` already folds package.use, global USE, and IUSE
            // defaults, so a single lookup gives the flag's effective state.
            return vd.is_some_and(|v| v.desired.get(flag) == UseFlagState::Enabled);
        }
        let at_installed_ver = self.installed.get(pkg).is_some_and(|(iv, _)| iv == ver);
        if at_installed_ver
            && let Some(iuse) = self.installed_iuse.get(pkg)
            && iuse.contains(&flag)
        {
            return self
                .installed_use
                .get(pkg)
                .is_some_and(|u| u.contains(&flag));
        }
        matches!(dep_default, Some(UseDefault::Enabled))
    }

    pub(crate) fn use_dep_branch_satisfied(&self, udeps: &[convert::UseDepConstraint]) -> bool {
        for constraint in udeps {
            let (target_pkg, vs) = &constraint.target;
            let Some((inst_ver, _)) = self.installed.get(target_pkg) else {
                continue; // not installed → can't verify, don't veto
            };
            if !vs.contains(inst_ver) {
                continue;
            }
            let active = self
                .installed_use
                .get(target_pkg)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            for ud in &constraint.use_deps {
                // Desired final state of the target's flag: active now, or the
                // version's desired set will enable it on rebuild.
                let dep_effective_enabled = active.contains(&ud.flag)
                    || self.effective_flag_new(target_pkg, inst_ver, ud.flag, ud.default);
                // Parent flag (only used by Conditional/Equal kinds, rare in OR
                // groups): approximate with the target's desired state.
                let parent_flag_enabled =
                    self.effective_flag_new(target_pkg, inst_ver, ud.flag, ud.default);
                // A violated constraint means this branch is not satisfiable.
                if eval_violated_use_dep(ud.kind, dep_effective_enabled, parent_flag_enabled)
                    .is_some()
                {
                    return false;
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::eval_violated_use_dep;
    use portage_atom::UseDepKind;

    /// PMS 8.2.6.4 six-form table, evaluated directly. `Some(true)` = violated,
    /// flag must be enabled; `Some(false)` = violated, flag must be disabled;
    /// `None` = satisfied. Both conditional-inverse rows are the pre-fix trap:
    /// `!flag?` with parent OFF requires the dep flag *disabled*
    /// (`!flag? ( dep[-flag] )`), so parent-off/dep-off is satisfied and
    /// parent-off/dep-on is the violation (must disable), not the reverse.
    #[test]
    fn eval_violated_use_dep_matches_pms_table() {
        use UseDepKind::*;
        // (kind, dep_enabled, parent_enabled) -> expected
        let cases = [
            (Enabled, true, false, None),
            (Enabled, false, false, Some(true)),
            (Disabled, false, false, None),
            (Disabled, true, false, Some(false)),
            // [flag?]: parent on -> dep must be on
            (Conditional, true, true, None),
            (Conditional, false, true, Some(true)),
            (Conditional, false, false, None),
            // [!flag?]: parent off -> dep must be OFF
            (ConditionalInverse, false, false, None),
            (ConditionalInverse, true, false, Some(false)),
            (ConditionalInverse, true, true, None),
            // [flag=]: dep must match parent
            (Equal, true, true, None),
            (Equal, false, true, Some(true)),
            (Equal, true, false, Some(false)),
            // [!flag=]: dep must be opposite of parent
            (EqualInverse, false, true, None),
            (EqualInverse, true, true, Some(false)),
            (EqualInverse, false, false, Some(true)),
        ];
        for (kind, dep, parent, expected) in cases {
            assert_eq!(
                eval_violated_use_dep(kind, dep, parent),
                expected,
                "kind={kind:?} dep_enabled={dep} parent_enabled={parent}"
            );
        }
    }
}
