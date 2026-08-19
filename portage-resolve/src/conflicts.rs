use std::collections::{HashMap, HashSet};

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Blocker, Cpn, Cpv, Dep, DepEntry, Operator, Version};
use portage_atom_pubgrub::{BlockerHit, PortageVersionSet};

use crate::installed::VdbEntry;

/// An interned slot name (`None` = unslotted)
///
/// Interned handles are cheap to copy and compare, so the whole conflict check
/// stays handle-based.
pub type Slot = Option<Interned<DefaultInterner>>;

/// A constraint violated by the proposed solution
pub struct Conflict {
    /// The installed package whose dep is violated
    pub installed_cpn: Cpn,
    /// The installed package's slot — needed to re-target it as a
    /// [`portage_atom_pubgrub::PortagePackage`] root dep when repairing a
    /// broken chain (see [`retained_owners`])
    pub slot: Slot,
    /// The installed package's version
    pub installed_ver: Version,
    /// The dep atom that is not satisfied
    pub dep: Dep,
    /// The version the solver chose (which violates the dep)
    pub proposed_ver: Version,
    /// The version the plan installs over this same installed package, when it does
    ///
    /// `Some` means the constraint is stale rather than broken: the package that
    /// carries it is itself being replaced this run, so the dep string evaluated
    /// here belongs to a build that won't survive the plan (lockstep families —
    /// `~`-pinned llvm/clang/lldb, perl virtuals — are entirely this case),
    /// reported apart from real breakage, not dropped.
    pub owner_replaced_by: Option<Version>,
}

/// A package the plan installs or upgrades, carrying its slot so the conflict
/// check can reason per-slot rather than collapsing every slot of a name into
/// one version
pub struct ProposedPkg {
    /// The package name
    pub cpn: Cpn,
    /// Its slot, if any
    pub slot: Slot,
    /// The version the plan installs
    pub version: Version,
}

/// Check all installed packages' dep strings against the proposed solution
///
/// Returns one `Conflict` per violated constraint: no package present after
/// the plan (a proposed package plus every installed package the plan
/// doesn't replace in the same `(cpn, slot)`) satisfies it. Slot-aware:
/// pulling `llvm:21` alongside a retained `llvm:20` doesn't break a consumer
/// pinned to `~llvm:20`, but an in-slot upgrade past a `<` bound does.
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
        let slot = e.slot;
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
        let slot = entry.slot;
        let owner_replaced_by = replaced.get(&(entry.cpn, slot)).map(|v| (*v).clone());
        collect_violations(
            slot,
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

/// Conflicts whose owner is *retained* — genuine breakage, not a stale constraint that will disappear because its owner is itself replaced this run (see [`Conflict::owner_replaced_by`])
///
/// The set a chain-completion repair loop should act on: each names an
/// installed package worth pulling into the plan as an upgrade/rebuild target
/// instead of merely reported.
pub fn retained_owners(conflicts: &[Conflict]) -> impl Iterator<Item = &Conflict> {
    conflicts.iter().filter(|c| c.owner_replaced_by.is_none())
}

/// True if any package present after the plan satisfies `dep` (name, slot and
/// version all considered)
fn dep_satisfied(dep: &Dep, present: &HashMap<Cpn, Vec<(Slot, Cpv)>>) -> bool {
    let Some(cands) = present.get(&dep.cpn) else {
        return false;
    };
    cands.iter().any(|(slot, cpv)| {
        let slot = slot.map(portage_atom::Slot::from_name);
        dep.matches_cpv(cpv, slot.as_ref())
    })
}

fn collect_violations(
    slot: Slot,
    entries: &[DepEntry],
    owner: &VdbEntry,
    owner_replaced_by: Option<&Version>,
    touched: &HashSet<Cpn>,
    present: &HashMap<Cpn, Vec<(Slot, Cpv)>>,
    out: &mut Vec<Conflict>,
) {
    walk_unsatisfied_deps(entries, touched, present, out, &mut |dep| {
        // The name is touched but no present package satisfies the dep.
        // Proposed packages are pushed before retained ones, so the first
        // present entry is the proposed version to blame.
        present
            .get(&dep.cpn)
            .and_then(|c| c.first())
            .map(|(_, cpv)| cpv.version.clone())
            .map(|proposed_ver| Conflict {
                installed_cpn: owner.cpn,
                slot,
                installed_ver: owner.version.clone(),
                dep: dep.clone(),
                proposed_ver,
                owner_replaced_by: owner_replaced_by.cloned(),
            })
    });
}

/// Shared dependency-tree walk for "is this atom satisfied by what's
/// present after the plan" checks: [`find_conflicts`] and
/// [`removal_obstacles`] need identical group semantics — `AllOf`/`^^`/`??`
/// report any unsatisfied branch naming a touched package; `AnyOf` (`||`)
/// only reports when *every* alternative is violated — so this is one
/// function, not two copies that could drift apart
///
/// `on_violation` decides both whether an unsatisfied atom counts (`None`
/// skips it) and what to record (`Some(t)` pushes `t`); it can look up
/// `dep.cpn` in `present` itself when it needs to blame a specific version.
fn walk_unsatisfied_deps<T>(
    entries: &[DepEntry],
    touched: &HashSet<Cpn>,
    present: &HashMap<Cpn, Vec<(Slot, Cpv)>>,
    out: &mut Vec<T>,
    on_violation: &mut impl FnMut(&Dep) -> Option<T>,
) {
    for entry in entries {
        match entry {
            DepEntry::Atom(dep) => {
                if dep.blocker.is_some() || !touched.contains(&dep.cpn) {
                    continue;
                }
                if !dep_satisfied(dep, present)
                    && let Some(t) = on_violation(dep)
                {
                    out.push(t);
                }
            }
            DepEntry::AllOf(children)
            | DepEntry::ExactlyOneOf(children)
            | DepEntry::AtMostOneOf(children) => {
                // Treat ^^/?? like AllOf for reverse-dep advisory: any
                // unsatisfied branch that names a touched package is reported.
                // Coarser than full group semantics, but previously these
                // groups were ignored entirely.
                walk_unsatisfied_deps(children, touched, present, out, on_violation);
            }
            // AnyOf: a conflict only exists if ALL alternatives are violated.
            DepEntry::AnyOf(children) => {
                let branch_violations: Vec<Vec<T>> = children
                    .iter()
                    .map(|child| {
                        let mut v = Vec::new();
                        walk_unsatisfied_deps(
                            std::slice::from_ref(child),
                            touched,
                            present,
                            &mut v,
                            on_violation,
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

/// A retained package that still needs one of the removal candidates a
/// blocker names — the reason that candidate cannot be auto-removed
#[derive(Debug, Clone)]
pub struct RemovalObstacle {
    /// The candidate cpn that is still needed
    pub needed: Cpn,
    /// The candidate's slot
    pub needed_slot: Slot,
    /// The retained installed package whose dep still needs it
    pub needed_by: Cpv,
    /// The dep atom that needs it
    pub dep: Dep,
}

/// Simulate unmerging `candidates` (retained installed packages a blocker names) from the post-plan world [`find_conflicts`] builds
///
/// Returns the obstacles; a candidate with no obstacle is safe to auto-remove —
/// what emerge schedules as an uninstall for an orphaned blocked package.
///
/// Candidates are removed *jointly*, with a fixpoint: a candidate found
/// still-needed is put back into the world and the rest re-checked, so a
/// pair of mutually-dependent removable candidates resolves correctly, and
/// a candidate that turns out needed can still anchor another.
///
/// Does not need to scan `proposed`'s own deps against the candidates: a
/// retained installed package by definition has no solution edge pointing
/// at it — anything depending on its cpn/slot would have pulled that
/// Favor-policy installed package into the solution instead of leaving it
/// retained (see `PortageDependencyProvider`'s solve semantics), so only
/// `installed` packages' deps can possibly still need a removal candidate.
pub fn removal_obstacles(
    candidates: &[(Cpn, Slot)],
    installed: &[VdbEntry],
    proposed: &[ProposedPkg],
) -> Vec<RemovalObstacle> {
    let mut removed: HashSet<(Cpn, Slot)> = candidates.iter().copied().collect();
    let mut final_obstacles = Vec::new();

    loop {
        // Every obstacle `removal_obstacles_once` returns necessarily names
        // a still-`removed` candidate (that's what `touched` restricts the
        // walk to) — each one is confirmed *not* removable and must be
        // excluded from the trial removal set, permanently: put it back
        // into the world (drop it from `removed`) and recheck the rest,
        // since excluding it can reactivate its own dependencies (the
        // fixpoint a mutually-dependent pair of candidates needs).
        let obstacles = removal_obstacles_once(&removed, installed, proposed);
        if obstacles.is_empty() {
            return final_obstacles;
        }
        let newly_excluded: HashSet<(Cpn, Slot)> = obstacles
            .iter()
            .map(|o| (o.needed, o.needed_slot))
            .collect();
        final_obstacles.extend(obstacles);
        removed.retain(|c| !newly_excluded.contains(c));
        if removed.is_empty() {
            return final_obstacles;
        }
    }
}

fn removal_obstacles_once(
    removed: &HashSet<(Cpn, Slot)>,
    installed: &[VdbEntry],
    proposed: &[ProposedPkg],
) -> Vec<RemovalObstacle> {
    let replaced: HashMap<(Cpn, Slot), &Version> = proposed
        .iter()
        .map(|p| ((p.cpn, p.slot), &p.version))
        .collect();

    let mut present: HashMap<Cpn, Vec<(Slot, Cpv)>> = HashMap::new();
    for p in proposed {
        present
            .entry(p.cpn)
            .or_default()
            .push((p.slot, Cpv::new(p.cpn, p.version.clone())));
    }
    for e in installed {
        let slot = e.slot;
        if replaced.contains_key(&(e.cpn, slot)) || removed.contains(&(e.cpn, slot)) {
            continue;
        }
        present
            .entry(e.cpn)
            .or_default()
            .push((slot, Cpv::new(e.cpn, e.version.clone())));
    }

    // Only a removal candidate's own cpn matters here — an installed
    // consumer's dep on anything else is unaffected by this simulation.
    let touched: HashSet<Cpn> = removed.iter().map(|(cpn, _)| *cpn).collect();

    let mut out = Vec::new();
    for owner in installed {
        let slot = owner.slot;
        if replaced.contains_key(&(owner.cpn, slot)) || removed.contains(&(owner.cpn, slot)) {
            // A replaced owner's dep strings die with it; a removed
            // candidate no longer needs to satisfy its own deps.
            continue;
        }
        let active_flags: HashSet<Interned<_>> = owner.active_use.iter().copied().collect();
        let evaluated = DepEntry::evaluate_use(&owner.deps, &active_flags);
        walk_unsatisfied_deps(&evaluated, &touched, &present, &mut out, &mut |dep| {
            let needed_slot = removed
                .iter()
                .find(|(cpn, _)| *cpn == dep.cpn)
                .map(|(_, s)| *s)
                .unwrap_or(None);
            Some(RemovalObstacle {
                needed: dep.cpn,
                needed_slot,
                needed_by: Cpv::new(owner.cpn, owner.version.clone()),
                dep: dep.clone(),
            })
        });
    }
    out
}

/// An installed package's active blocker atoms (USE conditionals resolved against its VDB flags)
///
/// Fed to the solver so `check_blockers` can report a blocker a retained
/// installed owner points at the plan — the owner is never in the solve graph,
/// so its blockers are otherwise invisible.
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

/// Whether any atom anywhere in the (unevaluated) dep tree is a blocker
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
/// [`PortageVersionSet`] (a bare atom with no version op accepts any version)
pub fn dep_to_version_set(dep: &Dep) -> PortageVersionSet {
    match &dep.version {
        None => PortageVersionSet::any(),
        Some(v) => {
            let op = dep.op.unwrap_or(Operator::GreaterOrEqual);
            PortageVersionSet::from_operator(op, dep.glob, v.clone())
        }
    }
}

/// When an auto-removed victim would be unmerged, relative to the merge of the package that blocks it — real portage's `BlockerDepPriority` edge direction: a strong `!!` blocker cannot tolerate any overlap, so its unmerge is ordered *before* the blocking merge; a weak `!` blocker tolerates transient overlap, ordered *after*
///
/// Both are equally auto-removed either way — this only affects scheduling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnmergeOrder {
    /// Weak `!`: unmerge after the blocking package merges
    AfterBlocker,
    /// Strong `!!`: unmerge before the blocking package merges
    BeforeBlocker,
}

/// The classification outcome for one blocker/victim pair
#[derive(Debug, Clone)]
pub enum BlockerVerdict {
    /// Nothing retained still needs the blocked installed package — real emerge would schedule it for unmerge
    ///
    /// Advisory-only until Step 2 threads this into the actual plan.
    WouldUnmerge {
        /// The blocked installed package that would be removed
        cpv: Cpv,
        /// Scheduling relative to the blocking merge (display/Step-2 only)
        order: UnmergeOrder,
    },
    /// The blocked installed package is still needed: a genuine,
    /// unresolvable conflict (for a strong blocker) or an unresolved soft
    /// block (for a weak one) — the caller decides the display severity
    /// from the originating hit's blocker strength
    StillNeeded {
        /// The blocked installed package that cannot safely be removed
        cpv: Cpv,
        /// Why it can't: every remaining reason something still needs it
        obstacles: Vec<RemovalObstacle>,
    },
    /// The blocked package is itself in the plan — both end-states coexist
    /// A hard conflict regardless of blocker strength: final-state
    /// coexistence, not transient overlap, is what weak `!` tolerates
    PlannedCoexistence {
        /// The package coexisting with the blocker in the final plan
        cpv: Cpv,
    },
    /// Both the owner and the victim are already-installed, retained packages
    ///
    /// Real portage suppresses these ("two currently installed packages conflict
    /// with each other... the damage is already done", `depgraph.py`'s
    /// `_validate_blockers`) rather than reporting them as actionable.
    PreExisting {
        /// The other already-installed package in the pre-existing conflict
        cpv: Cpv,
    },
}

/// One blocker hit, classified per victim
#[derive(Debug, Clone)]
pub struct ClassifiedBlocker {
    /// The underlying detected blocker hit
    pub hit: BlockerHit,
    /// One verdict per victim (or, for a reciprocal hit whose victim is a
    /// solution member, per the owner-as-removal-candidate case — see
    /// [`classify_blockers`])
    pub verdicts: Vec<BlockerVerdict>,
}

/// Classify every blocker hit as auto-removable, a genuine conflict, or a
/// case portage itself treats as non-actionable — Step 1 of the blocker
/// Tier-1 auto-unmerge plan: analysis only, no plan mutation
///
/// The removal candidates from every hit are simulated *jointly* in one
/// [`removal_obstacles`] call (not per-hit), so a mutually-dependent pair of
/// candidates named by two different blockers still resolves correctly, and
/// the openresolv-style forward+reciprocal edge pair for one real conflict
/// collapses onto the same candidate.
pub fn classify_blockers(
    hits: &[BlockerHit],
    installed: &[VdbEntry],
    proposed: &[ProposedPkg],
) -> Vec<ClassifiedBlocker> {
    // Every (cpn, slot) that some hit needs a removal verdict for: a victim
    // that's a retained installed package, or — for a reciprocal hit whose
    // victim is a solution member — the owner itself (see the module-level
    // classification table in the loop below).
    let mut candidates: HashSet<(Cpn, Slot)> = HashSet::new();
    for hit in hits {
        for victim in &hit.victims {
            if victim.retained_installed {
                candidates.insert((*victim.package.cpn(), victim.package.slot()));
            } else if hit.owner_installed {
                candidates.insert((*hit.owner.cpn(), hit.owner.slot()));
            }
        }
    }
    let candidates: Vec<(Cpn, Slot)> = candidates.into_iter().collect();
    let obstacles = removal_obstacles(&candidates, installed, proposed);
    let mut obstacles_by_candidate: HashMap<(Cpn, Slot), Vec<RemovalObstacle>> = HashMap::new();
    for o in obstacles {
        obstacles_by_candidate
            .entry((o.needed, o.needed_slot))
            .or_default()
            .push(o);
    }

    let verdict_for_candidate =
        |cpn: Cpn, slot: Slot, cpv: Cpv, order: UnmergeOrder| match obstacles_by_candidate
            .get(&(cpn, slot))
        {
            Some(obs) if !obs.is_empty() => BlockerVerdict::StillNeeded {
                cpv,
                obstacles: obs.clone(),
            },
            _ => BlockerVerdict::WouldUnmerge { cpv, order },
        };

    hits.iter()
        .map(|hit| {
            let order = match hit.atom.blocker {
                Some(Blocker::Strong) => UnmergeOrder::BeforeBlocker,
                _ => UnmergeOrder::AfterBlocker,
            };
            let verdicts = hit
                .victims
                .iter()
                .map(|victim| {
                    let victim_cpv = Cpv::new(*victim.package.cpn(), victim.version.clone());
                    match (hit.owner_installed, victim.retained_installed) {
                        (true, true) => BlockerVerdict::PreExisting { cpv: victim_cpv },
                        (false, false) => BlockerVerdict::PlannedCoexistence { cpv: victim_cpv },
                        // Forward hit: the victim is the removal candidate.
                        (false, true) => verdict_for_candidate(
                            *victim.package.cpn(),
                            victim.package.slot(),
                            victim_cpv,
                            order,
                        ),
                        // Reciprocal hit: the owner is the removal candidate,
                        // not the (immovable, planned) victim.
                        (true, false) => verdict_for_candidate(
                            *hit.owner.cpn(),
                            hit.owner.slot(),
                            Cpv::new(*hit.owner.cpn(), hit.owner_version.clone()),
                            order,
                        ),
                    }
                })
                .collect();
            ClassifiedBlocker {
                hit: hit.clone(),
                verdicts,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use portage_atom_pubgrub::{BlockerVictim, PortagePackage};

    fn pkg(cpn: &str, slot: &str) -> PortagePackage {
        PortagePackage::slotted(
            Cpn::try_new(cpn).expect("test cpn parses"),
            Interned::intern(slot),
        )
    }

    // One owner/atom/victim triple, as `check_blockers_detailed` would emit
    struct HitSpec {
        owner_cpn: &'static str,
        owner_slot: &'static str,
        owner_ver: &'static str,
        owner_installed: bool,
        atom: &'static str,
        victim_cpn: &'static str,
        victim_slot: &'static str,
        victim_ver: &'static str,
        victim_retained_installed: bool,
    }

    fn hit(s: HitSpec) -> BlockerHit {
        BlockerHit {
            owner: pkg(s.owner_cpn, s.owner_slot),
            owner_version: s.owner_ver.parse().expect("test version parses"),
            owner_installed: s.owner_installed,
            atom: Dep::parse(s.atom).expect("test blocker atom parses"),
            victims: vec![BlockerVictim {
                package: pkg(s.victim_cpn, s.victim_slot),
                version: s.victim_ver.parse().expect("test version parses"),
                retained_installed: s.victim_retained_installed,
            }],
        }
    }

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

    // The llvm/clang/lldb shape: a `~`-pinned family moving in lockstep
    //
    // The pin is violated in isolation, but its owner is upgraded in the same run,
    // so it is stale rather than broken.
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

    // The same pin, but its owner stays installed: real breakage
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

    // Replacement is per `(cpn, slot)`: upgrading another slot of the same
    // name leaves this owner in place
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

    // A retained slot still satisfying the pin is not a conflict at all
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

    // `retained_owners` is the repair-loop trigger: a stale (owner-replaced)
    // conflict must not appear, but a genuinely retained owner must, carrying
    // its slot so the loop can re-target it
    #[test]
    fn retained_owners_filters_out_stale_conflicts_and_keeps_the_slot() {
        let installed = vec![
            entry(
                "llvm-core/clang",
                "22",
                "22.1.6",
                &["~llvm-core/llvm-22.1.6"],
            ),
            entry(
                "llvm-core/lldb",
                "22",
                "22.1.6",
                &["~llvm-core/llvm-22.1.6"],
            ),
        ];
        let plan = vec![
            proposed("llvm-core/llvm", "22", "22.1.8"),
            proposed("llvm-core/clang", "22", "22.1.8"),
        ];
        let conflicts = find_conflicts(&installed, &plan);
        assert_eq!(conflicts.len(), 2);
        let retained: Vec<_> = retained_owners(&conflicts).collect();
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].installed_cpn.package.as_str(), "lldb");
        assert_eq!(
            retained[0].slot.map(|s| s.to_string()),
            Some("22".to_owned())
        );
    }

    fn cand(cpn: &str, slot: &str) -> (Cpn, Slot) {
        (
            Cpn::try_new(cpn).expect("test cpn parses"),
            Some(Interned::intern(slot)),
        )
    }

    // An installed package nothing else depends on is safe to auto-remove:
    // no obstacle
    #[test]
    fn orphaned_candidate_has_no_obstacle() {
        let installed = vec![entry("net-dns/openresolv", "0", "3.17.4", &[])];
        let obstacles = removal_obstacles(&[cand("net-dns/openresolv", "0")], &installed, &[]);
        assert!(
            obstacles.is_empty(),
            "orphan should be removable: {obstacles:?}"
        );
    }

    // A retained installed package still depending on the candidate blocks removal
    //
    // This is also the blame-lookup regression case Fable's plan flagged: after
    // the candidate is removed, `present` has *no* entries left at all for its cpn
    // (nothing else provides that name), which is exactly the shape the old inline
    // `present.get(...).and_then(|c| c.first())` blame silently dropped instead of
    // reporting.
    #[test]
    fn still_needed_candidate_reports_an_obstacle() {
        let installed = vec![
            entry("net-dns/openresolv", "0", "3.17.4", &[]),
            entry("app-misc/keeper", "0", "2.0", &["net-dns/openresolv"]),
        ];
        let obstacles = removal_obstacles(&[cand("net-dns/openresolv", "0")], &installed, &[]);
        assert_eq!(obstacles.len(), 1);
        assert_eq!(
            obstacles[0].needed,
            Cpn::try_new("net-dns/openresolv").unwrap()
        );
        assert_eq!(
            obstacles[0].needed_by,
            Cpv::new(
                Cpn::try_new("app-misc/keeper").unwrap(),
                Version::parse("2.0").unwrap()
            )
        );
    }

    // The canonical `virtual/resolvconf` shape: `|| ( net-dns/openresolv
    // sys-apps/systemd )` survives openresolv's removal because the
    // systemd branch (proposed, i.e. being installed) is still present —
    // the `AnyOf` group is not violated just because one alternative is
    // gone, matching real portage's actual auto-removal of an orphaned
    // blocked package in this exact scenario
    #[test]
    fn any_of_group_survives_removal_via_the_other_branch() {
        // entry()'s helper only builds flat Atom entries; a real `||` group
        // needs DepEntry::parse, which understands the full dep-string
        // grammar (groups included), unlike the plain Dep::parse used above.
        let resolvconf = VdbEntry {
            cpn: Cpn::try_new("virtual/resolvconf").expect("test cpn parses"),
            slot: Some(Interned::intern("0")),
            version: "1.0".parse().expect("test version parses"),
            active_use: Vec::new(),
            iuse: Vec::new(),
            deps: DepEntry::parse("|| ( net-dns/openresolv sys-apps/systemd )")
                .expect("test dep string parses"),
        };
        let installed = vec![entry("net-dns/openresolv", "0", "3.17.4", &[]), resolvconf];
        let plan = vec![proposed("sys-apps/systemd", "0", "260.2")];
        let obstacles = removal_obstacles(&[cand("net-dns/openresolv", "0")], &installed, &plan);
        assert!(
            obstacles.is_empty(),
            "AnyOf group must survive via the systemd branch: {obstacles:?}"
        );
    }

    // Joint removal fixpoint, both directions:
    // - A and B are both candidates; B is A's only consumer. Removing them
    //   jointly succeeds (B's dep on A never gets walked, since a removed
    //   candidate's own deps are skipped as an owner).
    // - Adding a retained consumer C of B changes the outcome: B is no
    //   longer safe to remove (needed by C), so B is put back into the
    //   world and rechecked as an owner — which re-activates its own
    //   dependency on A, so A becomes an obstacle too (needed by B).
    #[test]
    fn fixpoint_reintroduces_a_still_needed_candidates_own_dependency() {
        // Both A and B removable jointly: nothing outside the candidate set
        // needs either.
        let obstacles = removal_obstacles(
            &[cand("dev-libs/a", "0"), cand("dev-libs/b", "0")],
            &[
                entry("dev-libs/a", "0", "1.0", &[]),
                entry("dev-libs/b", "0", "1.0", &["dev-libs/a"]),
            ],
            &[],
        );
        assert!(
            obstacles.is_empty(),
            "A and B should be jointly removable: {obstacles:?}"
        );

        // C retained, depends on B: B can no longer be removed, and the
        // fixpoint must notice that B's own dep on A (now reactivated,
        // since B is back in the world) makes A an obstacle too.
        let obstacles = removal_obstacles(
            &[cand("dev-libs/a", "0"), cand("dev-libs/b", "0")],
            &[
                entry("dev-libs/a", "0", "1.0", &[]),
                entry("dev-libs/b", "0", "1.0", &["dev-libs/a"]),
                entry("app-misc/c", "0", "1.0", &["dev-libs/b"]),
            ],
            &[],
        );
        let needed: HashSet<_> = obstacles.iter().map(|o| o.needed).collect();
        assert!(
            needed.contains(&Cpn::try_new("dev-libs/b").unwrap()),
            "B should be reported as needed (by C): {obstacles:?}"
        );
        assert!(
            needed.contains(&Cpn::try_new("dev-libs/a").unwrap()),
            "A should be reported as needed (by B, reactivated by the fixpoint): {obstacles:?}"
        );
    }

    fn single_verdict(classified: &[ClassifiedBlocker]) -> &BlockerVerdict {
        assert_eq!(classified.len(), 1);
        assert_eq!(classified[0].verdicts.len(), 1);
        &classified[0].verdicts[0]
    }

    // Forward hit (owner is a planned/solution package), weak blocker,
    // orphaned installed victim: emerge would auto-unmerge it, ordered
    // after the blocking merge
    #[test]
    fn forward_weak_orphan_would_unmerge_after() {
        let h = hit(HitSpec {
            owner_cpn: "sys-apps/systemd",
            owner_slot: "0",
            owner_ver: "260.2",
            owner_installed: false,
            atom: "!net-dns/openresolv",
            victim_cpn: "net-dns/openresolv",
            victim_slot: "0",
            victim_ver: "3.17.4",
            victim_retained_installed: true,
        });
        let installed = vec![entry("net-dns/openresolv", "0", "3.17.4", &[])];
        let classified = classify_blockers(&[h], &installed, &[]);
        assert!(matches!(
            single_verdict(&classified),
            BlockerVerdict::WouldUnmerge {
                order: UnmergeOrder::AfterBlocker,
                ..
            }
        ));
    }

    // Same shape, but a retained consumer still needs the victim: a
    // genuine (soft, for a weak blocker) conflict, not auto-removable
    #[test]
    fn forward_weak_still_needed() {
        let h = hit(HitSpec {
            owner_cpn: "sys-apps/systemd",
            owner_slot: "0",
            owner_ver: "260.2",
            owner_installed: false,
            atom: "!net-dns/openresolv",
            victim_cpn: "net-dns/openresolv",
            victim_slot: "0",
            victim_ver: "3.17.4",
            victim_retained_installed: true,
        });
        let installed = vec![
            entry("net-dns/openresolv", "0", "3.17.4", &[]),
            entry("app-misc/keeper", "0", "2.0", &["net-dns/openresolv"]),
        ];
        let classified = classify_blockers(&[h], &installed, &[]);
        assert!(matches!(
            single_verdict(&classified),
            BlockerVerdict::StillNeeded { .. }
        ));
    }

    // A strong `!!` blocker orders the auto-unmerge *before* the blocking
    // merge instead of after — the real weak/strong asymmetry (both are
    // equally auto-removed; only the scheduling edge differs)
    #[test]
    fn strong_blocker_orders_unmerge_before() {
        let h = hit(HitSpec {
            owner_cpn: "sys-apps/systemd",
            owner_slot: "0",
            owner_ver: "260.2",
            owner_installed: false,
            atom: "!!net-dns/openresolv",
            victim_cpn: "net-dns/openresolv",
            victim_slot: "0",
            victim_ver: "3.17.4",
            victim_retained_installed: true,
        });
        let installed = vec![entry("net-dns/openresolv", "0", "3.17.4", &[])];
        let classified = classify_blockers(&[h], &installed, &[]);
        assert!(matches!(
            single_verdict(&classified),
            BlockerVerdict::WouldUnmerge {
                order: UnmergeOrder::BeforeBlocker,
                ..
            }
        ));
    }

    // Reciprocal hit: an installed owner's blocker fires against a planned
    // (solution-member) victim — the *owner*, not the immovable victim, is
    // the removal candidate (mirrors installed openresolv's `!systemd`
    // against a planned systemd)
    #[test]
    fn reciprocal_hit_makes_the_owner_the_candidate() {
        let h = hit(HitSpec {
            owner_cpn: "net-dns/openresolv",
            owner_slot: "0",
            owner_ver: "3.17.4",
            owner_installed: true,
            atom: "!sys-apps/systemd",
            victim_cpn: "sys-apps/systemd",
            victim_slot: "0",
            victim_ver: "260.2",
            victim_retained_installed: false,
        });
        let installed = vec![entry("net-dns/openresolv", "0", "3.17.4", &[])];
        let classified = classify_blockers(&[h], &installed, &[]);
        match single_verdict(&classified) {
            BlockerVerdict::WouldUnmerge { cpv, .. } => {
                assert_eq!(cpv.cpn, Cpn::try_new("net-dns/openresolv").unwrap());
            }
            other => panic!("expected WouldUnmerge for the owner, got {other:?}"),
        }
    }

    // Both owner and victim are in the final plan: a hard conflict
    // regardless of blocker strength (weak `!` tolerates transient
    // overlap, not permanent coexistence)
    #[test]
    fn planned_coexistence_is_a_hard_conflict() {
        let h = hit(HitSpec {
            owner_cpn: "app-misc/a",
            owner_slot: "0",
            owner_ver: "1.0",
            owner_installed: false,
            atom: "!app-misc/b",
            victim_cpn: "app-misc/b",
            victim_slot: "0",
            victim_ver: "1.0",
            victim_retained_installed: false,
        });
        let classified = classify_blockers(&[h], &[], &[]);
        assert!(matches!(
            single_verdict(&classified),
            BlockerVerdict::PlannedCoexistence { .. }
        ));
    }

    // Owner and victim are both already-installed, retained packages: real
    // portage suppresses this as non-actionable ("damage already done")
    #[test]
    fn pre_existing_conflict_between_two_installed_packages() {
        let h = hit(HitSpec {
            owner_cpn: "app-misc/a",
            owner_slot: "0",
            owner_ver: "1.0",
            owner_installed: true,
            atom: "!app-misc/b",
            victim_cpn: "app-misc/b",
            victim_slot: "0",
            victim_ver: "1.0",
            victim_retained_installed: true,
        });
        let installed = vec![
            entry("app-misc/a", "0", "1.0", &[]),
            entry("app-misc/b", "0", "1.0", &[]),
        ];
        let classified = classify_blockers(&[h], &installed, &[]);
        assert!(matches!(
            single_verdict(&classified),
            BlockerVerdict::PreExisting { .. }
        ));
    }

    // The canonical `virtual/resolvconf` scenario end-to-end through
    // classify_blockers: both the forward hit (systemd blocks installed
    // openresolv) and the reciprocal hit (installed openresolv blocks
    // planned systemd) collapse onto the *same* removal candidate
    // (openresolv) via the joint simulation, and both get a consistent
    // verdict
    #[test]
    fn systemd_resolvconf_canonical_case_both_edges_agree() {
        let forward = hit(HitSpec {
            owner_cpn: "sys-apps/systemd",
            owner_slot: "0",
            owner_ver: "260.2",
            owner_installed: false,
            atom: "!net-dns/openresolv",
            victim_cpn: "net-dns/openresolv",
            victim_slot: "0",
            victim_ver: "3.17.4",
            victim_retained_installed: true,
        });
        let reciprocal = hit(HitSpec {
            owner_cpn: "net-dns/openresolv",
            owner_slot: "0",
            owner_ver: "3.17.4",
            owner_installed: true,
            atom: "!sys-apps/systemd",
            victim_cpn: "sys-apps/systemd",
            victim_slot: "0",
            victim_ver: "260.2",
            victim_retained_installed: false,
        });
        let installed = vec![entry("net-dns/openresolv", "0", "3.17.4", &[])];
        let classified = classify_blockers(&[forward, reciprocal], &installed, &[]);
        assert_eq!(classified.len(), 2);
        for c in &classified {
            match &c.verdicts[..] {
                [BlockerVerdict::WouldUnmerge { cpv, .. }] => {
                    assert_eq!(cpv.cpn, Cpn::try_new("net-dns/openresolv").unwrap());
                }
                other => panic!("expected WouldUnmerge for openresolv, got {other:?}"),
            }
        }
    }
}
