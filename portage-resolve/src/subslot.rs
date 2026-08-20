//! Slot-operator (`:=`) rebuild detection
//!
//! When a package is merged, portage rewrites each `:=` dependency in its VDB
//! entry to the *bound* form `:slot/subslot=`. If a later plan moves that
//! dependency to a version with a different subslot, the consumer must be
//! rebuilt against the new ABI — emerge pulls it via the internal
//! `__auto_slot_operator_replace_installed__` set and marks both ends with the
//! `r` (forced rebuild) flag. This module detects those consumers from the
//! recorded bindings.

use std::collections::{HashMap, HashSet};

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, DepEntry, Slot, SlotDep, SlotOperator, Version};

use crate::conflicts::dep_to_version_set;
use crate::installed::VdbEntry;

/// An installed package needing a rebuild because a planned dependency's
/// subslot no longer matches the `:slot/subslot=` binding recorded in the VDB
pub struct SubslotRebuild {
    /// The installed package that needs rebuilding
    pub cpn: Cpn,
    /// Its installed slot
    pub slot: Option<Interned<DefaultInterner>>,
    /// Its installed version
    pub version: Version,
    /// The planned packages whose subslot change triggers the rebuild
    pub triggers: Vec<Cpn>,
}

/// The effective subslot of a planned version: PMS defaults the subslot to the
/// slot itself when the ebuild declares none
fn effective_subslot(slot: &Slot) -> Interned<DefaultInterner> {
    slot.subslot.unwrap_or(slot.slot)
}

fn collect_bound_atoms<'a>(entries: &'a [DepEntry], out: &mut Vec<&'a portage_atom::Dep>) {
    for entry in entries {
        match entry {
            DepEntry::Atom(dep)
                if dep.blocker.is_none()
                    && matches!(
                        &dep.slot_dep,
                        Some(SlotDep::Slot {
                            slot: Some(s),
                            op: Some(SlotOperator::Equal),
                        }) if s.subslot.is_some()
                    ) =>
            {
                out.push(dep);
            }
            DepEntry::AllOf(children)
            | DepEntry::AnyOf(children)
            | DepEntry::ExactlyOneOf(children)
            | DepEntry::AtMostOneOf(children) => {
                collect_bound_atoms(children, out);
            }
            DepEntry::Atom(_) | DepEntry::UseConditional { .. } => {}
        }
    }
}

/// Find installed packages whose recorded `:=` bindings are invalidated by the plan
///
/// `planned_slots` maps each in-plan CPN to its planned versions and their tree
/// slots; owners already in the plan are skipped (they are being upgraded or
/// rebuilt anyway).
pub fn find_rebuilds(
    installed: &[VdbEntry],
    planned_slots: &HashMap<Cpn, Vec<(Version, Slot)>>,
    in_plan: &HashSet<Cpn>,
) -> Vec<SubslotRebuild> {
    let mut rebuilds = Vec::new();

    for entry in installed {
        if in_plan.contains(&entry.cpn) {
            continue;
        }
        let active: HashSet<Interned<DefaultInterner>> = entry.active_use.iter().copied().collect();
        let evaluated = DepEntry::evaluate_use(&entry.deps, &active);
        let mut atoms = Vec::new();
        collect_bound_atoms(&evaluated, &mut atoms);

        let mut triggers: Vec<Cpn> = Vec::new();
        for dep in atoms {
            let Some(SlotDep::Slot { slot: Some(s), .. }) = &dep.slot_dep else {
                continue;
            };
            let Some(bound_subslot) = s.subslot else {
                // This should not happen as collect_bound_atoms filters for
                // s.subslot.is_some(), but handle it gracefully
                continue;
            };
            let Some(planned) = planned_slots.get(&dep.cpn) else {
                continue;
            };
            let vs = dep_to_version_set(dep);
            // A planned version in the bound slot, satisfying the dep's version
            // range, but with a different subslot ⇒ the binding is invalidated.
            // (A range mismatch is a dependency conflict, reported elsewhere.)
            let invalidated = planned.iter().any(|(ver, pslot)| {
                pslot.slot == s.slot
                    && vs.contains(ver)
                    && effective_subslot(pslot) != bound_subslot
            });
            if invalidated && !triggers.contains(&dep.cpn) {
                triggers.push(dep.cpn);
            }
        }
        if !triggers.is_empty() {
            triggers.sort_by_key(|c| c.to_string());
            rebuilds.push(SubslotRebuild {
                cpn: entry.cpn,
                slot: entry.slot,
                version: entry.version.clone(),
                triggers,
            });
        }
    }

    rebuilds.sort_by_key(|r| r.cpn.to_string());
    rebuilds
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vdb(cpn: &str, ver: &str, deps: &str) -> VdbEntry {
        VdbEntry {
            cpn: Cpn::parse(cpn).unwrap(),
            slot: Some(Interned::intern("0")),
            version: Version::parse(ver).unwrap(),
            active_use: Vec::new(),
            iuse: Vec::new(),
            eapi: portage_metadata::Eapi::Eight,
            deps: DepEntry::parse(deps).unwrap(),
        }
    }

    fn slot(s: &str, sub: Option<&str>) -> Slot {
        Slot {
            slot: Interned::intern(s),
            subslot: sub.map(Interned::intern),
        }
    }

    fn planned(items: &[(&str, &str, &str, Option<&str>)]) -> HashMap<Cpn, Vec<(Version, Slot)>> {
        let mut m: HashMap<Cpn, Vec<(Version, Slot)>> = HashMap::new();
        for (cpn, ver, s, sub) in items {
            m.entry(Cpn::parse(cpn).unwrap())
                .or_default()
                .push((Version::parse(ver).unwrap(), slot(s, *sub)));
        }
        m
    }

    #[test]
    fn subslot_change_triggers_rebuild() {
        let installed = vec![vdb(
            "net-libs/nodejs",
            "24.14.0",
            ">=dev-cpp/simdutf-5:0/25=",
        )];
        let plan = planned(&[("dev-cpp/simdutf", "9.0.0", "0", Some("34"))]);
        let got = find_rebuilds(&installed, &plan, &HashSet::new());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].cpn.to_string(), "net-libs/nodejs");
        assert_eq!(got[0].triggers[0].to_string(), "dev-cpp/simdutf");
    }

    #[test]
    fn same_subslot_no_rebuild() {
        let installed = vec![vdb(
            "net-libs/nodejs",
            "24.14.0",
            ">=dev-cpp/simdutf-5:0/25=",
        )];
        let plan = planned(&[("dev-cpp/simdutf", "8.0.0", "0", Some("25"))]);
        assert!(find_rebuilds(&installed, &plan, &HashSet::new()).is_empty());
    }

    #[test]
    fn owner_already_in_plan_skipped() {
        let installed = vec![vdb(
            "net-libs/nodejs",
            "24.14.0",
            ">=dev-cpp/simdutf-5:0/25=",
        )];
        let plan = planned(&[("dev-cpp/simdutf", "9.0.0", "0", Some("34"))]);
        let in_plan: HashSet<Cpn> = [Cpn::parse("net-libs/nodejs").unwrap()].into();
        assert!(find_rebuilds(&installed, &plan, &in_plan).is_empty());
    }

    #[test]
    fn version_range_mismatch_is_not_a_rebuild() {
        // The planned version falls outside the dep's range: that is a
        // dependency conflict (reported by conflicts.rs), not a subslot rebuild.
        let installed = vec![vdb(
            "net-libs/nodejs",
            "24.14.0",
            "<dev-cpp/simdutf-8:0/25=",
        )];
        let plan = planned(&[("dev-cpp/simdutf", "9.0.0", "0", Some("34"))]);
        assert!(find_rebuilds(&installed, &plan, &HashSet::new()).is_empty());
    }

    #[test]
    fn missing_subslot_defaults_to_slot() {
        // Planned SLOT="0" (no subslot) vs bound :0/25= ⇒ effective subslot "0"
        // differs from the recorded "25" ⇒ rebuild.
        let installed = vec![vdb(
            "net-libs/nodejs",
            "24.14.0",
            ">=dev-cpp/simdutf-5:0/25=",
        )];
        let plan = planned(&[("dev-cpp/simdutf", "9.0.0", "0", None)]);
        let got = find_rebuilds(&installed, &plan, &HashSet::new());
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn other_slot_does_not_trigger() {
        let installed = vec![vdb("dev-tcltk/expect", "5.45.4", "dev-lang/tcl:0/8.6=")];
        let plan = planned(&[("dev-lang/tcl", "9.0.3", "9", Some("9.0"))]);
        assert!(find_rebuilds(&installed, &plan, &HashSet::new()).is_empty());
    }
}
