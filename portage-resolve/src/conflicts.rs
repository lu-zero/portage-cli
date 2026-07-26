use std::collections::{HashMap, HashSet};

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, Dep, DepEntry, Operator, Version};
use portage_atom_pubgrub::PortageVersionSet;

use crate::installed::VdbEntry;

/// An interned slot name (`None` = unslotted). Interned handles are cheap to
/// copy and compare, so the whole conflict check stays handle-based.
type Slot = Option<Interned<DefaultInterner>>;

/// A constraint violated by the proposed solution.
pub struct Conflict {
    /// The installed package whose dep is violated.
    pub installed_cpn: Cpn,
    /// The installed package's version.
    pub installed_ver: Version,
    /// The dep atom that is not satisfied.
    pub dep: Dep,
    /// The version the solver chose (which violates the dep).
    pub proposed_ver: Version,
    /// The version the plan installs over this same installed package, when it
    /// does. `Some` means the constraint is stale rather than broken: the
    /// package that carries it is itself being replaced in this run, so the
    /// dep string evaluated here belongs to a build that will not survive the
    /// plan. Lockstep families (`~`-pinned llvm/clang/lldb, the perl virtuals)
    /// are entirely this case, so it is reported apart from real breakage
    /// rather than silently dropped.
    pub owner_replaced_by: Option<Version>,
}

/// A package the plan installs or upgrades, carrying its slot so the conflict
/// check can reason per-slot rather than collapsing every slot of a name into
/// one version.
pub struct ProposedPkg {
    /// The package name.
    pub cpn: Cpn,
    /// Its slot, if any.
    pub slot: Slot,
    /// The version the plan installs.
    pub version: Version,
}

/// Check all installed packages' dep strings against the proposed solution.
///
/// Returns one `Conflict` per violated constraint.  A dependency is only a
/// conflict when **no** package present after the plan satisfies it — where
/// "present" means a proposed package *plus* every installed package the plan
/// does not replace in the same `(cpn, slot)`.  This is slot-aware: pulling a
/// new slot (e.g. `llvm:21`) alongside a retained old slot (`llvm:20`) does not
/// break an installed consumer that pinned `~llvm:20`, whereas an in-slot
/// upgrade past a `<` bound (e.g. `docutils:0` to `0.23`) still does.
pub fn find_conflicts(installed: &[VdbEntry], proposed: &[ProposedPkg]) -> Vec<Conflict> {
    // `(cpn, slot)` the plan installs into, and with what; a same-slot installed
    // package is replaced and therefore not retained.
    let replaced: HashMap<(Cpn, Slot), &Version> = proposed
        .iter()
        .map(|p| ((p.cpn, p.slot), &p.version))
        .collect();

    // Only names the plan actually touches can introduce a new conflict; a dep
    // on an untouched name is unchanged by the plan.
    let touched: HashSet<Cpn> = proposed.iter().map(|p| p.cpn).collect();

    // Every package that will exist after the plan, keyed by name. Slotted
    // packages contribute one entry per coexisting slot.
    let mut present: HashMap<Cpn, Vec<(Slot, Cpv)>> = HashMap::new();
    for p in proposed {
        present
            .entry(p.cpn)
            .or_default()
            .push((p.slot, Cpv::new(p.cpn, p.version.clone())));
    }
    for e in installed {
        let slot = e.slot.as_deref().map(Interned::intern);
        if replaced.contains_key(&(e.cpn, slot)) {
            continue;
        }
        present
            .entry(e.cpn)
            .or_default()
            .push((slot, Cpv::new(e.cpn, e.version.clone())));
    }

    let mut conflicts = Vec::new();
    for entry in installed {
        let active_flags: HashSet<Interned<_>> = entry.active_use.iter().copied().collect();
        let evaluated = DepEntry::evaluate_use(&entry.deps, &active_flags);
        let slot = entry.slot.as_deref().map(Interned::intern);
        let owner_replaced_by = replaced.get(&(entry.cpn, slot)).map(|v| (*v).clone());
        collect_violations(
            &evaluated,
            entry,
            owner_replaced_by.as_ref(),
            &touched,
            &present,
            &mut conflicts,
        );
    }
    conflicts
}

/// True if any package present after the plan satisfies `dep` (name, slot and
/// version all considered).
fn dep_satisfied(dep: &Dep, present: &HashMap<Cpn, Vec<(Slot, Cpv)>>) -> bool {
    let Some(cands) = present.get(&dep.cpn) else {
        return false;
    };
    cands
        .iter()
        .any(|(slot, cpv)| dep.matches_cpv(cpv, slot.as_deref()))
}

fn collect_violations(
    entries: &[DepEntry],
    owner: &VdbEntry,
    owner_replaced_by: Option<&Version>,
    touched: &HashSet<Cpn>,
    present: &HashMap<Cpn, Vec<(Slot, Cpv)>>,
    out: &mut Vec<Conflict>,
) {
    for entry in entries {
        match entry {
            DepEntry::Atom(dep) => {
                if dep.blocker.is_some() || !touched.contains(&dep.cpn) {
                    continue;
                }
                if !dep_satisfied(dep, present) {
                    // The name is touched but no present package satisfies the
                    // dep. Proposed packages are pushed before retained ones, so
                    // the first present entry is the proposed version to blame.
                    let proposed_ver = present
                        .get(&dep.cpn)
                        .and_then(|c| c.first())
                        .map(|(_, cpv)| cpv.version.clone());
                    if let Some(proposed_ver) = proposed_ver {
                        out.push(Conflict {
                            installed_cpn: owner.cpn,
                            installed_ver: owner.version.clone(),
                            dep: dep.clone(),
                            proposed_ver,
                            owner_replaced_by: owner_replaced_by.cloned(),
                        });
                    }
                }
            }
            DepEntry::AllOf(children)
            | DepEntry::ExactlyOneOf(children)
            | DepEntry::AtMostOneOf(children) => {
                // Treat ^^/?? like AllOf for reverse-dep advisory: any
                // unsatisfied branch that names a touched package is reported.
                // Coarser than full group semantics, but previously these
                // groups were ignored entirely.
                collect_violations(children, owner, owner_replaced_by, touched, present, out);
            }
            // AnyOf: a conflict only exists if ALL alternatives are violated.
            DepEntry::AnyOf(children) => {
                let branch_violations: Vec<Vec<Conflict>> = children
                    .iter()
                    .map(|child| {
                        let mut v = Vec::new();
                        collect_violations(
                            std::slice::from_ref(child),
                            owner,
                            owner_replaced_by,
                            touched,
                            present,
                            &mut v,
                        );
                        v
                    })
                    .collect();
                // If every branch is violated, the OR group as a whole is
                // violated. We report the first branch's violations as
                // representative.
                let all_violated = branch_violations.iter().all(|v| !v.is_empty());
                if all_violated && let Some(first) = branch_violations.into_iter().next() {
                    out.extend(first);
                }
            }
            // Atom handled above; UseConditional already stripped by evaluate_use.
            DepEntry::UseConditional { .. } => {}
        }
    }
}

/// An installed package's active blocker atoms (USE conditionals resolved
/// against its VDB flags). Fed to the solver so `check_blockers` can report a
/// blocker a retained installed owner points at the plan — the owner is never
/// in the solve graph, so its blockers are otherwise invisible.
pub fn installed_blocker_atoms(entry: &VdbEntry) -> Vec<Dep> {
    // Most installed packages declare no blockers; a cheap structural pre-scan
    // skips the evaluate_use + clone for them, keeping this whole-VDB walk cheap.
    if !has_blocker_atom(&entry.deps) {
        return Vec::new();
    }
    let active: HashSet<Interned<DefaultInterner>> = entry.active_use.iter().copied().collect();
    let evaluated = DepEntry::evaluate_use(&entry.deps, &active);
    let mut out = Vec::new();
    collect_blocker_atoms(&evaluated, &mut out);
    out
}

/// Whether any atom anywhere in the (unevaluated) dep tree is a blocker.
fn has_blocker_atom(entries: &[DepEntry]) -> bool {
    entries.iter().any(|entry| match entry {
        DepEntry::Atom(dep) => dep.blocker.is_some(),
        DepEntry::UseConditional { children, .. }
        | DepEntry::AllOf(children)
        | DepEntry::AnyOf(children)
        | DepEntry::ExactlyOneOf(children)
        | DepEntry::AtMostOneOf(children) => has_blocker_atom(children),
    })
}

fn collect_blocker_atoms(entries: &[DepEntry], out: &mut Vec<Dep>) {
    for entry in entries {
        match entry {
            DepEntry::Atom(dep) if dep.blocker.is_some() => out.push(dep.clone()),
            DepEntry::AllOf(children) | DepEntry::AnyOf(children) => {
                collect_blocker_atoms(children, out)
            }
            _ => {}
        }
    }
}

/// Translate a dep atom's version constraint into the solver's
/// [`PortageVersionSet`] (a bare atom with no version op accepts any version).
pub fn dep_to_version_set(dep: &Dep) -> PortageVersionSet {
    match &dep.version {
        None => PortageVersionSet::any(),
        Some(v) => {
            let op = dep.op.unwrap_or(Operator::GreaterOrEqual);
            PortageVersionSet::from_operator(op, dep.glob, v.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(cpn: &str, slot: &str, version: &str, deps: &[&str]) -> VdbEntry {
        VdbEntry {
            cpn: Cpn::try_new(cpn).expect("test cpn parses"),
            slot: Some(Interned::intern(slot)),
            version: version.parse().expect("test version parses"),
            active_use: Vec::new(),
            iuse: Vec::new(),
            deps: deps
                .iter()
                .map(|d| DepEntry::Atom(Dep::parse(d).expect("test atom parses")))
                .collect(),
        }
    }

    fn proposed(cpn: &str, slot: &str, version: &str) -> ProposedPkg {
        ProposedPkg {
            cpn: Cpn::try_new(cpn).expect("test cpn parses"),
            slot: Some(Interned::intern(slot)),
            version: version.parse().expect("test version parses"),
        }
    }

    /// The llvm/clang/lldb shape: a `~`-pinned family moving in lockstep. The
    /// pin is violated in isolation, but its owner is upgraded in the same run,
    /// so it is stale rather than broken.
    #[test]
    fn a_replaced_owners_violated_pin_is_marked_stale() {
        let installed = vec![entry(
            "llvm-core/clang",
            "22",
            "22.1.6",
            &["~llvm-core/llvm-22.1.6"],
        )];
        let plan = vec![
            proposed("llvm-core/llvm", "22", "22.1.8"),
            proposed("llvm-core/clang", "22", "22.1.8"),
        ];
        let got = find_conflicts(&installed, &plan);
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].owner_replaced_by.as_ref().map(ToString::to_string),
            Some("22.1.8".to_owned())
        );
    }

    /// The same pin, but its owner stays installed: real breakage.
    #[test]
    fn a_retained_owners_violated_pin_is_a_real_conflict() {
        let installed = vec![entry(
            "llvm-core/lldb",
            "22",
            "22.1.6",
            &["~llvm-core/llvm-22.1.6"],
        )];
        let plan = vec![proposed("llvm-core/llvm", "22", "22.1.8")];
        let got = find_conflicts(&installed, &plan);
        assert_eq!(got.len(), 1);
        assert!(got[0].owner_replaced_by.is_none());
    }

    /// Replacement is per `(cpn, slot)`: upgrading another slot of the same
    /// name leaves this owner in place.
    #[test]
    fn replacement_is_slot_specific() {
        let installed = vec![entry(
            "llvm-core/lldb",
            "21",
            "21.1.8",
            &["~llvm-core/llvm-22.1.6"],
        )];
        let plan = vec![
            proposed("llvm-core/llvm", "22", "22.1.8"),
            proposed("llvm-core/lldb", "22", "22.1.8"),
        ];
        let got = find_conflicts(&installed, &plan);
        assert_eq!(got.len(), 1);
        assert!(got[0].owner_replaced_by.is_none());
    }

    /// A retained slot still satisfying the pin is not a conflict at all.
    #[test]
    fn a_retained_slot_that_satisfies_the_pin_reports_nothing() {
        let installed = vec![
            entry(
                "llvm-core/lldb",
                "21",
                "21.1.8",
                &["~llvm-core/llvm-21.1.8"],
            ),
            entry("llvm-core/llvm", "21", "21.1.8", &[]),
        ];
        let plan = vec![proposed("llvm-core/llvm", "22", "22.1.8")];
        assert!(find_conflicts(&installed, &plan).is_empty());
    }
}
