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
    // `(cpn, slot)` pairs the plan installs into; a same-slot installed package
    // is replaced and therefore not retained.
    let replaced: HashSet<(Cpn, Slot)> = proposed.iter().map(|p| (p.cpn, p.slot)).collect();

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
        if replaced.contains(&(e.cpn, slot)) {
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
        collect_violations(&evaluated, entry, &touched, &present, &mut conflicts);
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
                collect_violations(children, owner, touched, present, out);
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
