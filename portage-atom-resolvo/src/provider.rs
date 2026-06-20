//! Bridge between portage-atom and resolvo's [`DependencyProvider`] trait.
//!
//! [`PortageDependencyProvider`] pre-populates a [`PortagePool`] from a
//! [`PackageRepository`] and implements both [`Interner`] and
//! [`DependencyProvider`] so that [`resolvo::Solver`] can resolve
//! Portage-style dependencies.

use std::collections::{HashMap, HashSet};
use std::fmt;

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{
    Blocker, Cpn, Cpv, Dep, DepEntry, Operator, SlotDep, SlotOperator, UseDepKind, Version,
};
use resolvo::{
    Candidates, Condition, ConditionId, ConditionalRequirement, Dependencies,
    HintDependenciesAvailable, KnownDependencies, NameId, Requirement, SolvableId, SolverCache,
    StringId, VersionSetId, VersionSetUnionId,
};

use crate::pool::{
    DepClass, DepEdge, InstalledPolicy, InstalledSet, PackageDeps, PackageMetadata, PackageName,
    PortagePool, UseConfig, VersionConstraint,
};
use crate::repository::PackageRepository;
use crate::version_match::version_matches;

/// Internal data for a solver-decided USE flag.
///
/// Each solver-decided flag is modelled as a complementary pair of virtual
/// solvables (`virtual/USE_<flag>` and `virtual/NotUSE_<flag>`) with mutual
/// exclusion.  Packages that reference the flag get a
/// `|| ( NotUSE_<flag> USE_<flag> )` requirement so the solver is forced to
/// pick exactly one.
struct FlagVirtuals {
    /// Condition true when the flag is ON (`virtual/USE_<flag>` selected).
    on_condition: ConditionId,
    /// Condition true when the flag is OFF (`virtual/NotUSE_<flag>` selected).
    off_condition: ConditionId,
    /// Pre-computed union `|| ( NotUSE_<flag> USE_<flag> )` — injected into
    /// every solvable that references the flag.  `NotUSE` is listed first
    /// to bias the solver toward flag-off (minimal deps).
    choice_union: VersionSetUnionId,
}

/// Mutable state threaded through dependency tree conversion.
struct ConvertContext<'a> {
    pool: &'a mut PortagePool,
    cpn_slots: &'a mut HashMap<Cpn, Vec<NameId>>,
    blocker_types: &'a mut HashMap<VersionSetId, Blocker>,
    rebuild_triggers: &'a mut HashSet<VersionSetId>,
    flag_virtuals: &'a HashMap<Interned<DefaultInterner>, FlagVirtuals>,
    use_config: &'a UseConfig,
    encountered_flags: HashSet<Interned<DefaultInterner>>,
    candidates: &'a mut HashMap<NameId, Vec<SolvableId>>,
    dep_map: &'a mut HashMap<SolvableId, KnownDependencies>,
    xof_counter: &'a mut usize,
}

/// Dependency provider bridging portage-atom types to the resolvo solver.
///
/// Construction eagerly walks every package in the repository, interns all
/// solvables and dependency trees into the pool, and pre-computes
/// [`KnownDependencies`] for each solvable. The resulting provider is
/// read-only and suitable for passing to [`resolvo::Solver::new`].
pub struct PortageDependencyProvider {
    pub(crate) pool: PortagePool,
    /// Pre-computed candidates per name.
    candidates: HashMap<NameId, Vec<SolvableId>>,
    /// Pre-computed dependencies per solvable.
    dependencies: HashMap<SolvableId, KnownDependencies>,
    /// Map from unversioned CPN to all slotted NameIds known for that CPN.
    cpn_slots: HashMap<Cpn, Vec<NameId>>,
    /// Blocker type for each version set that came from a blocker dep.
    /// Only populated for `constrains` entries; absent means not a blocker.
    blocker_types: HashMap<VersionSetId, Blocker>,
    /// Version sets that carry a `:=` slot operator (rebuild trigger).
    /// When the dependency's slot or sub-slot changes, the dependent
    /// package must be rebuilt.
    rebuild_triggers: HashSet<VersionSetId>,
    flag_virtuals: HashMap<Interned<DefaultInterner>, FlagVirtuals>,
    use_config: UseConfig,
    /// SolvableId to favor per NameId (installed, soft preference).
    favored: HashMap<NameId, SolvableId>,
    /// SolvableId to lock per NameId (installed, hard constraint).
    locked: HashMap<NameId, SolvableId>,
}

impl PortageDependencyProvider {
    /// Build a provider from a repository and a [`UseConfig`].
    ///
    /// Flags listed in [`UseConfig::enabled`] / [`UseConfig::disabled`] are
    /// eagerly evaluated at construction time (same as the old behaviour).
    /// Flags listed in [`UseConfig::solver_decided`] create virtual
    /// `virtual/USE_<flag>` solvables and resolvo conditions so the SAT
    /// solver can decide whether to activate them.
    pub fn new(repo: &dyn PackageRepository, use_config: &UseConfig) -> Self {
        Self::with_installed(repo, use_config, &InstalledSet::default())
    }

    /// Build a provider from a repository, a [`UseConfig`], and an
    /// [`InstalledSet`] describing packages already installed on the system.
    ///
    /// Installed packages found in the repository are matched by CPV and
    /// marked as [`InstalledPolicy::Favored`] or [`InstalledPolicy::Locked`].
    /// Installed packages *not* in the repository are injected as extra
    /// solvables so the solver can reference them.
    pub fn with_installed(
        repo: &dyn PackageRepository,
        use_config: &UseConfig,
        installed: &InstalledSet,
    ) -> Self {
        let mut pool = PortagePool::new();
        let mut candidates: HashMap<NameId, Vec<SolvableId>> = HashMap::new();
        let mut dep_map: HashMap<SolvableId, KnownDependencies> = HashMap::new();
        let mut cpn_slots: HashMap<Cpn, Vec<NameId>> = HashMap::new();
        let mut blocker_types: HashMap<VersionSetId, Blocker> = HashMap::new();
        let mut rebuild_triggers: HashSet<VersionSetId> = HashSet::new();
        let mut favored: HashMap<NameId, SolvableId> = HashMap::new();
        let mut locked: HashMap<NameId, SolvableId> = HashMap::new();

        // Build an index of installed packages by CPV.
        let mut installed_index: HashMap<Cpv, InstalledPolicy> = HashMap::new();
        for (meta, policy) in &installed.packages {
            installed_index.insert(meta.cpv.clone(), *policy);
        }

        // Phase 1: intern all real solvables.
        let mut solvable_meta: Vec<(SolvableId, PackageDeps)> = Vec::new();
        let mut found_installed: HashSet<Cpv> = HashSet::new();

        for cpn in repo.all_packages() {
            for meta in repo.versions_for(&cpn) {
                let pkg_name = PackageName {
                    cpn: meta.cpv.cpn,
                    slot: meta.slot,
                };
                let name_id = pool.intern_name(pkg_name);

                // Track all slotted NameIds per CPN.
                let slot_list = cpn_slots.entry(meta.cpv.cpn).or_default();
                if !slot_list.contains(&name_id) {
                    slot_list.push(name_id);
                }

                let pkg_deps = meta.dependencies.clone();
                let sid = pool.intern_solvable(name_id, meta.clone());
                candidates.entry(name_id).or_default().push(sid);
                solvable_meta.push((sid, pkg_deps));

                // Check if this solvable matches an installed package.
                if let Some(&policy) = installed_index.get(&meta.cpv) {
                    found_installed.insert(meta.cpv.clone());
                    match policy {
                        InstalledPolicy::Favored => {
                            favored.insert(name_id, sid);
                        }
                        InstalledPolicy::Locked => {
                            locked.insert(name_id, sid);
                        }
                    }
                }
            }
        }

        // Inject installed packages not found in the repository.
        for (meta, policy) in &installed.packages {
            if found_installed.contains(&meta.cpv) {
                continue;
            }
            let pkg_name = PackageName {
                cpn: meta.cpv.cpn,
                slot: meta.slot,
            };
            let name_id = pool.intern_name(pkg_name);

            let slot_list = cpn_slots.entry(meta.cpv.cpn).or_default();
            if !slot_list.contains(&name_id) {
                slot_list.push(name_id);
            }

            let pkg_deps = meta.dependencies.clone();
            let sid = pool.intern_solvable(name_id, meta.clone());
            candidates.entry(name_id).or_default().push(sid);
            solvable_meta.push((sid, pkg_deps));

            match policy {
                InstalledPolicy::Favored => {
                    favored.insert(name_id, sid);
                }
                InstalledPolicy::Locked => {
                    locked.insert(name_id, sid);
                }
            }
        }

        // Phase 1.5: create virtual solvables for solver-decided USE flags.
        //
        // For each flag we create two virtual packages that mutually exclude
        // each other.  Selecting `virtual/USE_<flag>` means the flag is ON;
        // selecting `virtual/NotUSE_<flag>` means the flag is OFF.
        let mut flag_virtuals: HashMap<Interned<DefaultInterner>, FlagVirtuals> = HashMap::new();
        let version_zero = Version::parse("0").unwrap();

        for flag in &use_config.solver_decided {
            // --- ON virtual: virtual/USE_<flag>-1.0 ---
            let on_cpn = Cpn::new("virtual", format!("USE_{flag}"));
            let on_name = PackageName {
                cpn: on_cpn,
                slot: None,
            };
            let on_name_id = pool.intern_name(on_name);
            cpn_slots.entry(on_cpn).or_default().push(on_name_id);

            let on_meta = PackageMetadata {
                cpv: Cpv::parse(&format!("virtual/USE_{flag}-1.0")).unwrap(),
                slot: None,
                subslot: None,
                iuse: vec![],
                use_flags: HashSet::new(),
                repo: None,
                dependencies: PackageDeps::default(),
            };
            let on_sid = pool.intern_solvable(on_name_id, on_meta);
            candidates.entry(on_name_id).or_default().push(on_sid);

            let on_constraint = VersionConstraint {
                cpn: on_cpn,
                operator: Operator::GreaterOrEqual,
                version: version_zero.clone(),
                glob: false,
                slot: None,
                subslot: None,
                repo: None,
                use_constraints: vec![],
                inverted: false,
            };
            let on_vs = pool.intern_version_set(on_name_id, on_constraint);
            let on_cond = pool.intern_condition(Condition::Requirement(on_vs));

            // --- OFF virtual: virtual/NotUSE_<flag>-1.0 ---
            let off_cpn = Cpn::new("virtual", format!("NotUSE_{flag}"));
            let off_name = PackageName {
                cpn: off_cpn,
                slot: None,
            };
            let off_name_id = pool.intern_name(off_name);
            cpn_slots.entry(off_cpn).or_default().push(off_name_id);

            let off_meta = PackageMetadata {
                cpv: Cpv::parse(&format!("virtual/NotUSE_{flag}-1.0")).unwrap(),
                slot: None,
                subslot: None,
                iuse: vec![],
                use_flags: HashSet::new(),
                repo: None,
                dependencies: PackageDeps::default(),
            };
            let off_sid = pool.intern_solvable(off_name_id, off_meta);
            candidates.entry(off_name_id).or_default().push(off_sid);

            let off_constraint = VersionConstraint {
                cpn: off_cpn,
                operator: Operator::GreaterOrEqual,
                version: version_zero.clone(),
                glob: false,
                slot: None,
                subslot: None,
                repo: None,
                use_constraints: vec![],
                inverted: false,
            };
            let off_vs = pool.intern_version_set(off_name_id, off_constraint);
            let off_cond = pool.intern_condition(Condition::Requirement(off_vs));

            // --- Mutual exclusion: each virtual blocks the other ---
            dep_map.insert(
                on_sid,
                KnownDependencies {
                    requirements: vec![],
                    constrains: vec![off_vs],
                },
            );
            dep_map.insert(
                off_sid,
                KnownDependencies {
                    requirements: vec![],
                    constrains: vec![on_vs],
                },
            );

            // --- Choice union: || ( NotUSE_<flag> USE_<flag> ) ---
            // NotUSE listed first to bias the solver toward flag-off.
            let choice_union = pool.intern_version_set_union(vec![off_vs, on_vs]);

            flag_virtuals.insert(
                *flag,
                FlagVirtuals {
                    on_condition: on_cond,
                    off_condition: off_cond,
                    choice_union,
                },
            );
        }

        // Phase 2: convert dependency trees into resolvo requirements.
        let mut xof_counter: usize = 0;
        for (sid, pkg_deps) in solvable_meta {
            let mut requirements = Vec::new();
            let mut constrains = Vec::new();

            let mut ctx = ConvertContext {
                pool: &mut pool,
                cpn_slots: &mut cpn_slots,
                blocker_types: &mut blocker_types,
                rebuild_triggers: &mut rebuild_triggers,
                flag_virtuals: &flag_virtuals,
                use_config,
                encountered_flags: HashSet::new(),
                candidates: &mut candidates,
                dep_map: &mut dep_map,
                xof_counter: &mut xof_counter,
            };
            for (_class, entries) in pkg_deps.iter_classes() {
                Self::convert_deps(entries, &mut ctx, &mut requirements, &mut constrains);
            }

            // Inject choice requirements for each solver-decided flag
            // referenced by this solvable's dependency tree.
            for flag in &ctx.encountered_flags {
                if let Some(fv) = ctx.flag_virtuals.get(flag) {
                    requirements.push(ConditionalRequirement {
                        condition: None,
                        requirement: Requirement::Union(fv.choice_union),
                    });
                }
            }

            ctx.dep_map.insert(
                sid,
                KnownDependencies {
                    requirements,
                    constrains,
                },
            );
        }

        Self {
            pool,
            candidates,
            dependencies: dep_map,
            cpn_slots,
            blocker_types,
            rebuild_triggers,
            flag_virtuals,
            use_config: use_config.clone(),
            favored,
            locked,
        }
    }

    /// Recursively convert a slice of [`DepEntry`]s into resolvo requirements
    /// and constrains.
    fn convert_deps(
        entries: &[DepEntry],
        ctx: &mut ConvertContext<'_>,
        requirements: &mut Vec<ConditionalRequirement>,
        constrains: &mut Vec<VersionSetId>,
    ) {
        for entry in entries {
            match entry {
                DepEntry::Atom(dep) => {
                    Self::convert_atom(dep, ctx, requirements, constrains);
                }
                DepEntry::UseConditional {
                    flag,
                    negate,
                    children,
                } => {
                    if let Some(fv) = ctx.flag_virtuals.get(flag) {
                        // Solver-decided flag — attach the appropriate condition.
                        ctx.encountered_flags.insert(*flag);
                        let cond_id = if *negate {
                            fv.off_condition
                        } else {
                            fv.on_condition
                        };
                        let mut cond_reqs = Vec::new();
                        Self::convert_deps(children, ctx, &mut cond_reqs, constrains);
                        for mut req in cond_reqs {
                            req.condition = Some(cond_id);
                            requirements.push(req);
                        }
                    } else {
                        // Eager evaluation (enabled/disabled).
                        let flag_active = ctx.use_config.enabled.contains(flag);
                        let include = if *negate { !flag_active } else { flag_active };
                        if include {
                            Self::convert_deps(children, ctx, requirements, constrains);
                        }
                    }
                }
                DepEntry::AnyOf(alternatives) => {
                    Self::convert_any_of(alternatives, ctx, requirements, constrains);
                }
                DepEntry::ExactlyOneOf(alternatives) => {
                    Self::convert_one_of_group(alternatives, false, ctx, requirements, constrains);
                }
                DepEntry::AtMostOneOf(alternatives) => {
                    Self::convert_one_of_group(alternatives, true, ctx, requirements, constrains);
                }
                DepEntry::AllOf(children) => {
                    Self::convert_deps(children, ctx, requirements, constrains);
                }
            }
        }
    }

    /// Convert a `^^ ( )` or `?? ( )` group into virtual choice solvables
    /// with pairwise mutual exclusion.
    ///
    /// Each immediate child of the group becomes a *virtual choice solvable*.
    /// The solver must select exactly one choice (for `^^`) or at most one
    /// (for `??`).  This is enforced by:
    ///
    /// 1. Each choice solvable blocks every other choice via `constrains`.
    /// 2. The dependent package requires `Union(all choices)` — the solver
    ///    picks one.
    ///
    /// For `??`, an additional "none" choice with no requirements is added
    /// (listed first in the union to bias the solver toward no selection)
    /// so the solver can satisfy the union without installing any real
    /// alternative.
    ///
    /// Each child's requirements are produced by recursively calling
    /// [`convert_deps`] on a single-element slice, so `Atom`,
    /// `UseConditional`, nested `|| ( )`, and even nested `^^ ( )` /
    /// `?? ( )` are all handled.  Blockers inside children become
    /// constrains on the virtual solvable.
    fn convert_one_of_group(
        alternatives: &[DepEntry],
        allow_none: bool,
        ctx: &mut ConvertContext<'_>,
        requirements: &mut Vec<ConditionalRequirement>,
        _parent_constrains: &mut Vec<VersionSetId>,
    ) {
        let group_id = *ctx.xof_counter;
        *ctx.xof_counter += 1;

        let version_zero = Version::parse("0").unwrap();

        // (solvable_id, version_set_id, child_requirements, child_constrains)
        let mut choices: Vec<(
            SolvableId,
            VersionSetId,
            Vec<ConditionalRequirement>,
            Vec<VersionSetId>,
        )> = Vec::new();

        // For ??, create a "none" virtual first to bias the solver toward
        // not selecting any alternative (same pattern as NotUSE_ listed
        // first for solver-decided flags).
        if allow_none {
            let cpn = Cpn::new("virtual", format!("xof_{group_id}_none"));
            let pkg_name = PackageName { cpn, slot: None };
            let name_id = ctx.pool.intern_name(pkg_name);
            ctx.cpn_slots.entry(cpn).or_default().push(name_id);

            let meta = PackageMetadata {
                cpv: Cpv::parse(&format!("virtual/xof_{group_id}_none-1.0")).unwrap(),
                slot: None,
                subslot: None,
                iuse: vec![],
                use_flags: HashSet::new(),
                repo: None,
                dependencies: PackageDeps::default(),
            };
            let sid = ctx.pool.intern_solvable(name_id, meta);
            ctx.candidates.entry(name_id).or_default().push(sid);

            let constraint = VersionConstraint {
                cpn,
                operator: Operator::GreaterOrEqual,
                version: version_zero.clone(),
                glob: false,
                slot: None,
                subslot: None,
                repo: None,
                use_constraints: vec![],
                inverted: false,
            };
            let vs_id = ctx.pool.intern_version_set(name_id, constraint);

            choices.push((sid, vs_id, Vec::new(), Vec::new()));
        }

        // Create one virtual choice solvable per real alternative.
        for (i, alt) in alternatives.iter().enumerate() {
            let cpn = Cpn::new("virtual", format!("xof_{group_id}_{i}"));
            let pkg_name = PackageName { cpn, slot: None };
            let name_id = ctx.pool.intern_name(pkg_name);
            ctx.cpn_slots.entry(cpn).or_default().push(name_id);

            let meta = PackageMetadata {
                cpv: Cpv::parse(&format!("virtual/xof_{group_id}_{i}-1.0")).unwrap(),
                slot: None,
                subslot: None,
                iuse: vec![],
                use_flags: HashSet::new(),
                repo: None,
                dependencies: PackageDeps::default(),
            };
            let sid = ctx.pool.intern_solvable(name_id, meta);
            ctx.candidates.entry(name_id).or_default().push(sid);

            let constraint = VersionConstraint {
                cpn,
                operator: Operator::GreaterOrEqual,
                version: version_zero.clone(),
                glob: false,
                slot: None,
                subslot: None,
                repo: None,
                use_constraints: vec![],
                inverted: false,
            };
            let vs_id = ctx.pool.intern_version_set(name_id, constraint);

            // Convert the child entry's deps by recursing into convert_deps.
            let mut child_reqs = Vec::new();
            let mut child_constrains = Vec::new();
            Self::convert_deps(
                std::slice::from_ref(alt),
                ctx,
                &mut child_reqs,
                &mut child_constrains,
            );

            choices.push((sid, vs_id, child_reqs, child_constrains));
        }

        // Wire pairwise exclusion: each choice blocks every other choice.
        let all_vs_ids: Vec<VersionSetId> = choices.iter().map(|(_, vs_id, _, _)| *vs_id).collect();

        for (i, (sid, _, child_reqs, child_constrains)) in choices.into_iter().enumerate() {
            let mut constrains_for_choice = child_constrains;
            for (j, &vs_id) in all_vs_ids.iter().enumerate() {
                if i != j {
                    constrains_for_choice.push(vs_id);
                }
            }
            ctx.dep_map.insert(
                sid,
                KnownDependencies {
                    requirements: child_reqs,
                    constrains: constrains_for_choice,
                },
            );
        }

        // Push the union requirement to the parent.
        if all_vs_ids.len() == 1 {
            requirements.push(ConditionalRequirement {
                condition: None,
                requirement: Requirement::Single(all_vs_ids[0]),
            });
        } else {
            let union_id = ctx.pool.intern_version_set_union(all_vs_ids);
            requirements.push(ConditionalRequirement {
                condition: None,
                requirement: Requirement::Union(union_id),
            });
        }
    }

    /// Convert a single dependency atom into requirements/constrains.
    ///
    /// When a dep specifies a slot, the requirement targets a single slotted
    /// [`NameId`]. When no slot is specified, the requirement becomes a union
    /// over all known slotted names for that CPN, so the solver can pick any
    /// slot.
    fn convert_atom(
        dep: &Dep,
        ctx: &mut ConvertContext<'_>,
        requirements: &mut Vec<ConditionalRequirement>,
        constrains: &mut Vec<VersionSetId>,
    ) {
        let (slot, subslot) = extract_slot(dep);
        let repo = dep.repo;
        let use_constraints = resolve_use_deps(dep, ctx.use_config);
        let blocker = dep.blocker;
        let is_blocker = blocker.is_some();
        let is_rebuild_trigger = has_slot_equal_op(dep);
        let (op, version) = dep_op_version(dep);

        // Helper: push a version set as a blocker constrain, recording its type.
        let mut push_blocker = |vs_id: VersionSetId| {
            constrains.push(vs_id);
            if let Some(b) = blocker {
                ctx.blocker_types.insert(vs_id, b);
            }
        };

        // Helper: record a version set as a rebuild trigger (`:=`).
        let mut mark_trigger = |vs_id: VersionSetId| {
            if is_rebuild_trigger {
                ctx.rebuild_triggers.insert(vs_id);
            }
        };

        if let Some(ref slot_val) = slot {
            // Slotted dep — targets a single NameId.
            let pkg_name = PackageName {
                cpn: dep.cpn,
                slot: Some(*slot_val),
            };
            let name_id = ctx.pool.intern_name(pkg_name);
            let constraint = VersionConstraint {
                cpn: dep.cpn,
                operator: op,
                version,
                glob: dep.glob,
                slot: Some(*slot_val),
                subslot,
                repo,
                use_constraints: use_constraints.clone(),
                inverted: is_blocker,
            };
            let vs_id = ctx.pool.intern_version_set(name_id, constraint);
            mark_trigger(vs_id);

            if is_blocker {
                push_blocker(vs_id);
            } else {
                requirements.push(ConditionalRequirement {
                    condition: None,
                    requirement: Requirement::Single(vs_id),
                });
            }
        } else {
            // Unslotted dep — union over all known slots.
            let slot_names = ctx.cpn_slots.get(&dep.cpn);

            match slot_names {
                Some(names) if names.len() == 1 => {
                    let name_id = names[0];
                    let constraint = VersionConstraint {
                        cpn: dep.cpn,
                        operator: op,
                        version,
                        glob: dep.glob,
                        slot: None,
                        subslot: None,
                        repo,
                        use_constraints: use_constraints.clone(),
                        inverted: is_blocker,
                    };
                    let vs_id = ctx.pool.intern_version_set(name_id, constraint);
                    mark_trigger(vs_id);

                    if is_blocker {
                        push_blocker(vs_id);
                    } else {
                        requirements.push(ConditionalRequirement {
                            condition: None,
                            requirement: Requirement::Single(vs_id),
                        });
                    }
                }
                Some(names) => {
                    let vs_ids: Vec<VersionSetId> = names
                        .iter()
                        .map(|&name_id| {
                            let constraint = VersionConstraint {
                                cpn: dep.cpn,
                                operator: op,
                                version: version.clone(),
                                glob: dep.glob,
                                slot: None,
                                subslot: None,
                                repo,
                                use_constraints: use_constraints.clone(),
                                inverted: is_blocker,
                            };
                            ctx.pool.intern_version_set(name_id, constraint)
                        })
                        .collect();

                    for &vs_id in &vs_ids {
                        mark_trigger(vs_id);
                    }

                    if is_blocker {
                        for vs_id in vs_ids {
                            push_blocker(vs_id);
                        }
                    } else if vs_ids.len() == 1 {
                        requirements.push(ConditionalRequirement {
                            condition: None,
                            requirement: Requirement::Single(vs_ids[0]),
                        });
                    } else {
                        let union_id = ctx.pool.intern_version_set_union(vs_ids);
                        requirements.push(ConditionalRequirement {
                            condition: None,
                            requirement: Requirement::Union(union_id),
                        });
                    }
                }
                None => {
                    // Package not in the repository — skip the requirement.
                    // The solver cannot satisfy a dep on a missing package,
                    // so we drop it (matching the pubgrub provider's approach).
                }
            }
        }
    }

    /// Convert an `|| ( ... )` group into a `Requirement::Union`.
    fn convert_any_of(
        alternatives: &[DepEntry],
        ctx: &mut ConvertContext<'_>,
        requirements: &mut Vec<ConditionalRequirement>,
        constrains: &mut Vec<VersionSetId>,
    ) {
        let mut vs_ids = Vec::new();

        for alt in alternatives {
            match alt {
                DepEntry::Atom(dep) => {
                    if dep.blocker.is_some() {
                        Self::convert_atom(dep, ctx, &mut Vec::new(), constrains);
                        continue;
                    }

                    let (slot, subslot) = extract_slot(dep);
                    let (op, version) = dep_op_version(dep);
                    let use_constraints = resolve_use_deps(dep, ctx.use_config);

                    if let Some(ref slot_val) = slot {
                        let pkg_name = PackageName {
                            cpn: dep.cpn,
                            slot: Some(*slot_val),
                        };
                        let name_id = ctx.pool.intern_name(pkg_name);
                        let constraint = VersionConstraint {
                            cpn: dep.cpn,
                            operator: op,
                            version,
                            glob: dep.glob,
                            slot: Some(*slot_val),
                            subslot,
                            repo: dep.repo,
                            use_constraints,
                            inverted: false,
                        };
                        vs_ids.push(ctx.pool.intern_version_set(name_id, constraint));
                    } else {
                        // Unslotted — add one VS per known slot.
                        if let Some(names) = ctx.cpn_slots.get(&dep.cpn) {
                            for &name_id in names {
                                let constraint = VersionConstraint {
                                    cpn: dep.cpn,
                                    operator: op,
                                    version: version.clone(),
                                    glob: dep.glob,
                                    slot: None,
                                    subslot: None,
                                    repo: dep.repo,
                                    use_constraints: use_constraints.clone(),
                                    inverted: false,
                                };
                                vs_ids.push(ctx.pool.intern_version_set(name_id, constraint));
                            }
                        } else {
                            // Package not in the repository — skip.
                        }
                    }
                }
                DepEntry::UseConditional {
                    flag,
                    negate,
                    children,
                } => {
                    if let Some(fv) = ctx.flag_virtuals.get(flag) {
                        // Solver-decided flag inside || ( ).
                        ctx.encountered_flags.insert(*flag);
                        let cond_id = if *negate {
                            fv.off_condition
                        } else {
                            fv.on_condition
                        };
                        let mut cond_reqs = Vec::new();
                        Self::convert_any_of(children, ctx, &mut cond_reqs, constrains);
                        for mut req in cond_reqs {
                            req.condition = Some(cond_id);
                            requirements.push(req);
                        }
                    } else {
                        // Eager evaluation.
                        let flag_active = ctx.use_config.enabled.contains(flag);
                        let include = if *negate { !flag_active } else { flag_active };
                        if include {
                            Self::convert_any_of(children, ctx, requirements, constrains);
                        }
                    }
                }
                DepEntry::AnyOf(nested) => {
                    Self::convert_any_of(nested, ctx, requirements, constrains);
                }
                DepEntry::ExactlyOneOf(nested) => {
                    Self::convert_one_of_group(nested, false, ctx, requirements, constrains);
                }
                DepEntry::AtMostOneOf(nested) => {
                    Self::convert_one_of_group(nested, true, ctx, requirements, constrains);
                }
                DepEntry::AllOf(children) => {
                    let allof_id = *ctx.xof_counter;
                    *ctx.xof_counter += 1;

                    let cpn = Cpn::new("virtual", format!("allof_{allof_id}"));
                    let pkg_name = PackageName { cpn, slot: None };
                    let name_id = ctx.pool.intern_name(pkg_name);
                    ctx.cpn_slots.entry(cpn).or_default().push(name_id);

                    let meta = PackageMetadata {
                        cpv: Cpv::parse(&format!("virtual/allof_{allof_id}-1.0")).unwrap(),
                        slot: None,
                        subslot: None,
                        iuse: vec![],
                        use_flags: HashSet::new(),
                        repo: None,
                        dependencies: PackageDeps::default(),
                    };
                    let sid = ctx.pool.intern_solvable(name_id, meta);
                    ctx.candidates.entry(name_id).or_default().push(sid);

                    let constraint = VersionConstraint {
                        cpn,
                        operator: Operator::GreaterOrEqual,
                        version: Version::parse("0").unwrap(),
                        glob: false,
                        slot: None,
                        subslot: None,
                        repo: None,
                        use_constraints: vec![],
                        inverted: false,
                    };
                    let vs_id = ctx.pool.intern_version_set(name_id, constraint);

                    let mut child_reqs = Vec::new();
                    let mut child_constrains = Vec::new();
                    Self::convert_deps(children, ctx, &mut child_reqs, &mut child_constrains);

                    ctx.dep_map.insert(
                        sid,
                        KnownDependencies {
                            requirements: child_reqs,
                            constrains: child_constrains,
                        },
                    );

                    vs_ids.push(vs_id);
                }
            }
        }

        if vs_ids.len() == 1 {
            requirements.push(ConditionalRequirement {
                condition: None,
                requirement: Requirement::Single(vs_ids[0]),
            });
        } else if vs_ids.len() > 1 {
            let union_id = ctx.pool.intern_version_set_union(vs_ids);
            requirements.push(ConditionalRequirement {
                condition: None,
                requirement: Requirement::Union(union_id),
            });
        }
    }

    /// Intern a root requirement for use in [`resolvo::Problem`].
    ///
    /// Call this for every top-level package the user wants installed,
    /// then pass the resulting [`ConditionalRequirement`]s to
    /// [`resolvo::Problem::requirements`].
    pub fn intern_requirement(&mut self, dep: &Dep) -> ConditionalRequirement {
        let (slot, subslot) = extract_slot(dep);
        let (op, version) = dep_op_version(dep);
        let use_constraints = resolve_use_deps(dep, &self.use_config);

        if let Some(ref slot_val) = slot {
            // Slotted — single NameId.
            let pkg_name = PackageName {
                cpn: dep.cpn,
                slot: Some(*slot_val),
            };
            let name_id = self.pool.intern_name(pkg_name);
            let constraint = VersionConstraint {
                cpn: dep.cpn,
                operator: op,
                version,
                glob: dep.glob,
                slot: Some(*slot_val),
                subslot,
                repo: dep.repo,
                use_constraints: use_constraints.clone(),
                inverted: false,
            };
            let vs_id = self.pool.intern_version_set(name_id, constraint);
            ConditionalRequirement {
                condition: None,
                requirement: Requirement::Single(vs_id),
            }
        } else {
            // Unslotted — union over all known slots.
            let slot_names = self.cpn_slots.get(&dep.cpn).cloned();

            match slot_names {
                Some(names) if names.len() == 1 => {
                    let name_id = names[0];
                    let constraint = VersionConstraint {
                        cpn: dep.cpn,
                        operator: op,
                        version,
                        glob: dep.glob,
                        slot: None,
                        subslot: None,
                        repo: dep.repo,
                        use_constraints: use_constraints.clone(),
                        inverted: false,
                    };
                    let vs_id = self.pool.intern_version_set(name_id, constraint);
                    ConditionalRequirement {
                        condition: None,
                        requirement: Requirement::Single(vs_id),
                    }
                }
                Some(names) => {
                    let vs_ids: Vec<VersionSetId> = names
                        .iter()
                        .map(|&name_id| {
                            let constraint = VersionConstraint {
                                cpn: dep.cpn,
                                operator: op,
                                version: version.clone(),
                                glob: dep.glob,
                                slot: None,
                                subslot: None,
                                repo: dep.repo,
                                use_constraints: use_constraints.clone(),
                                inverted: false,
                            };
                            self.pool.intern_version_set(name_id, constraint)
                        })
                        .collect();
                    let union_id = self.pool.intern_version_set_union(vs_ids);
                    ConditionalRequirement {
                        condition: None,
                        requirement: Requirement::Union(union_id),
                    }
                }
                None => {
                    let pkg_name = PackageName {
                        cpn: dep.cpn,
                        slot: None,
                    };
                    let name_id = self.pool.intern_name(pkg_name);
                    let constraint = VersionConstraint {
                        cpn: dep.cpn,
                        operator: op,
                        version,
                        glob: dep.glob,
                        slot: None,
                        subslot: None,
                        repo: dep.repo,
                        use_constraints,
                        inverted: false,
                    };
                    let vs_id = self.pool.intern_version_set(name_id, constraint);
                    ConditionalRequirement {
                        condition: None,
                        requirement: Requirement::Single(vs_id),
                    }
                }
            }
        }
    }

    /// Access the underlying pool (for inspecting solution results).
    pub fn pool(&self) -> &PortagePool {
        &self.pool
    }

    /// Debug: return display names for all NameIds that have no candidates.
    pub fn debug_empty_candidates(&self) -> Vec<String> {
        let mut empty = Vec::new();
        for (name_id, solvables) in &self.candidates {
            if solvables.is_empty() {
                let pkg_name = self.pool.resolve_name(*name_id);
                empty.push(format!("{}", pkg_name));
            }
        }
        empty.sort();
        empty
    }

    /// Look up the [`PackageMetadata`] for a solved [`SolvableId`].
    pub fn package_metadata(&self, solvable: SolvableId) -> &PackageMetadata {
        self.pool.resolve_solvable(solvable)
    }

    /// Look up the blocker type (weak or strong) for a version-set that
    /// was generated from a blocker dependency.
    ///
    /// Returns `None` for version-sets that are not blockers.
    pub fn blocker_type(&self, vs_id: VersionSetId) -> Option<Blocker> {
        self.blocker_types.get(&vs_id).copied()
    }

    /// Check whether a version-set carries a `:=` slot operator,
    /// meaning the dependent package must be rebuilt when the
    /// dependency's slot or sub-slot changes.
    pub fn is_rebuild_trigger(&self, vs_id: VersionSetId) -> bool {
        self.rebuild_triggers.contains(&vs_id)
    }

    /// resolvo condition that holds when `flag` is enabled, if the flag has a
    /// solver-decided virtual.
    pub fn flag_condition(&self, flag: Interned<DefaultInterner>) -> Option<ConditionId> {
        self.flag_virtuals.get(&flag).map(|fv| fv.on_condition)
    }

    /// resolvo condition that holds when `flag` is disabled, if the flag has a
    /// solver-decided virtual.
    pub fn flag_off_condition(&self, flag: Interned<DefaultInterner>) -> Option<ConditionId> {
        self.flag_virtuals.get(&flag).map(|fv| fv.off_condition)
    }

    /// Build a labeled dependency graph from a solver solution.
    ///
    /// For each solvable in `solution`, walks its structured dependency
    /// tree and emits a [`DepEdge`] for every non-blocker atom that
    /// matches another solvable in the solution. USE-conditional groups
    /// are evaluated against the provider's [`UseConfig`].
    pub fn dependency_graph(&self, solution: &[SolvableId]) -> Vec<DepEdge> {
        let mut edges = Vec::new();

        for &from in solution {
            let meta = self.pool.resolve_solvable(from);
            for (class, entries) in meta.dependencies.iter_classes() {
                self.collect_dep_edges(from, class, entries, solution, &mut edges);
            }
        }

        edges
    }

    /// Recursively walk dep entries and emit edges.
    fn collect_dep_edges(
        &self,
        from: SolvableId,
        class: DepClass,
        entries: &[DepEntry],
        solution: &[SolvableId],
        edges: &mut Vec<DepEdge>,
    ) {
        for entry in entries {
            match entry {
                DepEntry::Atom(dep) => {
                    // Skip blockers — they don't create install-order edges.
                    if dep.blocker.is_some() {
                        continue;
                    }
                    for &to in solution {
                        if to == from {
                            continue;
                        }
                        let to_meta = self.pool.resolve_solvable(to);
                        if dep_matches_solvable(dep, to_meta, &self.use_config) {
                            edges.push(DepEdge { from, to, class });
                        }
                    }
                }
                DepEntry::UseConditional {
                    flag,
                    negate,
                    children,
                } => {
                    let flag_active = self.use_config.enabled.contains(flag)
                        || self.use_config.solver_decided.contains(flag);
                    let include = if *negate { !flag_active } else { flag_active };
                    if include {
                        self.collect_dep_edges(from, class, children, solution, edges);
                    }
                }
                DepEntry::AnyOf(alternatives)
                | DepEntry::ExactlyOneOf(alternatives)
                | DepEntry::AtMostOneOf(alternatives)
                | DepEntry::AllOf(alternatives) => {
                    self.collect_dep_edges(from, class, alternatives, solution, edges);
                }
            }
        }
    }

    /// Compute an install order from a solver solution.
    ///
    /// Returns `Ok(ordered)` with solvables in installation order
    /// (dependencies before dependents), or `Err(cycle_members)` if
    /// there is a hard cycle that cannot be broken by deferring
    /// `PDEPEND` edges.
    ///
    /// The algorithm uses Kahn's topological sort on the non-PDEPEND
    /// edges. PDEPEND edges are inherently deferrable (they represent
    /// "install after me" rather than "must exist before me"), so
    /// excluding them naturally breaks cycles that Portage handles via
    /// post-merge installation.
    pub fn install_order(
        &self,
        solution: &[SolvableId],
    ) -> Result<Vec<SolvableId>, Vec<SolvableId>> {
        let all_edges = self.dependency_graph(solution);

        // Build adjacency list and in-degree map, excluding PDEPEND edges.
        let mut adj: HashMap<SolvableId, Vec<SolvableId>> = HashMap::new();
        let mut in_degree: HashMap<SolvableId, usize> = HashMap::new();

        // Initialise all solution members.
        for &sid in solution {
            adj.entry(sid).or_default();
            in_degree.entry(sid).or_insert(0);
        }

        for edge in &all_edges {
            if edge.class == DepClass::Pdepend {
                continue; // defer PDEPEND edges
            }
            adj.entry(edge.from).or_default();
            // edge.from depends on edge.to, so edge.to → edge.from in the
            // install order graph (to must be installed before from).
            adj.entry(edge.to).or_default().push(edge.from);
            *in_degree.entry(edge.from).or_insert(0) += 1;
        }

        // Kahn's algorithm.
        let mut queue: std::collections::VecDeque<SolvableId> = in_degree
            .iter()
            .filter(|(_, deg)| **deg == 0)
            .map(|(&sid, _)| sid)
            .collect();

        // Sort the initial queue for deterministic output.
        let mut sorted_queue: Vec<SolvableId> = queue.drain(..).collect();
        sorted_queue.sort_by(|a, b| {
            let ma = self.pool.resolve_solvable(*a);
            let mb = self.pool.resolve_solvable(*b);
            ma.cpv.cmp(&mb.cpv)
        });
        queue.extend(sorted_queue);

        let mut order = Vec::with_capacity(solution.len());

        while let Some(node) = queue.pop_front() {
            order.push(node);
            if let Some(dependents) = adj.get(&node) {
                let mut next = Vec::new();
                for &dep in dependents {
                    if let Some(deg) = in_degree.get_mut(&dep) {
                        *deg -= 1;
                        if *deg == 0 {
                            next.push(dep);
                        }
                    }
                }
                // Sort for deterministic output.
                next.sort_by(|a, b| {
                    let ma = self.pool.resolve_solvable(*a);
                    let mb = self.pool.resolve_solvable(*b);
                    ma.cpv.cmp(&mb.cpv)
                });
                queue.extend(next);
            }
        }

        if order.len() == solution.len() {
            Ok(order)
        } else {
            // Remaining nodes form hard cycles.
            let ordered_set: HashSet<SolvableId> = order.iter().copied().collect();
            let cycle_members: Vec<SolvableId> = solution
                .iter()
                .copied()
                .filter(|sid| !ordered_set.contains(sid))
                .collect();
            Err(cycle_members)
        }
    }
}

// --- Display wrappers ---

struct DisplaySolvable<'a>(&'a PortagePool, SolvableId);

impl fmt::Display for DisplaySolvable<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let meta = self.0.resolve_solvable(self.1);
        write!(f, "{}", meta.cpv)?;
        if let Some(slot) = &meta.slot {
            write!(f, ":{}", slot)?;
        }
        Ok(())
    }
}

struct DisplayName<'a>(&'a PortageDependencyProvider, NameId);

impl fmt::Display for DisplayName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let pkg_name = self.0.pool.resolve_name(self.1);
        write!(f, "{}", pkg_name)?;

        // When a slotted name has no candidates but other slots of the same
        // CPN do, append a hint so the user knows the package exists in a
        // different slot.
        if pkg_name.slot.is_some() {
            let has_candidates = self
                .0
                .candidates
                .get(&self.1)
                .is_some_and(|c| !c.is_empty());

            if !has_candidates && let Some(slot_names) = self.0.cpn_slots.get(&pkg_name.cpn) {
                let available: Vec<_> = slot_names
                    .iter()
                    .filter(|&&nid| nid != self.1)
                    .filter(|&&nid| self.0.candidates.get(&nid).is_some_and(|c| !c.is_empty()))
                    .filter_map(|&nid| self.0.pool.resolve_name(nid).slot.as_deref())
                    .collect();

                if !available.is_empty() {
                    write!(
                        f,
                        " (available slots: {})",
                        available
                            .iter()
                            .map(|s| format!(":{s}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )?;
                }
            }
        }

        Ok(())
    }
}

struct DisplayVersionSet<'a>(&'a PortagePool, VersionSetId);

impl fmt::Display for DisplayVersionSet<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.resolve_version_set(self.1))
    }
}

struct DisplayString<'a>(&'a PortagePool, StringId);

impl fmt::Display for DisplayString<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.resolve_string(self.1))
    }
}

// --- Interner ---

impl resolvo::Interner for PortageDependencyProvider {
    type NameId = NameId;
    type SolvableId = SolvableId;

    fn display_solvable(&self, solvable: SolvableId) -> impl fmt::Display + '_ {
        DisplaySolvable(&self.pool, solvable)
    }

    fn display_name(&self, name: NameId) -> impl fmt::Display + '_ {
        DisplayName(self, name)
    }

    fn display_version_set(&self, version_set: VersionSetId) -> impl fmt::Display + '_ {
        DisplayVersionSet(&self.pool, version_set)
    }

    fn display_string(&self, string_id: StringId) -> impl fmt::Display + '_ {
        DisplayString(&self.pool, string_id)
    }

    fn version_set_name(&self, version_set: VersionSetId) -> NameId {
        self.pool.version_set_name(version_set)
    }

    fn solvable_name(&self, solvable: SolvableId) -> NameId {
        self.pool.solvable_name(solvable)
    }

    fn version_sets_in_union(
        &self,
        version_set_union: VersionSetUnionId,
    ) -> impl Iterator<Item = VersionSetId> {
        self.pool
            .resolve_version_set_union(version_set_union)
            .iter()
            .copied()
    }

    fn resolve_condition(&self, condition: resolvo::ConditionId) -> Condition {
        self.pool.resolve_condition(condition).clone()
    }
}

// --- DependencyProvider ---

impl resolvo::DependencyProvider for PortageDependencyProvider {
    async fn get_candidates(&self, name: NameId) -> Option<Candidates> {
        let solvables = self.candidates.get(&name)?;
        Some(Candidates {
            candidates: solvables.clone(),
            favored: self.favored.get(&name).copied(),
            locked: self.locked.get(&name).copied(),
            hint_dependencies_available: HintDependenciesAvailable::All,
            excluded: Vec::new(),
        })
    }

    async fn sort_candidates(&self, _solver: &SolverCache<Self>, solvables: &mut [SolvableId]) {
        // Sort newest first so the solver prefers newer versions.
        solvables.sort_by(|a, b| {
            let va = &self.pool.resolve_solvable(*a).cpv.version;
            let vb = &self.pool.resolve_solvable(*b).cpv.version;
            vb.cmp(va) // descending
        });
    }

    async fn filter_candidates(
        &self,
        candidates: &[SolvableId],
        version_set: VersionSetId,
        inverse: bool,
    ) -> Vec<SolvableId> {
        let constraint = self.pool.resolve_version_set(version_set);

        candidates
            .iter()
            .copied()
            .filter(|&sid| {
                let meta = self.pool.resolve_solvable(sid);
                let mut matches = version_matches(
                    &meta.cpv.version,
                    &constraint.operator,
                    constraint.glob,
                    &constraint.version,
                ) && slot_matches(meta, constraint);

                // Blocker constrains store the *blocked* operator with
                // `inverted = true`.  Flipping the match here means resolvo's
                // own `inverse` flag (used for constrains) ends up forbidding
                // candidates that *match* the blocker — exactly what we want.
                // See [`VersionConstraint`] for the full explanation.
                if constraint.inverted {
                    matches = !matches;
                }

                if inverse { !matches } else { matches }
            })
            .collect()
    }

    async fn get_dependencies(&self, solvable: SolvableId) -> Dependencies {
        match self.dependencies.get(&solvable) {
            Some(deps) => Dependencies::Known(deps.clone()),
            None => Dependencies::Known(KnownDependencies {
                requirements: Vec::new(),
                constrains: Vec::new(),
            }),
        }
    }
}

// --- helpers ---

/// Extract the slot and sub-slot from a [`Dep`]'s slot dependency.
///
/// Returns `(slot, subslot)`. `:*` and `:=` return `(None, None)`,
/// which makes `slot_matches` accept all candidates regardless of
/// their slot.
fn extract_slot(
    dep: &Dep,
) -> (
    Option<Interned<DefaultInterner>>,
    Option<Interned<DefaultInterner>>,
) {
    match &dep.slot_dep {
        Some(SlotDep::Slot {
            slot: Some(s),
            op: _,
        }) => (Some(s.slot), s.subslot),
        Some(SlotDep::Operator(SlotOperator::Star)) => (None, None),
        Some(SlotDep::Operator(SlotOperator::Equal)) => (None, None),
        _ => (None, None),
    }
}

/// Check whether a dep carries a `:=` slot operator (rebuild trigger).
///
/// This matches both bare `:=` and named-slot `:SLOT=` forms.
fn has_slot_equal_op(dep: &Dep) -> bool {
    matches!(
        &dep.slot_dep,
        Some(SlotDep::Operator(SlotOperator::Equal))
            | Some(SlotDep::Slot {
                op: Some(SlotOperator::Equal),
                ..
            })
    )
}

/// Extract operator and bare version from a dep (defaults to `>=0` for unversioned).
fn dep_op_version(dep: &Dep) -> (Operator, Version) {
    match &dep.version {
        Some(v) => {
            let op = dep.op.unwrap_or(Operator::Equal);
            (op, v.clone())
        }
        None => (Operator::GreaterOrEqual, Version::parse("0").unwrap()),
    }
}

/// Check whether a candidate's slot, sub-slot, and repository match the constraint.
///
/// USE-dep constraints are **not** enforced here — they require profile
/// context (enabled/disabled flags) that isn't available at solve time.
/// USE deps should be validated post-solve, matching the pubgrub provider.
fn slot_matches(meta: &PackageMetadata, constraint: &VersionConstraint) -> bool {
    if let Some(required_slot) = constraint.slot
        && meta.slot != Some(required_slot)
    {
        return false;
    }
    if let Some(required_subslot) = constraint.subslot
        && meta.subslot != Some(required_subslot)
    {
        return false;
    }
    if let Some(required_repo) = constraint.repo
        && meta.repo != Some(required_repo)
    {
        return false;
    }
    true
}

/// Check whether a dependency atom matches a concrete package version.
///
/// This is the post-solve counterpart of `filter_candidates`: it tests
/// CPN, version operator, slot, sub-slot, repository, and USE dep
/// constraints against a [`PackageMetadata`].
fn dep_matches_solvable(dep: &Dep, meta: &PackageMetadata, use_config: &UseConfig) -> bool {
    // CPN must match.
    if dep.cpn != meta.cpv.cpn {
        return false;
    }

    // Version constraint (if any).
    let (op, constraint_version) = dep_op_version(dep);
    if !version_matches(&meta.cpv.version, &op, dep.glob, &constraint_version) {
        return false;
    }

    // Slot / sub-slot from the dep atom.
    let (slot, subslot) = extract_slot(dep);
    if let Some(required_slot) = slot
        && meta.slot != Some(required_slot)
    {
        return false;
    }
    if let Some(required_subslot) = subslot
        && meta.subslot != Some(required_subslot)
    {
        return false;
    }

    // Repository constraint.
    if let Some(required_repo) = dep.repo
        && meta.repo != Some(required_repo)
    {
        return false;
    }

    // USE dep constraints.
    let use_constraints = resolve_use_deps(dep, use_config);
    for (flag, must_be_enabled) in &use_constraints {
        let is_enabled = meta.use_flags.contains(flag);
        if is_enabled != *must_be_enabled {
            return false;
        }
    }

    true
}

fn resolve_use_deps(dep: &Dep, use_config: &UseConfig) -> Vec<(Interned<DefaultInterner>, bool)> {
    let Some(use_deps) = &dep.use_deps else {
        return Vec::new();
    };
    let mut constraints = Vec::new();
    for ud in use_deps {
        let parent_flag_on = use_config.enabled.contains(&ud.flag);
        match ud.kind {
            UseDepKind::Enabled => constraints.push((ud.flag, true)),
            UseDepKind::Disabled => constraints.push((ud.flag, false)),
            UseDepKind::Conditional => {
                if parent_flag_on {
                    constraints.push((ud.flag, true));
                }
            }
            UseDepKind::ConditionalInverse => {
                if !parent_flag_on {
                    constraints.push((ud.flag, true));
                }
            }
            UseDepKind::Equal => {
                constraints.push((ud.flag, parent_flag_on));
            }
            UseDepKind::EqualInverse => {
                constraints.push((ud.flag, !parent_flag_on));
            }
        }
    }
    #[allow(clippy::unnecessary_sort_by)]
    constraints.sort_by(|a, b| a.0.cmp(&b.0));
    constraints
}
