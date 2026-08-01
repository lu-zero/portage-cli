use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Dep, SlotOperator, UseDepKind, Version};

use crate::error::Error;
use crate::package::PortagePackage;
use crate::provider::PortageDependencyProvider;
use crate::use_config::{UseConfig, UseFlagState};

/// A resolved slot-operator binding.
///
/// After resolution, maps each `:=` / `:0=` dependency to the actual slot
/// chosen by the solver, for rebuild tracking.
///
/// See [PMS 8.3.3](https://projects.gentoo.org/pms/9/pms.html#slot_deps).
#[derive(Debug, Clone)]
pub struct SlotOperatorBinding {
    /// The package that declared the slot-operator dependency
    /// (e.g. `"app-misc/app-1.0"`).
    pub parent: String,
    /// The CPN of the target dependency (e.g. `"dev-libs/lib"`).
    pub target_cpn: String,
    /// The slot the solver assigned to the target (e.g. `Some("0")`).
    pub slot: Option<Interned<DefaultInterner>>,
    /// The slot name declared in the atom, if any (`:0=` → `Some("0")`,
    /// `:=` → `None`).
    pub declared_slot: Option<Interned<DefaultInterner>>,
    /// The operator string (`"="` for `:=` / `:0=`).
    pub operator: &'static str,
}

/// One package satisfied by a blocker's atom after the plan.
#[derive(Debug, Clone)]
pub struct BlockerVictim {
    /// The package satisfying the blocker atom.
    pub package: PortagePackage,
    /// Its version.
    pub version: Version,
    /// `true`: an installed package the plan retains (a removal candidate).
    /// `false`: a solution member (the plan installs/keeps it — this
    /// package's own end-state coexists with the blocker, a hard conflict
    /// regardless of blocker strength).
    pub retained_installed: bool,
}

/// One blocker atom satisfied by at least one package present after the
/// plan, with full victim identity — the input to blocker classification
/// (auto-removable vs. genuine conflict), unlike [`Error::BlockerConflict`]'s
/// flattened advisory strings.
#[derive(Debug, Clone)]
pub struct BlockerHit {
    /// The package declaring the blocker.
    pub owner: PortagePackage,
    /// The owner's version.
    pub owner_version: Version,
    /// `true` when `owner` is a retained installed package (the reciprocal
    /// loop, e.g. an installed package blocking something in the plan)
    /// rather than a solution member.
    pub owner_installed: bool,
    /// The blocker atom; `atom.blocker` carries the weak/strong strength.
    pub atom: Dep,
    /// Every package the atom matched, post-plan.
    pub victims: Vec<BlockerVictim>,
}

impl PortageDependencyProvider {
    /// Validate USE-dep constraints against a solution (post-solve check).
    ///
    /// For each package in the solution that declares USE-dep constraints,
    /// verify that the target package satisfies them according to
    /// [PMS 8.3.4](https://projects.gentoo.org/pms/9/pms.html#style-and-style-use-dependencies):
    ///
    /// - `[flag]` — target's flag must be enabled
    /// - `[-flag]` — target's flag must be disabled
    /// - `[flag?]` — if parent's flag is ON, target's flag must be ON
    /// - `[!flag?]` — if parent's flag is OFF, target's flag must be OFF
    /// - `[flag=]` — target's flag must match parent's flag state
    /// - `[!flag=]` — target's flag must be opposite of parent's flag state
    ///
    /// Parent flag state is resolved from `use_config` (user-decided) or from
    /// the solution's virtual USE packages (solver-decided).
    pub fn check_use_deps(
        &self,
        solution: &pubgrub::SelectedDependencies<PortagePackage, Version>,
        use_config: &UseConfig,
    ) -> Vec<Error> {
        let mut errors = Vec::new();
        for (pkg, version) in solution.iter() {
            let Some(vd) = self.package_data(pkg).and_then(|d| d.versions.get(version)) else {
                continue;
            };
            for constraint in &vd.use_deps {
                let (target_pkg, target_vs) = &constraint.target;
                let target_entry = solution
                    .iter()
                    .find(|(p, v)| p.cpn() == target_pkg.cpn() && target_vs.contains(v));
                let Some((_, target_ver)) = target_entry else {
                    continue;
                };

                let target_iuse = self
                    .package_data(target_pkg)
                    .and_then(|d| d.versions.get(target_ver))
                    .map(|v| v.iuse.as_slice())
                    .unwrap_or(&[]);

                for ud in &constraint.use_deps {
                    let target_flag_state = resolve_flag_state(target_iuse, ud, use_config);

                    let parent_flag_state = match ud.kind {
                        UseDepKind::Conditional
                        | UseDepKind::ConditionalInverse
                        | UseDepKind::Equal
                        | UseDepKind::EqualInverse => {
                            let parent_virtual_name = format!(
                                "__internal__/USE_{}_{}",
                                constraint.parent_cpn_str.replace('/', "_"),
                                ud.flag.as_str()
                            );
                            resolve_parent_flag(ud.flag, &parent_virtual_name, use_config, solution)
                        }
                        _ => UseFlagState::Disabled,
                    };

                    let satisfied = match ud.kind {
                        UseDepKind::Enabled => target_flag_state == UseFlagState::Enabled,
                        UseDepKind::Disabled => target_flag_state == UseFlagState::Disabled,
                        UseDepKind::Conditional => {
                            if parent_flag_state == UseFlagState::Enabled {
                                target_flag_state == UseFlagState::Enabled
                            } else {
                                true
                            }
                        }
                        UseDepKind::ConditionalInverse => {
                            // PMS 8.2.6.4: `foo[!bar?]` = `bar? ( foo ) !bar? ( foo[-bar] )`.
                            // Parent flag OFF ⇒ the `!bar?` branch requires the target's
                            // flag be *disabled* (`foo[-bar]`), not enabled.
                            if parent_flag_state == UseFlagState::Disabled {
                                target_flag_state == UseFlagState::Disabled
                            } else {
                                true
                            }
                        }
                        UseDepKind::Equal => target_flag_state == parent_flag_state,
                        UseDepKind::EqualInverse => target_flag_state != parent_flag_state,
                    };

                    if !satisfied {
                        errors.push(Error::UseDepConflict(
                            format!("{}-{}", pkg, version),
                            format!(
                                "{}: {}[{}] not satisfied (target={:?}, parent={:?})",
                                target_pkg,
                                match ud.kind {
                                    UseDepKind::Enabled => format!("+{}", ud.flag.as_str()),
                                    UseDepKind::Disabled => format!("-{}", ud.flag.as_str()),
                                    UseDepKind::Conditional => format!("{}?", ud.flag.as_str()),
                                    UseDepKind::ConditionalInverse =>
                                        format!("!{}?", ud.flag.as_str()),
                                    UseDepKind::Equal => format!("{}=", ud.flag.as_str()),
                                    UseDepKind::EqualInverse => format!("!{}=", ud.flag.as_str()),
                                },
                                ud.flag.as_str(),
                                target_flag_state,
                                parent_flag_state,
                            ),
                        ));
                    }
                }
            }
        }
        errors
    }

    /// Validate repository constraints against a solution (post-solve check).
    ///
    /// For each package in the solution that declares `::repo` constraints,
    /// verify the target package comes from the required repository.
    ///
    /// See [PMS 8.3.5](https://projects.gentoo.org/pms/9/pms.html#repository).
    pub fn check_repo_constraints(
        &self,
        solution: &pubgrub::SelectedDependencies<PortagePackage, Version>,
    ) -> Vec<Error> {
        let mut errors = Vec::new();
        for (pkg, version) in solution.iter() {
            let Some(vd) = self.package_data(pkg).and_then(|d| d.versions.get(version)) else {
                continue;
            };
            for constraint in &vd.repo_constraints {
                let (target_pkg, target_vs) = &constraint.target;
                let target_entry = solution
                    .iter()
                    .find(|(p, v)| p.cpn() == target_pkg.cpn() && target_vs.contains(v));
                let Some((target_pkg_key, target_ver)) = target_entry else {
                    continue;
                };
                let target_repo = self
                    .package_data(target_pkg_key)
                    .and_then(|d| d.versions.get(target_ver))
                    .and_then(|v| v.repo.as_ref());
                match target_repo {
                    Some(r) if *r == constraint.repo => {}
                    _ => {
                        errors.push(Error::RepoConstraintConflict(
                            format!("{}-{}", pkg, version),
                            format!(
                                "{}::{} required but target comes from {:?}",
                                target_pkg,
                                constraint.repo.as_str(),
                                target_repo.map(|r| r.as_str()),
                            ),
                        ));
                    }
                }
            }
        }
        errors
    }

    /// Validate blockers against a solution (post-solve check).
    ///
    /// A blocker `!dev-libs/foo` means that if this package is installed,
    /// `dev-libs/foo` (with matching version/slot constraints if any) must
    /// NOT be in the solution.
    ///
    /// Thin wrapper over [`Self::check_blockers_detailed`] for existing
    /// callers that only want the advisory strings — new code (blocker
    /// classification) should call the detailed form, which keeps the
    /// victim's identity instead of throwing it away.
    pub fn check_blockers(
        &self,
        solution: &pubgrub::SelectedDependencies<PortagePackage, Version>,
    ) -> Vec<Error> {
        let mut conflicts = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for hit in self.check_blockers_detailed(solution) {
            record_blocker(
                &mut conflicts,
                &mut seen,
                format!("{}-{}", hit.owner, hit.owner_version),
                &hit.atom,
            );
        }
        conflicts
    }

    /// Validate blockers against a solution (post-solve check), keeping full
    /// victim identity for classification (auto-removable vs. genuine
    /// conflict) rather than collapsing to an advisory string.
    ///
    /// A blocker `!dev-libs/foo` means that if this package is installed,
    /// `dev-libs/foo` (with matching version/slot constraints if any) must
    /// NOT be in the solution.
    pub fn check_blockers_detailed(
        &self,
        solution: &pubgrub::SelectedDependencies<PortagePackage, Version>,
    ) -> Vec<BlockerHit> {
        let mut hits = Vec::new();
        // `(cpn, slot)` the plan installs into; an installed package absent here
        // is retained after the merge. O(1) vs scanning the whole solution.
        let solution_keys: std::collections::HashSet<_> =
            solution.iter().map(|(p, _)| (p.cpn(), p.slot())).collect();
        let retained = |p: &PortagePackage| !solution_keys.contains(&(p.cpn(), p.slot()));

        // A blocker fires when its atom is satisfied by a package present after
        // the plan: a solution member or a retained installed one. The installed
        // side matters for e.g. `systemd[resolvconf]` blocking an installed
        // net-dns/openresolv that nothing pulls into the solve.
        for (pkg, version) in solution.iter() {
            let Some(vd) = self.package_data(pkg).and_then(|d| d.versions.get(version)) else {
                continue;
            };
            for blocker in &vd.blockers {
                let victims = self.blocker_victims(blocker, solution, &retained);
                if !victims.is_empty() {
                    hits.push(BlockerHit {
                        owner: pkg.clone(),
                        owner_version: version.clone(),
                        owner_installed: false,
                        atom: blocker.clone(),
                        victims,
                    });
                }
            }
        }

        // Reciprocal: a blocker declared by a retained installed package (fed in
        // pre-evaluated, since its owner is never in the solve) against the plan.
        for (owner, blockers) in &self.installed_blockers {
            if !retained(owner) {
                continue;
            }
            let Some((owner_ver, _)) = self.installed.get(owner) else {
                continue;
            };
            for blocker in blockers {
                let victims = self.blocker_victims(blocker, solution, &retained);
                if !victims.is_empty() {
                    hits.push(BlockerHit {
                        owner: owner.clone(),
                        owner_version: owner_ver.clone(),
                        owner_installed: true,
                        atom: blocker.clone(),
                        victims,
                    });
                }
            }
        }

        hits
    }

    /// Every package present after the plan (a solution member, or an
    /// installed one `retained` keeps in place) that satisfies `blocker`'s
    /// atom.
    fn blocker_victims(
        &self,
        blocker: &Dep,
        solution: &pubgrub::SelectedDependencies<PortagePackage, Version>,
        retained: &impl Fn(&PortagePackage) -> bool,
    ) -> Vec<BlockerVictim> {
        let mut victims: Vec<BlockerVictim> = solution
            .iter()
            .filter(|(p, v)| self.blocker_satisfied_by(blocker, p, v, false))
            .map(|(p, v)| BlockerVictim {
                package: p.clone(),
                version: v.clone(),
                retained_installed: false,
            })
            .collect();
        victims.extend(
            self.installed
                .iter()
                .filter(|(p, (v, _))| retained(p) && self.blocker_satisfied_by(blocker, p, v, true))
                .map(|(p, (v, _))| BlockerVictim {
                    package: p.clone(),
                    version: v.clone(),
                    retained_installed: true,
                }),
        );
        victims
    }

    /// Whether `blocker`'s atom (cpn/slot, version, and any USE-dep) is satisfied
    /// by `(cand_pkg, cand_ver)`. `from_installed` picks the USE source: VDB
    /// flags for an installed candidate, freshly-resolved USE otherwise. A
    /// `!foo[bar]` blocker fires only when the candidate has the flag (so
    /// `!glibc[crypt(-)]` ignores a glibc built without `crypt`).
    fn blocker_satisfied_by(
        &self,
        blocker: &Dep,
        cand_pkg: &PortagePackage,
        cand_ver: &Version,
        from_installed: bool,
    ) -> bool {
        if !blocker_cpn_slot_matches(cand_pkg, blocker) {
            return false;
        }
        if let Some(v) = &blocker.version {
            let op = blocker.op.unwrap_or(portage_atom::Operator::Equal);
            if !version_matches_operator(cand_ver, op, blocker.glob, v) {
                return false;
            }
        }
        match &blocker.use_deps {
            None => true,
            Some(use_deps) => use_deps.iter().all(|ud| {
                let eff = if from_installed {
                    self.installed_use
                        .get(cand_pkg)
                        .is_some_and(|u| u.contains(&ud.flag))
                } else {
                    self.effective_flag_new(cand_pkg, cand_ver, ud.flag, ud.default)
                };
                match ud.kind {
                    UseDepKind::Enabled => eff,
                    UseDepKind::Disabled => !eff,
                    // Conditional/Equal forms don't occur on blockers in practice;
                    // treat as applying so a real one isn't missed.
                    _ => true,
                }
            }),
        }
    }

    /// Resolve slot-operator bindings from a solution.
    ///
    /// For each package in the solution that declared `:=` / `:0=` dependencies,
    /// look up the actual slot the solver assigned to each target and return
    /// the bindings. This information is used for rebuild tracking: when a
    /// dependency's slot changes, the parent package must be rebuilt.
    ///
    /// `:*` dependencies are excluded — they express "any slot" with no
    /// rebuild implications.
    ///
    /// See [PMS 8.3.3](https://projects.gentoo.org/pms/9/pms.html#slot_deps).
    pub fn slot_operator_bindings(
        &self,
        solution: &pubgrub::SelectedDependencies<PortagePackage, Version>,
    ) -> Vec<SlotOperatorBinding> {
        let mut bindings = Vec::new();
        for (pkg, version) in solution.iter() {
            let Some(vd) = self.package_data(pkg).and_then(|d| d.versions.get(version)) else {
                continue;
            };
            for op_dep in &vd.slot_operator_deps {
                let bound_slot = solution.iter().find_map(|(sol_pkg, sol_ver)| {
                    if sol_pkg.cpn() == op_dep.target.0.cpn() && op_dep.target.1.contains(sol_ver) {
                        Some((sol_pkg.slot(), sol_ver.clone()))
                    } else {
                        None
                    }
                });
                if let Some((slot, _ver)) = bound_slot {
                    bindings.push(SlotOperatorBinding {
                        parent: format!("{}-{}", pkg, version),
                        target_cpn: format!("{}", op_dep.target.0.cpn()),
                        slot,
                        declared_slot: op_dep.slot,
                        operator: match op_dep.operator {
                            SlotOperator::Equal => "=",
                            SlotOperator::Star => "*",
                        },
                    });
                }
            }
        }
        bindings
    }
}

/// Push a deduplicated blocker conflict; `owner` is the cpv string declaring it.
fn record_blocker(
    conflicts: &mut Vec<Error>,
    seen: &mut std::collections::HashSet<(String, String)>,
    owner: String,
    blocker: &Dep,
) {
    let blocker_str = blocker.to_string();
    if seen.insert((owner.clone(), blocker_str.clone())) {
        let strength = match blocker.blocker {
            Some(portage_atom::Blocker::Strong) => "strong(!!)",
            _ => "weak(!)",
        };
        conflicts.push(Error::BlockerConflict {
            pkg: owner,
            blocker: blocker_str,
            strength,
        });
    }
}

pub(crate) fn blocker_cpn_slot_matches(sol_pkg: &PortagePackage, blocker: &Dep) -> bool {
    if sol_pkg.cpn() != &blocker.cpn {
        return false;
    }
    match &blocker.slot_dep {
        None => true,
        Some(portage_atom::SlotDep::Slot { slot: Some(sd), .. }) => sol_pkg.slot() == Some(sd.slot),
        Some(portage_atom::SlotDep::Slot { slot: None, .. }) => true,
        Some(portage_atom::SlotDep::Operator(_)) => true,
    }
}

pub(crate) fn version_matches_operator(
    candidate: &Version,
    op: portage_atom::Operator,
    glob: bool,
    target: &Version,
) -> bool {
    use std::cmp::Ordering;
    let cmp = candidate.cmp(target);
    match op {
        portage_atom::Operator::Equal => {
            if glob {
                candidate.glob_matches(target)
            } else {
                cmp == Ordering::Equal
            }
        }
        portage_atom::Operator::GreaterOrEqual => cmp != Ordering::Less,
        portage_atom::Operator::Greater => cmp == Ordering::Greater,
        portage_atom::Operator::LessOrEqual => cmp != Ordering::Greater,
        portage_atom::Operator::Less => cmp == Ordering::Less,
        portage_atom::Operator::Approximate => {
            let mut base_target = target.clone();
            base_target.revision = portage_atom::Revision::default();
            let mut base_candidate = candidate.clone();
            base_candidate.revision = portage_atom::Revision::default();
            base_candidate == base_target
        }
    }
}

pub(crate) fn resolve_flag_state(
    target_iuse: &[Interned<DefaultInterner>],
    ud: &portage_atom::UseDep,
    use_config: &UseConfig,
) -> UseFlagState {
    let flag_defined = target_iuse.iter().any(|f| f.as_str() == ud.flag.as_str());
    if flag_defined {
        use_config.get(ud.flag)
    } else {
        match ud.default {
            Some(portage_atom::UseDefault::Enabled) => UseFlagState::Enabled,
            Some(portage_atom::UseDefault::Disabled) => UseFlagState::Disabled,
            None => UseFlagState::Disabled,
        }
    }
}

/// Read a USE-flag decision from the solved solution.
///
/// `virtual_name` is the interned name of a `UseDecision` node
/// (e.g. `"USE_test_pkg_ssl"`). Returns the flag state from the USE config
/// if already known; otherwise reads version 1 (enabled) or 0 (disabled)
/// from the solution.
pub(crate) fn resolve_parent_flag(
    flag: Interned<DefaultInterner>,
    virtual_name: &str,
    use_config: &UseConfig,
    solution: &pubgrub::SelectedDependencies<PortagePackage, Version>,
) -> UseFlagState {
    let config_state = use_config.get(flag);
    if !matches!(config_state, UseFlagState::SolverDecided { .. }) {
        return config_state;
    }

    let virtual_name_interned = Interned::<DefaultInterner>::intern(virtual_name);
    for (pkg, ver) in solution.iter() {
        if let PortagePackage::UseDecision { name } = pkg
            && name == &virtual_name_interned
        {
            let ver_str = ver.to_string();
            if ver_str == "1" {
                return UseFlagState::Enabled;
            } else {
                return UseFlagState::Disabled;
            }
        }
    }

    UseFlagState::Disabled
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::{InMemoryRepository, PackageDeps};
    use crate::version_set::PortageVersionSet;
    use portage_atom::interner::Interned;
    use portage_atom::{Cpn, Dep, DepEntry};

    fn empty_deps() -> PackageDeps {
        PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        }
    }

    #[test]
    fn slot_operator_binding_bare_equals() {
        let mut repo = InMemoryRepository::new();
        let slot_0 = Interned::<DefaultInterner>::intern("0");

        repo.add_version(
            portage_atom::Cpv::parse("app-misc/app-1.0").unwrap(),
            Some(slot_0),
            None,
            PackageDeps {
                depend: (vec![DepEntry::Atom(Dep::parse("dev-libs/lib:=").unwrap())]).into(),
                rdepend: (vec![]).into(),
                bdepend: (vec![]).into(),
                pdepend: (vec![]).into(),
                idepend: (vec![]).into(),
            },
        );
        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/lib-1.0").unwrap(),
            Some(slot_0),
            None,
            empty_deps(),
        );

        let mut provider = PortageDependencyProvider::new(repo);
        let app = PortagePackage::slotted(Cpn::parse("app-misc/app").unwrap(), slot_0);
        let solution = provider
            .resolve_targets(vec![(app, PortageVersionSet::any())])
            .unwrap();
        let bindings = provider.slot_operator_bindings(&solution);
        assert_eq!(bindings.len(), 1, "should have one slot-operator binding");
        assert_eq!(bindings[0].operator, "=");
        assert_eq!(bindings[0].declared_slot, None);
        assert_eq!(bindings[0].slot, Some(slot_0));
    }

    #[test]
    fn slot_operator_binding_explicit_slot() {
        let mut repo = InMemoryRepository::new();
        let slot_0 = Interned::<DefaultInterner>::intern("0");

        repo.add_version(
            portage_atom::Cpv::parse("app-misc/app-1.0").unwrap(),
            Some(slot_0),
            None,
            PackageDeps {
                depend: (vec![DepEntry::Atom(Dep::parse("dev-libs/lib:0=").unwrap())]).into(),
                rdepend: (vec![]).into(),
                bdepend: (vec![]).into(),
                pdepend: (vec![]).into(),
                idepend: (vec![]).into(),
            },
        );
        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/lib-1.0").unwrap(),
            Some(slot_0),
            None,
            empty_deps(),
        );

        let mut provider = PortageDependencyProvider::new(repo);
        let app = PortagePackage::slotted(Cpn::parse("app-misc/app").unwrap(), slot_0);
        let solution = provider
            .resolve_targets(vec![(app, PortageVersionSet::any())])
            .unwrap();
        let bindings = provider.slot_operator_bindings(&solution);
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].operator, "=");
        assert_eq!(bindings[0].declared_slot, Some(slot_0));
    }
    #[test]
    fn check_blockers_detects_conflict() {
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        };

        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/openssl-3.0.0").unwrap(),
            None,
            None,
            PackageDeps {
                depend: (vec![DepEntry::Atom(Dep::parse("!dev-libs/libressl").unwrap())]).into(),
                rdepend: (vec![]).into(),
                bdepend: (vec![]).into(),
                pdepend: (vec![]).into(),
                idepend: (vec![]).into(),
            },
        );
        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/libressl-3.9.0").unwrap(),
            None,
            None,
            empty(),
        );

        let mut provider = PortageDependencyProvider::new(repo);
        let openssl = PortagePackage::unslotted(Cpn::parse("dev-libs/openssl").unwrap());
        let libressl = PortagePackage::unslotted(Cpn::parse("dev-libs/libressl").unwrap());
        let solution = provider
            .resolve_targets(vec![
                (openssl, PortageVersionSet::any()),
                (libressl, PortageVersionSet::any()),
            ])
            .unwrap();
        let conflicts = provider.check_blockers(&solution);
        assert!(
            !conflicts.is_empty(),
            "should detect blocker conflict between openssl and libressl"
        );

        // check_blockers_detailed twin: keeps the victim's identity instead
        // of collapsing to an advisory string.
        let hits = provider.check_blockers_detailed(&solution);
        assert_eq!(hits.len(), 1);
        assert!(!hits[0].owner_installed);
        assert_eq!(hits[0].victims.len(), 1);
        assert_eq!(
            hits[0].victims[0].package,
            PortagePackage::unslotted(Cpn::parse("dev-libs/libressl").unwrap())
        );
        assert!(!hits[0].victims[0].retained_installed);
    }

    /// Regression test for the same bug class as `graph.rs`'s
    /// `host_package_bdepend_on_another_host_package_orders_correctly`
    /// (`208c818`'s alias-miss bug, found again in `validate.rs`): a
    /// `Host`-flavored solved package's own data lives under its
    /// `Target`-flavored alias, so a raw `self.packages.get(pkg)` in
    /// `check_blockers`'s main loop silently missed it — dropping every
    /// blocker a Host-routed package declares, regardless of what it
    /// targets. `dev-build/user` here is scheduled as an unsatisfied Host
    /// BDEPEND (mirroring the `graph.rs` test's setup) and blocks
    /// `dev-build/blocked`, which is separately pulled in as a normal
    /// Target-side RDEPEND — so both are genuinely present in the solution
    /// and the conflict must be detected.
    #[test]
    fn check_blockers_fires_from_a_host_routed_packages_own_blocker() {
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        };

        repo.add_version(
            portage_atom::Cpv::parse("dev-build/user-1.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            PackageDeps {
                depend: (vec![DepEntry::Atom(Dep::parse("!dev-build/blocked").unwrap())]).into(),
                ..empty()
            },
        );
        repo.add_version(
            portage_atom::Cpv::parse("dev-build/blocked-1.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            empty(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            PackageDeps {
                bdepend: (vec![DepEntry::Atom(Dep::parse("dev-build/user").unwrap())]).into(),
                rdepend: (vec![DepEntry::Atom(Dep::parse("dev-build/blocked").unwrap())]).into(),
                ..empty()
            },
        );

        let mut provider = PortageDependencyProvider::new(repo);
        provider.set_cross_active(true);
        provider.set_with_bdeps(true);
        // No `add_host_installed`: the host lacks `user`, so it schedules as
        // an unsatisfied Host BDEPEND — a genuinely Host-flavored solved
        // package whose own `!dev-build/blocked` declaration must still be
        // read via the alias.

        let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
        let solution = provider
            .resolve_targets(vec![(a, PortageVersionSet::any())])
            .unwrap();

        let conflicts = provider.check_blockers(&solution);
        assert!(
            !conflicts.is_empty(),
            "Host-routed user's own blocker on blocked must be detected"
        );
    }

    #[test]
    fn check_blockers_skips_unsatisfied_use_conditional() {
        // openssl weakly blocks libressl only when libressl has `foo`; with foo
        // off, the blocker must not fire (mirrors firefox's `!glibc[crypt(-)]`).
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        };
        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/openssl-3.0.0").unwrap(),
            None,
            None,
            PackageDeps {
                depend: vec![DepEntry::Atom(
                    Dep::parse("!dev-libs/libressl[foo]").unwrap(),
                )]
                .into(),
                ..empty()
            },
        );
        repo.add_version_with_iuse(
            portage_atom::Cpv::parse("dev-libs/libressl-3.9.0").unwrap(),
            None,
            None,
            vec![Interned::intern("foo")], // foo in IUSE but off (empty config, no default)
            empty(),
        );

        let mut provider = PortageDependencyProvider::new(repo);
        let solution = provider
            .resolve_targets(vec![
                (
                    PortagePackage::unslotted(Cpn::parse("dev-libs/openssl").unwrap()),
                    PortageVersionSet::any(),
                ),
                (
                    PortagePackage::unslotted(Cpn::parse("dev-libs/libressl").unwrap()),
                    PortageVersionSet::any(),
                ),
            ])
            .unwrap();
        assert!(
            provider.check_blockers(&solution).is_empty(),
            "conditional blocker must not fire when libressl's `foo` is off"
        );
    }

    // A blocker on a solved package fires against a package that is only
    // *installed* (retained, never pulled into the solve) — mirrors
    // `sys-apps/systemd[resolvconf]` soft-blocking an installed
    // `net-dns/openresolv` that nothing in the graph depends on.
    #[test]
    fn check_blockers_fires_against_retained_installed() {
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        };
        repo.add_version(
            portage_atom::Cpv::parse("sys-apps/systemd-260.2").unwrap(),
            None,
            None,
            PackageDeps {
                rdepend: (vec![DepEntry::Atom(Dep::parse("!net-dns/openresolv").unwrap())]).into(),
                ..empty()
            },
        );

        let mut provider = PortageDependencyProvider::new(repo);
        // openresolv is installed but nothing pulls it into the solve.
        provider.add_installed(crate::provider::InstalledPackage {
            package: PortagePackage::unslotted(Cpn::parse("net-dns/openresolv").unwrap()),
            version: Version::parse("3.17.4").unwrap(),
            policy: crate::provider::InstalledPolicy::Favor,
            active_use: vec![],
            iuse: vec![],
        });

        let solution = provider
            .resolve_targets(vec![(
                PortagePackage::unslotted(Cpn::parse("sys-apps/systemd").unwrap()),
                PortageVersionSet::any(),
            )])
            .unwrap();
        assert!(
            solution
                .get(&PortagePackage::unslotted(
                    Cpn::parse("net-dns/openresolv").unwrap()
                ))
                .is_none(),
            "openresolv must not be in the solution (nothing depends on it)"
        );
        assert_eq!(
            provider.check_blockers(&solution).len(),
            1,
            "blocker against the retained installed openresolv must still fire"
        );

        // check_blockers_detailed twin: victim identity is preserved so a
        // classifier can distinguish "installed and orphaned" from "still
        // needed" without re-deriving it from a formatted string.
        let hits = provider.check_blockers_detailed(&solution);
        assert_eq!(hits.len(), 1);
        assert!(!hits[0].owner_installed, "owner is the solved systemd");
        assert_eq!(hits[0].victims.len(), 1);
        assert_eq!(
            hits[0].victims[0].package,
            PortagePackage::unslotted(Cpn::parse("net-dns/openresolv").unwrap())
        );
        assert_eq!(
            hits[0].victims[0].version,
            Version::parse("3.17.4").unwrap()
        );
        assert!(hits[0].victims[0].retained_installed);
    }

    // Reciprocal of the above: a blocker declared by a retained *installed*
    // package fires against a solved target — mirrors `net-dns/openresolv`'s
    // `!sys-apps/systemd` against a systemd in the plan. The owner is never in
    // the solve, so its blockers are fed via `add_installed_blockers`.
    #[test]
    fn check_blockers_fires_from_installed_owner() {
        let mut repo = InMemoryRepository::new();
        repo.add_version(
            portage_atom::Cpv::parse("sys-apps/systemd-260.2").unwrap(),
            None,
            None,
            PackageDeps {
                depend: (vec![]).into(),
                rdepend: (vec![]).into(),
                bdepend: (vec![]).into(),
                pdepend: (vec![]).into(),
                idepend: (vec![]).into(),
            },
        );

        let mut provider = PortageDependencyProvider::new(repo);
        let openresolv = PortagePackage::unslotted(Cpn::parse("net-dns/openresolv").unwrap());
        provider.add_installed(crate::provider::InstalledPackage {
            package: openresolv.clone(),
            version: Version::parse("3.17.4").unwrap(),
            policy: crate::provider::InstalledPolicy::Favor,
            active_use: vec![],
            iuse: vec![],
        });
        provider.add_installed_blockers(&openresolv, &[Dep::parse("!sys-apps/systemd").unwrap()]);

        let solution = provider
            .resolve_targets(vec![(
                PortagePackage::unslotted(Cpn::parse("sys-apps/systemd").unwrap()),
                PortageVersionSet::any(),
            )])
            .unwrap();
        assert_eq!(
            provider.check_blockers(&solution).len(),
            1,
            "installed openresolv's blocker against the solved systemd must fire"
        );

        // check_blockers_detailed twin: the reciprocal owner is flagged
        // installed, and its victim is the solved (not installed) systemd —
        // the shape a classifier needs to know "the owner is the removal
        // candidate here, not the victim".
        let hits = provider.check_blockers_detailed(&solution);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].owner_installed);
        assert_eq!(hits[0].owner, openresolv);
        assert_eq!(hits[0].victims.len(), 1);
        assert_eq!(
            hits[0].victims[0].package,
            PortagePackage::unslotted(Cpn::parse("sys-apps/systemd").unwrap())
        );
        assert!(!hits[0].victims[0].retained_installed);
    }

    #[test]
    fn check_blockers_no_conflict_when_clean() {
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        };

        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/openssl-3.0.0").unwrap(),
            None,
            None,
            PackageDeps {
                depend: (vec![DepEntry::Atom(Dep::parse("!dev-libs/libressl").unwrap())]).into(),
                rdepend: (vec![]).into(),
                bdepend: (vec![]).into(),
                pdepend: (vec![]).into(),
                idepend: (vec![]).into(),
            },
        );
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/foo-1.0").unwrap(),
            None,
            None,
            empty(),
        );

        let mut provider = PortageDependencyProvider::new(repo);
        let openssl = PortagePackage::unslotted(Cpn::parse("dev-libs/openssl").unwrap());
        let foo = PortagePackage::unslotted(Cpn::parse("app-misc/foo").unwrap());
        let solution = provider
            .resolve_targets(vec![
                (openssl, PortageVersionSet::any()),
                (foo, PortageVersionSet::any()),
            ])
            .unwrap();
        let conflicts = provider.check_blockers(&solution);
        assert!(
            conflicts.is_empty(),
            "no blocker conflict expected: {conflicts:?}"
        );
    }

    #[test]
    fn check_use_deps_enabled_satisfied() {
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        };

        repo.add_version_with_iuse(
            portage_atom::Cpv::parse("dev-libs/openssl-3.0.0").unwrap(),
            None,
            None,
            vec![Interned::intern("ssl")],
            empty(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/myapp-1.0").unwrap(),
            None,
            None,
            PackageDeps {
                depend: (DepEntry::parse("dev-libs/openssl[ssl]").unwrap()).into(),
                rdepend: (vec![]).into(),
                bdepend: (vec![]).into(),
                pdepend: (vec![]).into(),
                idepend: (vec![]).into(),
            },
        );

        let mut use_config = UseConfig::new();
        use_config.enable(Interned::intern("ssl"));

        let mut provider = {
            repo.set_use_config(use_config.clone());
            PortageDependencyProvider::new(repo)
        };
        let myapp = PortagePackage::unslotted(Cpn::parse("app-misc/myapp").unwrap());
        let solution = provider
            .resolve_targets(vec![(myapp, PortageVersionSet::any())])
            .unwrap();
        let errors = provider.check_use_deps(&solution, &use_config);
        assert!(errors.is_empty(), "unexpected USE-dep errors: {errors:?}");
    }

    #[test]
    fn check_use_deps_enabled_violated() {
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        };

        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/openssl-3.0.0").unwrap(),
            None,
            None,
            empty(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/myapp-1.0").unwrap(),
            None,
            None,
            PackageDeps {
                depend: (DepEntry::parse("dev-libs/openssl[ssl]").unwrap()).into(),
                rdepend: (vec![]).into(),
                bdepend: (vec![]).into(),
                pdepend: (vec![]).into(),
                idepend: (vec![]).into(),
            },
        );

        let use_config = UseConfig::new();

        let mut provider = {
            repo.set_use_config(use_config.clone());
            PortageDependencyProvider::new(repo)
        };
        let myapp = PortagePackage::unslotted(Cpn::parse("app-misc/myapp").unwrap());
        let solution = provider
            .resolve_targets(vec![(myapp, PortageVersionSet::any())])
            .unwrap();
        let errors = provider.check_use_deps(&solution, &use_config);
        assert!(
            !errors.is_empty(),
            "should detect USE-dep violation for [ssl] when ssl is off"
        );
    }

    #[test]
    fn check_use_deps_conditional_with_parent_on() {
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        };

        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/openssl-3.0.0").unwrap(),
            None,
            None,
            empty(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/myapp-1.0").unwrap(),
            None,
            None,
            PackageDeps {
                depend: (DepEntry::parse("dev-libs/openssl[ssl?]").unwrap()).into(),
                rdepend: (vec![]).into(),
                bdepend: (vec![]).into(),
                pdepend: (vec![]).into(),
                idepend: (vec![]).into(),
            },
        );

        let mut use_config = UseConfig::new();
        use_config.enable(Interned::intern("ssl"));

        let mut provider = {
            repo.set_use_config(use_config.clone());
            PortageDependencyProvider::new(repo)
        };
        let myapp = PortagePackage::unslotted(Cpn::parse("app-misc/myapp").unwrap());
        let solution = provider
            .resolve_targets(vec![(myapp, PortageVersionSet::any())])
            .unwrap();
        let errors = provider.check_use_deps(&solution, &use_config);
        assert!(
            !errors.is_empty(),
            "[ssl?] with parent ssl=ON should require target ssl=ON"
        );
    }

    #[test]
    fn check_use_deps_conditional_with_parent_off() {
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        };

        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/openssl-3.0.0").unwrap(),
            None,
            None,
            empty(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/myapp-1.0").unwrap(),
            None,
            None,
            PackageDeps {
                depend: (DepEntry::parse("dev-libs/openssl[ssl?]").unwrap()).into(),
                rdepend: (vec![]).into(),
                bdepend: (vec![]).into(),
                pdepend: (vec![]).into(),
                idepend: (vec![]).into(),
            },
        );

        let use_config = UseConfig::new();

        let mut provider = {
            repo.set_use_config(use_config.clone());
            PortageDependencyProvider::new(repo)
        };
        let myapp = PortagePackage::unslotted(Cpn::parse("app-misc/myapp").unwrap());
        let solution = provider
            .resolve_targets(vec![(myapp, PortageVersionSet::any())])
            .unwrap();
        let errors = provider.check_use_deps(&solution, &use_config);
        assert!(
            errors.is_empty(),
            "[ssl?] with parent ssl=OFF should be unconstrained"
        );
    }

    #[test]
    fn check_use_deps_conditional_inverse_parent_off_requires_target_off() {
        // PMS 8.2.6.4: `openssl[!ssl?]` = `ssl? ( openssl ) !ssl? ( openssl[-ssl] )`.
        // With the parent's `ssl` OFF, the `!ssl?` branch requires the target's
        // `ssl` be *disabled*. Target `ssl` is off (undeclared ⇒ default off), so
        // the constraint is satisfied — no error. The pre-fix code inverted this
        // (demanded target `ssl` ON), so it wrongly reported a conflict here.
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        };

        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/openssl-3.0.0").unwrap(),
            None,
            None,
            empty(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/myapp-1.0").unwrap(),
            None,
            None,
            PackageDeps {
                depend: (DepEntry::parse("dev-libs/openssl[!ssl?]").unwrap()).into(),
                rdepend: (vec![]).into(),
                bdepend: (vec![]).into(),
                pdepend: (vec![]).into(),
                idepend: (vec![]).into(),
            },
        );

        let use_config = UseConfig::new();

        let mut provider = {
            repo.set_use_config(use_config.clone());
            PortageDependencyProvider::new(repo)
        };
        let myapp = PortagePackage::unslotted(Cpn::parse("app-misc/myapp").unwrap());
        let solution = provider
            .resolve_targets(vec![(myapp, PortageVersionSet::any())])
            .unwrap();
        let errors = provider.check_use_deps(&solution, &use_config);
        assert!(
            errors.is_empty(),
            "[!ssl?] with parent ssl=OFF and target ssl=OFF should be satisfied (require target -ssl), got {errors:?}"
        );
    }

    #[test]
    fn check_use_deps_equal_matches() {
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        };

        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/openssl-3.0.0").unwrap(),
            None,
            None,
            empty(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/myapp-1.0").unwrap(),
            None,
            None,
            PackageDeps {
                depend: (DepEntry::parse("dev-libs/openssl[ssl=]").unwrap()).into(),
                rdepend: (vec![]).into(),
                bdepend: (vec![]).into(),
                pdepend: (vec![]).into(),
                idepend: (vec![]).into(),
            },
        );

        let use_config = UseConfig::new();

        let mut provider = {
            repo.set_use_config(use_config.clone());
            PortageDependencyProvider::new(repo)
        };
        let myapp = PortagePackage::unslotted(Cpn::parse("app-misc/myapp").unwrap());
        let solution = provider
            .resolve_targets(vec![(myapp, PortageVersionSet::any())])
            .unwrap();
        let errors = provider.check_use_deps(&solution, &use_config);
        assert!(
            errors.is_empty(),
            "[ssl=] with both parent/target ssl=OFF should match"
        );
    }

    #[test]
    fn check_repo_constraint_satisfied() {
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        };
        let gentoo = Interned::intern("gentoo");

        repo.add_version_full(
            portage_atom::Cpv::parse("dev-libs/openssl-3.0.0").unwrap(),
            None,
            None,
            Some(gentoo),
            vec![],
            empty(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/myapp-1.0").unwrap(),
            None,
            None,
            PackageDeps {
                depend: (DepEntry::parse("dev-libs/openssl::gentoo").unwrap()).into(),
                rdepend: (vec![]).into(),
                bdepend: (vec![]).into(),
                pdepend: (vec![]).into(),
                idepend: (vec![]).into(),
            },
        );

        let mut provider = PortageDependencyProvider::new(repo);
        let myapp = PortagePackage::unslotted(Cpn::parse("app-misc/myapp").unwrap());
        let solution = provider
            .resolve_targets(vec![(myapp, PortageVersionSet::any())])
            .unwrap();
        let errors = provider.check_repo_constraints(&solution);
        assert!(
            errors.is_empty(),
            "repo constraint should be satisfied: {errors:?}"
        );
    }

    #[test]
    fn check_repo_constraint_violated() {
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        };

        repo.add_version_full(
            portage_atom::Cpv::parse("dev-libs/openssl-3.0.0").unwrap(),
            None,
            None,
            Some(Interned::intern("other")),
            vec![],
            empty(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/myapp-1.0").unwrap(),
            None,
            None,
            PackageDeps {
                depend: (DepEntry::parse("dev-libs/openssl::gentoo").unwrap()).into(),
                rdepend: (vec![]).into(),
                bdepend: (vec![]).into(),
                pdepend: (vec![]).into(),
                idepend: (vec![]).into(),
            },
        );

        let mut provider = PortageDependencyProvider::new(repo);
        let myapp = PortagePackage::unslotted(Cpn::parse("app-misc/myapp").unwrap());
        let solution = provider
            .resolve_targets(vec![(myapp, PortageVersionSet::any())])
            .unwrap();
        let errors = provider.check_repo_constraints(&solution);
        assert!(
            !errors.is_empty(),
            "should detect repo constraint violation"
        );
    }

    #[test]
    fn check_blockers_respects_slot() {
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        };

        let slot_0 = Interned::<DefaultInterner>::intern("0");
        let slot_1 = Interned::<DefaultInterner>::intern("1");

        repo.add_version(
            portage_atom::Cpv::parse("app-misc/blocker-pkg-1.0").unwrap(),
            Some(slot_0),
            None,
            PackageDeps {
                depend: (vec![DepEntry::Atom(Dep::parse("!app-misc/target:0").unwrap())]).into(),
                rdepend: (vec![]).into(),
                bdepend: (vec![]).into(),
                pdepend: (vec![]).into(),
                idepend: (vec![]).into(),
            },
        );
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/target-1.0").unwrap(),
            Some(slot_0),
            None,
            empty(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/target-1.0").unwrap(),
            Some(slot_1),
            None,
            empty(),
        );

        // Part 1: target:0 should conflict with blocker !target:0
        let blocker_pkg =
            PortagePackage::slotted(Cpn::parse("app-misc/blocker-pkg").unwrap(), slot_0);
        let target_slot0 = PortagePackage::slotted(Cpn::parse("app-misc/target").unwrap(), slot_0);

        let mut provider = PortageDependencyProvider::new(repo);
        let solution = provider
            .resolve_targets(vec![
                (blocker_pkg.clone(), PortageVersionSet::any()),
                (target_slot0, PortageVersionSet::any()),
            ])
            .unwrap();
        let conflicts = provider.check_blockers(&solution);
        assert!(
            !conflicts.is_empty(),
            "blocker !target:0 should conflict with target:0 in solution"
        );

        // Part 2: !target:0 should NOT conflict when only target:1 is in solution
        let mut repo2 = InMemoryRepository::new();
        repo2.add_version(
            portage_atom::Cpv::parse("app-misc/blocker-pkg-1.0").unwrap(),
            Some(slot_0),
            None,
            PackageDeps {
                depend: (vec![DepEntry::Atom(Dep::parse("!app-misc/target:0").unwrap())]).into(),
                rdepend: (vec![]).into(),
                bdepend: (vec![]).into(),
                pdepend: (vec![]).into(),
                idepend: (vec![]).into(),
            },
        );
        repo2.add_version(
            portage_atom::Cpv::parse("app-misc/target-1.0").unwrap(),
            Some(slot_1),
            None,
            empty(),
        );

        let target_slot1 = PortagePackage::slotted(Cpn::parse("app-misc/target").unwrap(), slot_1);
        let mut provider2 = PortageDependencyProvider::new(repo2);
        let solution2 = provider2
            .resolve_targets(vec![
                (blocker_pkg, PortageVersionSet::any()),
                (target_slot1, PortageVersionSet::any()),
            ])
            .unwrap();
        let conflicts2 = provider2.check_blockers(&solution2);
        assert!(
            conflicts2.is_empty(),
            "blocker !target:0 should NOT conflict with target:1 in solution"
        );
    }

    #[test]
    fn check_blockers_approximate_operator() {
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        };

        // app-misc/app blocks ~dev-libs/lib-1.0 (all revisions of 1.0)
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/app-1.0").unwrap(),
            None,
            None,
            PackageDeps {
                depend: (vec![DepEntry::Atom(Dep::parse("!~dev-libs/lib-1.0").unwrap())]).into(),
                rdepend: (vec![]).into(),
                bdepend: (vec![]).into(),
                pdepend: (vec![]).into(),
                idepend: (vec![]).into(),
            },
        );
        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/lib-1.0-r3").unwrap(),
            None,
            None,
            empty(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/lib-2.0").unwrap(),
            None,
            None,
            empty(),
        );

        let app = PortagePackage::unslotted(Cpn::parse("app-misc/app").unwrap());
        let lib = PortagePackage::unslotted(Cpn::parse("dev-libs/lib").unwrap());

        // lib-1.0-r3 is a revision of 1.0 — should conflict with !~lib-1.0
        let mut provider = PortageDependencyProvider::new(repo.clone());
        let solution = provider
            .resolve_targets(vec![
                (app.clone(), PortageVersionSet::any()),
                (
                    lib.clone(),
                    crate::version_set::PortageVersionSet::from_operator(
                        portage_atom::Operator::Approximate,
                        false,
                        portage_atom::Version::parse("1.0").unwrap(),
                    ),
                ),
            ])
            .unwrap();
        let conflicts = provider.check_blockers(&solution);
        assert!(
            !conflicts.is_empty(),
            "!~lib-1.0 should conflict with lib-1.0-r3 (a revision of 1.0)"
        );

        // lib-2.0 is a different base version — should NOT conflict with !~lib-1.0
        let mut provider2 = PortageDependencyProvider::new(repo);
        let solution2 = provider2
            .resolve_targets(vec![
                (app, PortageVersionSet::any()),
                (
                    lib,
                    crate::version_set::PortageVersionSet::from_operator(
                        portage_atom::Operator::GreaterOrEqual,
                        false,
                        portage_atom::Version::parse("2.0").unwrap(),
                    ),
                ),
            ])
            .unwrap();
        let conflicts2 = provider2.check_blockers(&solution2);
        assert!(
            conflicts2.is_empty(),
            "!~lib-1.0 should NOT conflict with lib-2.0: {conflicts2:?}"
        );
    }

    #[test]
    fn check_blockers_version_matched() {
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        };

        repo.add_version(
            portage_atom::Cpv::parse("app-misc/app-1.0").unwrap(),
            None,
            None,
            PackageDeps {
                depend: (vec![DepEntry::Atom(Dep::parse("!>=dev-libs/lib-2.0").unwrap())]).into(),
                rdepend: (vec![]).into(),
                bdepend: (vec![]).into(),
                pdepend: (vec![]).into(),
                idepend: (vec![]).into(),
            },
        );
        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/lib-1.0").unwrap(),
            None,
            None,
            empty(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/lib-2.0").unwrap(),
            None,
            None,
            empty(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/lib-3.0").unwrap(),
            None,
            None,
            empty(),
        );

        let mut provider = PortageDependencyProvider::new(repo);
        let app = PortagePackage::unslotted(Cpn::parse("app-misc/app").unwrap());
        let lib_pkg = PortagePackage::unslotted(Cpn::parse("dev-libs/lib").unwrap());
        let solution = provider
            .resolve_targets(vec![
                (app, PortageVersionSet::any()),
                (lib_pkg, PortageVersionSet::any()),
            ])
            .unwrap();
        let lib_ver = solution
            .get(&PortagePackage::unslotted(
                Cpn::parse("dev-libs/lib").unwrap(),
            ))
            .unwrap();

        let conflicts = provider.check_blockers(&solution);
        if lib_ver >= &Version::parse("2.0").unwrap() {
            assert!(
                !conflicts.is_empty(),
                "blocker !>=lib-2.0 should conflict with lib-{}",
                lib_ver
            );
        }
    }
}
