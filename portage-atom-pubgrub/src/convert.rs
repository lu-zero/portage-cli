use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Dep, DepEntry, Operator, SlotDep, SlotOperator, UseDep, Version};

use crate::package::PortagePackage;
use crate::required_use::RequiredUse;
use crate::use_config::{UseConfig, UseFlagState};
use crate::version_set::PortageVersionSet;

/// One dependency requirement: target package, version range, and the
/// outermost eagerly-evaluated USE flag gating it (if any).
pub(crate) type Req = (
    PortagePackage,
    PortageVersionSet,
    Option<Interned<DefaultInterner>>,
);

static CHOICE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn next_choice_id() -> u64 {
    CHOICE_COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// A virtual choice package encoding an OR group (`||`, `^^`, `??`).
///
/// The solver picks exactly one version of this package, and each version's
/// dependencies are one alternative from the group.
#[derive(Clone)]
pub(crate) struct VirtualChoice {
    /// The virtual package to register in the provider.
    pub package: PortagePackage,
    /// (version, dependencies for that version with optional gating flag).
    pub versions: Vec<(Version, Vec<Req>)>,
    /// Per-branch USE dep constraints, indexed parallel to `versions`.
    ///
    /// Stored separately so `choose_version` can check USE dep satisfiability
    /// per branch without those constraints leaking into the parent's dep list.
    pub branch_use_deps: Vec<(Version, Vec<UseDepConstraint>)>,
    /// Per-branch blockers, parallel to `versions`.
    ///
    /// Attached to the virtual package's chosen version so post-solve
    /// `check_blockers` only fires blockers from the selected OR/`UseDecision`
    /// branch — not every alternative.
    pub branch_blockers: Vec<(Version, Vec<Dep>)>,
}

/// Result of converting a dependency tree.
#[derive(Clone)]
pub(crate) struct ConversionResult {
    /// Direct dependency constraints with the outermost gating USE flag, if any.
    pub requirements: Vec<Req>,
    /// Blocker atoms for post-solve validation.
    pub blockers: Vec<Dep>,
    /// Virtual choice packages to register in the provider.
    pub virtual_choices: Vec<VirtualChoice>,
    /// USE-dep constraints for post-solve validation.
    pub use_deps: Vec<UseDepConstraint>,
    /// Repo-constrained deps for post-solve validation.
    pub repo_constraints: Vec<RepoConstraint>,
    /// Slot-operator deps (`:=` / `:0=`) for post-solve slot binding.
    pub slot_operator_deps: Vec<SlotOperatorDep>,
}

/// A slot-operator dependency extracted during conversion.
///
/// Records `:=` and `:slot=` dependencies so that, after resolution,
/// the solver's slot assignment can be bound for rebuild tracking.
///
/// `:*` dependencies are **not** collected — they express "any slot"
/// with no rebuild implications.
///
/// See [PMS 8.3.3](https://projects.gentoo.org/pms/9/pms.html#slot_deps).
#[derive(Debug, Clone)]
pub(crate) struct SlotOperatorDep {
    /// The target package and version range.
    pub target: (PortagePackage, PortageVersionSet),
    /// The slot operator (`Equal` for `:=` / `:0=`).
    pub operator: SlotOperator,
    /// The declared slot name, if the atom specified one (`:0=` → `Some("0")`,
    /// `:=` → `None`).
    pub slot: Option<Interned<DefaultInterner>>,
}

/// A repository constraint extracted from a dependency atom.
///
/// See [PMS 8.3.5](https://projects.gentoo.org/pms/9/pms.html#repository).
#[derive(Debug, Clone)]
pub(crate) struct RepoConstraint {
    /// The target package that must come from a specific repo.
    pub target: (PortagePackage, PortageVersionSet),
    /// The required repository name.
    pub repo: Interned<DefaultInterner>,
}

/// A USE-dep constraint extracted from a dependency atom.
///
/// See [PMS 8.3.4](https://projects.gentoo.org/pms/9/pms.html#style-and-style-use-dependencies).
#[derive(Debug, Clone)]
pub(crate) struct UseDepConstraint {
    /// The package that must satisfy the USE constraints.
    pub target: (PortagePackage, PortageVersionSet),
    /// The USE flag constraints on that package.
    pub use_deps: Vec<UseDep>,
    /// The parent package's CPN string (e.g. "dev-libs/openssl"),
    /// used to look up solver-decided USE virtuals for `[flag?]`/`[flag=]`.
    /// Interned: this is allocated once per USE-dep atom per provider
    /// rebuild (up to ~8x per invocation during the USE-dep co-solve
    /// fixpoint), then carried through a `BTreeSet` in `post_solve.rs`.
    pub parent_cpn_str: Interned<DefaultInterner>,
}

/// Slot info for a CPN: pre-computed `(slot_interned, slotted_package)` pairs.
pub type SlotMap = HashMap<Cpn, Vec<(Interned<DefaultInterner>, PortagePackage)>>;

/// The `UseDecision` virtual package for a given package's USE flag.
///
/// USE flags are package-scoped, so the node name embeds the CPN: the *same*
/// node is shared by the conditional-dep encoding and the `REQUIRED_USE`
/// encoding, which is what makes "force flag on" also fire the deps gated on it.
pub(crate) fn use_decision_package(
    cpn_str: &str,
    flag: &Interned<DefaultInterner>,
) -> PortagePackage {
    PortagePackage::use_decision(Interned::intern(&format!(
        "USE_{}_{}",
        cpn_str.replace('/', "_"),
        flag.as_str()
    )))
}

/// Convert a `DepEntry` tree into PubGrub dependency constraints.
///
/// USE conditionals are handled according to the `UseConfig`:
/// - `Enabled`/`Disabled` flags are eagerly evaluated
/// - `SolverDecided` flags produce virtual package references
///
/// OR groups (`||`, `^^`, `??`) are encoded as virtual choice packages.
///
/// `slot_map` maps each CPN to its known slots, so unslotted deps on
/// multi-slot packages can be resolved correctly.
pub fn convert_deps(
    entries: &[DepEntry],
    cpn_str: &str,
    use_config: &UseConfig,
    slot_map: &SlotMap,
) -> ConversionResult {
    let mut ctx = ConvertCtx {
        cpn_str,
        use_config,
        slot_map,
        requirements: Vec::new(),
        blockers: Vec::new(),
        virtual_choices: Vec::new(),
        use_deps: Vec::new(),
        repo_constraints: Vec::new(),
        slot_operator_deps: Vec::new(),
        gating_flag: None,
    };

    for entry in entries {
        ctx.convert_entry(entry);
    }

    ConversionResult {
        requirements: ctx.requirements,
        blockers: ctx.blockers,
        virtual_choices: ctx.virtual_choices,
        use_deps: ctx.use_deps,
        repo_constraints: ctx.repo_constraints,
        slot_operator_deps: ctx.slot_operator_deps,
    }
}

struct ConvertCtx<'a> {
    cpn_str: &'a str,
    use_config: &'a UseConfig,
    slot_map: &'a SlotMap,
    requirements: Vec<Req>,
    blockers: Vec<Dep>,
    virtual_choices: Vec<VirtualChoice>,
    use_deps: Vec<UseDepConstraint>,
    repo_constraints: Vec<RepoConstraint>,
    slot_operator_deps: Vec<SlotOperatorDep>,
    /// The outermost eagerly-evaluated USE flag currently gating deps, if any.
    gating_flag: Option<Interned<DefaultInterner>>,
}

impl ConvertCtx<'_> {
    fn convert_entry(&mut self, entry: &DepEntry) {
        match entry {
            DepEntry::Atom(dep) => {
                if dep.blocker.is_some() {
                    self.blockers.push(dep.clone());
                } else {
                    self.convert_atom(dep);
                }
            }
            DepEntry::UseConditional {
                flag,
                negate,
                children,
            } => {
                // `use_config` is the caller-resolved desired set, with IUSE
                // defaults already folded in, so a plain lookup is authoritative.
                let state = self.use_config.get(*flag);
                match state {
                    UseFlagState::Enabled => {
                        if !negate {
                            let prev = self.gating_flag.take();
                            if prev.is_none() {
                                self.gating_flag = Some(*flag);
                            }
                            for child in children {
                                self.convert_entry(child);
                            }
                            self.gating_flag = prev;
                        }
                    }
                    UseFlagState::Disabled => {
                        if *negate {
                            for child in children {
                                self.convert_entry(child);
                            }
                        }
                    }
                    UseFlagState::SolverDecided { .. } => {
                        let virtual_pkg = use_decision_package(self.cpn_str, flag);

                        // Blockers ride on the UseDecision branch versions, not
                        // the parent package — so check_blockers only fires them
                        // when the solver actually picks that flag value.
                        let (on_deps, on_blockers) = if *negate {
                            (vec![], vec![])
                        } else {
                            let saved_reqs = std::mem::take(&mut self.requirements);
                            let saved_blockers = std::mem::take(&mut self.blockers);
                            for child in children {
                                self.convert_entry(child);
                            }
                            let reqs = std::mem::take(&mut self.requirements);
                            let on_blockers = std::mem::take(&mut self.blockers);
                            self.requirements = saved_reqs;
                            self.blockers = saved_blockers;
                            (reqs, on_blockers)
                        };

                        let (off_deps, off_blockers) = if *negate {
                            let saved_reqs = std::mem::take(&mut self.requirements);
                            let saved_blockers = std::mem::take(&mut self.blockers);
                            for child in children {
                                self.convert_entry(child);
                            }
                            let reqs = std::mem::take(&mut self.requirements);
                            let off_blockers = std::mem::take(&mut self.blockers);
                            self.requirements = saved_reqs;
                            self.blockers = saved_blockers;
                            (reqs, off_blockers)
                        } else {
                            (vec![], vec![])
                        };

                        let v0 = Version::new(&[0]);
                        let v1 = Version::new(&[1]);
                        self.virtual_choices.push(VirtualChoice {
                            package: virtual_pkg.clone(),
                            versions: vec![(v0.clone(), off_deps), (v1.clone(), on_deps)],
                            branch_use_deps: vec![],
                            branch_blockers: vec![(v0, off_blockers), (v1, on_blockers)],
                        });
                        self.requirements.push((
                            virtual_pkg,
                            PortageVersionSet::any(),
                            self.gating_flag,
                        ));
                    }
                }
            }
            DepEntry::AnyOf(children) | DepEntry::ExactlyOneOf(children) => {
                self.convert_choice_group(children, false);
            }
            DepEntry::AtMostOneOf(children) => {
                self.convert_choice_group(children, true);
            }
            DepEntry::AllOf(children) => {
                for child in children {
                    self.convert_entry(child);
                }
            }
        }
    }

    fn convert_atom(&mut self, dep: &Dep) {
        let cpn = &dep.cpn;
        let version_set = match &dep.version {
            Some(v) => {
                let op = dep.op.unwrap_or(Operator::Equal);
                PortageVersionSet::from_operator(op, dep.glob, v.clone())
            }
            None => PortageVersionSet::any(),
        };

        if let Some(slot_dep) = &dep.slot_dep {
            match slot_dep {
                SlotDep::Slot {
                    slot: Some(slot),
                    op,
                } => {
                    let pkg = PortagePackage::slotted(*cpn, slot.slot);
                    self.collect_post_solve(&pkg, &version_set, dep);
                    self.collect_slot_op(&pkg, &version_set, *op, Some(slot.slot));
                    self.requirements.push((pkg, version_set, self.gating_flag));
                }
                SlotDep::Slot { slot: None, op } => {
                    self.collect_slot_op_unslotted(cpn, &version_set, *op);
                    self.collect_post_solve_unslotted(cpn, &version_set, dep);
                    self.push_unslotted_or_choice(cpn, version_set);
                }
                SlotDep::Operator(SlotOperator::Equal) => {
                    self.collect_slot_op_unslotted(cpn, &version_set, Some(SlotOperator::Equal));
                    self.collect_post_solve_unslotted(cpn, &version_set, dep);
                    self.push_unslotted_or_choice(cpn, version_set);
                }
                SlotDep::Operator(SlotOperator::Star) => {
                    self.collect_post_solve_unslotted(cpn, &version_set, dep);
                    self.push_unslotted_or_choice(cpn, version_set);
                }
            }
            return;
        }

        self.collect_post_solve_unslotted(cpn, &version_set, dep);
        self.push_unslotted_or_choice(cpn, version_set);
    }

    fn collect_post_solve(&mut self, pkg: &PortagePackage, vs: &PortageVersionSet, dep: &Dep) {
        if let Some(use_deps) = &dep.use_deps
            && !use_deps.is_empty()
        {
            self.use_deps.push(UseDepConstraint {
                target: (pkg.clone(), vs.clone()),
                use_deps: use_deps.clone(),
                parent_cpn_str: Interned::intern(self.cpn_str),
            });
        }
        if let Some(repo) = dep.repo {
            self.repo_constraints.push(RepoConstraint {
                target: (pkg.clone(), vs.clone()),
                repo,
            });
        }
    }

    fn collect_post_solve_unslotted(&mut self, cpn: &Cpn, vs: &PortageVersionSet, dep: &Dep) {
        match self.slot_map.get(cpn).map(|v| v.as_slice()) {
            // Single accepted slot: attach to that exact PortagePackage key.
            Some([(_, sole_pkg)]) => self.collect_post_solve(sole_pkg, vs, dep),
            // Multi-slot: the provider only ever selects slotted packages, so
            // an unslotted key never hits installed/solution lookups. Attach
            // the constraint to every slotted candidate for this CPN.
            Some(slots) if !slots.is_empty() => {
                for (_, pkg) in slots {
                    self.collect_post_solve(pkg, vs, dep);
                }
            }
            // No slot info yet: fall back to unslotted (single-slot packages
            // that haven't been slot-mapped, or synthetic tests).
            _ => {
                let pkg = PortagePackage::unslotted(*cpn);
                self.collect_post_solve(&pkg, vs, dep);
            }
        }
    }

    fn collect_slot_op(
        &mut self,
        pkg: &PortagePackage,
        vs: &PortageVersionSet,
        op: Option<SlotOperator>,
        declared_slot: Option<Interned<DefaultInterner>>,
    ) {
        if op == Some(SlotOperator::Equal) {
            self.slot_operator_deps.push(SlotOperatorDep {
                target: (pkg.clone(), vs.clone()),
                operator: SlotOperator::Equal,
                slot: declared_slot,
            });
        }
    }

    fn collect_slot_op_unslotted(
        &mut self,
        cpn: &Cpn,
        vs: &PortageVersionSet,
        op: Option<SlotOperator>,
    ) {
        if op != Some(SlotOperator::Equal) {
            return;
        }
        if let Some([(_, sole_pkg)]) = self.slot_map.get(cpn).map(|v| v.as_slice()) {
            self.slot_operator_deps.push(SlotOperatorDep {
                target: (sole_pkg.clone(), vs.clone()),
                operator: SlotOperator::Equal,
                slot: None,
            });
        } else {
            let pkg = PortagePackage::unslotted(*cpn);
            self.slot_operator_deps.push(SlotOperatorDep {
                target: (pkg, vs.clone()),
                operator: SlotOperator::Equal,
                slot: None,
            });
        }
    }

    fn push_unslotted_or_choice(&mut self, cpn: &Cpn, version_set: PortageVersionSet) {
        let target_slots = self.slot_map.get(cpn).map(|v| v.as_slice());

        match target_slots {
            None | Some([]) => {
                self.requirements.push((
                    PortagePackage::unslotted(*cpn),
                    version_set,
                    self.gating_flag,
                ));
            }
            Some([(_, sole_pkg)]) => {
                self.requirements
                    .push((sole_pkg.clone(), version_set, self.gating_flag));
            }
            Some(slots) => {
                let id = next_choice_id();
                let choice_pkg = PortagePackage::slot_choice(id);
                let gf = self.gating_flag;
                // Unlike `||` Choice nodes (first-listed wins via n-i numbering),
                // slot-star deps have no listed preference: number slots so the
                // lex-last slot (typically newest for numeric slots like gcc:16)
                // gets the highest synthetic version and `choose_version`'s max()
                // picks it without a separate min() path.
                let versions: Vec<(Version, Vec<Req>)> = slots
                    .iter()
                    .enumerate()
                    .map(|(i, (_, slot_pkg))| {
                        let ver = Version::new(&[(i + 1) as u64]);
                        (ver, vec![(slot_pkg.clone(), version_set.clone(), gf)])
                    })
                    .collect();
                self.virtual_choices.push(VirtualChoice {
                    package: choice_pkg.clone(),
                    versions,
                    branch_use_deps: vec![],
                    branch_blockers: vec![],
                });
                // The slot-choice virtual itself is gated by the same flag.
                self.requirements
                    .push((choice_pkg, PortageVersionSet::any(), self.gating_flag));
            }
        }
    }

    fn convert_choice_group(&mut self, children: &[DepEntry], allow_none: bool) {
        let id = next_choice_id();
        let pkg = PortagePackage::choice(id);

        let n = children.len();
        let mut versions = Vec::new();
        let mut branch_use_deps: Vec<(Version, Vec<UseDepConstraint>)> = Vec::new();
        let mut branch_blockers: Vec<(Version, Vec<Dep>)> = Vec::new();

        if allow_none {
            versions.push((Version::new(&[0]), vec![])); // empty: no deps for "none" branch
            branch_use_deps.push((Version::new(&[0]), vec![]));
            branch_blockers.push((Version::new(&[0]), vec![]));
        }

        for (i, child) in children.iter().enumerate() {
            let ver_num = n - i;
            let saved_reqs = std::mem::take(&mut self.requirements);
            let saved_blockers = std::mem::take(&mut self.blockers);
            // Save use_deps so they don't leak into the parent's dep list.
            // USE dep constraints from inside an OR branch only apply when
            // that branch is chosen — they must stay per-branch.
            let saved_use_deps = std::mem::take(&mut self.use_deps);

            self.convert_entry(child);

            let deps = std::mem::take(&mut self.requirements);
            let choice_blockers = std::mem::take(&mut self.blockers);
            let this_branch_use_deps = std::mem::take(&mut self.use_deps);

            self.requirements = saved_reqs;
            self.blockers = saved_blockers;
            self.use_deps = saved_use_deps;
            // Blockers stay on the branch (attached via branch_blockers), not
            // promoted to the parent package.

            let ver = Version::new(&[ver_num as u64]);
            versions.push((ver.clone(), deps));
            branch_use_deps.push((ver.clone(), this_branch_use_deps));
            branch_blockers.push((ver, choice_blockers));
        }

        self.virtual_choices.push(VirtualChoice {
            package: pkg.clone(),
            versions,
            branch_use_deps,
            branch_blockers,
        });
        // The choice virtual itself carries the current gating flag.
        self.requirements
            .push((pkg, PortageVersionSet::any(), self.gating_flag));
    }
}

// ===========================================================================
// Level-C REQUIRED_USE encoding  (docs/required-use-level-c.md)
// ===========================================================================

/// Result of encoding a package's `REQUIRED_USE` into solver constraints.
pub(crate) struct RequiredUseEncoding {
    /// Requirements to add to the package version: pulls/forces the relevant
    /// `UseDecision` nodes and references the at-least-one `Choice` nodes.
    pub requirements: Vec<Req>,
    /// `UseDecision` nodes (with implication / pairwise-exclusion deps) and
    /// `Choice` nodes (at-least-one).  Merged with the conditional-dep nodes by
    /// `register_virtual_choices`.
    pub virtual_choices: Vec<VirtualChoice>,
}

/// One operand of a `REQUIRED_USE` group/clause, after classifying its flag
/// against the desired config. Doubles as a clause *literal* for the guarded
/// (nested) encoding (`emit_guarded`).
#[derive(Clone)]
enum Operand {
    /// A flag whose value is fixed by policy; `satisfied` is whether the operand
    /// (respecting its `!`) already holds.
    Fixed { satisfied: bool },
    /// A ceded flag: `node` is its `UseDecision`, `sat_ver` the version (`0`/`1`)
    /// at which the operand (respecting its `!`) is satisfied, and `prefer_ver`
    /// the version the keep-config preference would pick (used to order choice
    /// branches toward a no-flip solution).
    Free {
        node: PortagePackage,
        sat_ver: u64,
        prefer_ver: u64,
    },
}

/// A ceded clause literal: `(node, required_ver, prefer_ver)`.
type FreeLit = (PortagePackage, u64, u64);

/// The ceded literals among a list of classified operands; `Fixed` operands
/// have no node and are dropped.
fn free_operands(ops: &[Operand]) -> Vec<FreeLit> {
    ops.iter()
        .filter_map(|o| match o {
            Operand::Free {
                node,
                sat_ver,
                prefer_ver,
            } => Some((node.clone(), *sat_ver, *prefer_ver)),
            Operand::Fixed { .. } => None,
        })
        .collect()
}

/// Encode a package's `REQUIRED_USE` into `UseDecision`/`Choice` constraints.
///
/// Fixed flags are partially evaluated; ceded (`SolverDecided`) flags are
/// constrained. The one construct the encoder does not handle (group operands
/// that are not bare flags) is left unencoded — the cli's Level-A check still
/// reports it. See `docs/required-use-level-c.md`.
pub(crate) fn encode_required_use(
    cpn_str: &str,
    ru: &RequiredUse,
    desired: &UseConfig,
) -> RequiredUseEncoding {
    let mut b = RuBuilder {
        cpn_str,
        desired,
        node_on: HashMap::new(),
        node_off: HashMap::new(),
        touched: std::collections::BTreeSet::new(),
        requirements: Vec::new(),
        virtual_choices: Vec::new(),
    };
    b.clause(ru);
    b.finish()
}

struct RuBuilder<'a> {
    cpn_str: &'a str,
    desired: &'a UseConfig,
    /// Extra deps for each node's version-1 (flag ON).
    node_on: HashMap<PortagePackage, Vec<Req>>,
    /// Extra deps for each node's version-0 (flag OFF).
    node_off: HashMap<PortagePackage, Vec<Req>>,
    /// Nodes that must exist (both versions registered) and be pulled in.
    touched: std::collections::BTreeSet<PortagePackage>,
    requirements: Vec<Req>,
    virtual_choices: Vec<VirtualChoice>,
}

impl RuBuilder<'_> {
    const OFF: u64 = 0;
    const ON: u64 = 1;

    fn ver(v: u64) -> Version {
        Version::new(&[v])
    }

    fn singleton(node: &PortagePackage, v: u64) -> Req {
        (
            node.clone(),
            PortageVersionSet::from_operator(Operator::Equal, false, Self::ver(v)),
            None,
        )
    }

    /// Classify a `Flag { name, negated }` operand against the desired config.
    fn operand(&self, name: &Interned<DefaultInterner>, negated: bool) -> Operand {
        match self.desired.get(*name) {
            UseFlagState::SolverDecided { prefer } => Operand::Free {
                node: use_decision_package(self.cpn_str, name),
                sat_ver: if negated { Self::OFF } else { Self::ON },
                prefer_ver: u64::from(prefer),
            },
            state => {
                let on = matches!(state, UseFlagState::Enabled);
                Operand::Fixed {
                    satisfied: on != negated,
                }
            }
        }
    }

    /// Order ceded literals so those the keep-config preference already satisfies
    /// come first (highest choice versions), so the solver can satisfy a clause
    /// without flipping a flag when it already can. Returns `(node, required_ver)`.
    fn order_by_preference(lits: &[FreeLit]) -> Vec<(PortagePackage, u64)> {
        let mut v: Vec<&FreeLit> = lits.iter().collect();
        // `false` (preference already satisfies the literal) sorts before `true`;
        // sort is stable, so original order is kept within each group.
        v.sort_by_key(|(_, req, prefer)| req != prefer);
        v.into_iter().map(|(n, req, _)| (n.clone(), *req)).collect()
    }

    /// Force a ceded node to a specific version on the package itself.
    fn force(&mut self, node: &PortagePackage, v: u64) {
        self.touched.insert(node.clone());
        self.requirements.push(Self::singleton(node, v));
    }

    /// When `src` is at `src_ver`, require `dst` at `dst_ver`.
    fn imply(&mut self, src: &PortagePackage, src_ver: u64, dst: &PortagePackage, dst_ver: u64) {
        self.touched.insert(src.clone());
        self.touched.insert(dst.clone());
        let bucket = if src_ver == Self::ON {
            self.node_on.entry(src.clone()).or_default()
        } else {
            self.node_off.entry(src.clone()).or_default()
        };
        bucket.push(Self::singleton(dst, dst_ver));
    }

    /// At least one of the ceded operands must be satisfied (a `Choice` node).
    /// Branches are ordered preference-satisfied-first so the solver picks an
    /// already-met operand (no flip) when one exists.
    fn at_least_one(&mut self, free: &[FreeLit]) {
        let ordered = Self::order_by_preference(free);
        let id = next_choice_id();
        let pkg = PortagePackage::choice(id);
        let n = ordered.len();
        let mut versions = Vec::with_capacity(n);
        for (i, (node, sat_ver)) in ordered.iter().enumerate() {
            // n..1 numbering (parallel to convert_choice_group), one branch per flag.
            versions.push((
                Self::ver((n - i) as u64),
                vec![Self::singleton(node, *sat_ver)],
            ));
            self.touched.insert(node.clone());
        }
        self.virtual_choices.push(VirtualChoice {
            package: pkg.clone(),
            versions,
            branch_use_deps: Vec::new(),
            branch_blockers: Vec::new(),
        });
        self.requirements
            .push((pkg, PortageVersionSet::any(), None));
    }

    /// At most one of the ceded operands may be satisfied (pairwise exclusion).
    fn at_most_one(&mut self, free: &[FreeLit]) {
        for (i, (ni, svi, _)) in free.iter().enumerate() {
            for (j, (nj, svj, _)) in free.iter().enumerate() {
                if i != j {
                    // ni satisfied ⇒ nj not satisfied.
                    self.imply(ni, *svi, nj, Self::ON + Self::OFF - *svj);
                }
            }
        }
    }

    /// Encode one clause (recursing through top-level `All`).
    fn clause(&mut self, c: &RequiredUse) {
        match c {
            RequiredUse::All(children) => {
                for child in children {
                    self.clause(child);
                }
            }
            RequiredUse::Flag { name, negated } => match self.operand(name, *negated) {
                Operand::Free { node, sat_ver, .. } => self.force(&node, sat_ver),
                // Fixed-satisfied: nothing to do. Fixed-unsatisfiable: a fixed
                // flag we cannot change — leave it to the Level-A reporter.
                Operand::Fixed { .. } => {}
            },
            RequiredUse::AnyOf(ch) => self.group(ch, Group::Any),
            RequiredUse::ExactlyOne(ch) => self.group(ch, Group::Exactly),
            RequiredUse::AtMostOne(ch) => self.group(ch, Group::AtMost),
            RequiredUse::UseConditional {
                flag,
                negated,
                entries,
            } => self.conditional(flag, *negated, entries),
        }
    }

    fn group(&mut self, children: &[RequiredUse], kind: Group) {
        // Only flat groups of bare flags are encoded; anything else is deferred.
        let Some(ops) = self.flag_operands(children) else {
            return;
        };
        let fixed_sat = ops
            .iter()
            .filter(|o| matches!(o, Operand::Fixed { satisfied: true }))
            .count();
        let free: Vec<FreeLit> = free_operands(&ops);

        match kind {
            Group::Any => {
                if fixed_sat >= 1 {
                    return; // already satisfied
                }
                match free.len() {
                    0 => {} // unsatisfiable → Level A
                    1 => self.force(&free[0].0, free[0].1),
                    _ => self.at_least_one(&free),
                }
            }
            Group::AtMost => {
                if fixed_sat >= 2 {
                    return; // unsatisfiable → Level A
                }
                if fixed_sat == 1 {
                    // the fixed one is "the" one → all free must be off.
                    for (node, sv, _) in &free {
                        self.force(node, Self::ON + Self::OFF - *sv);
                    }
                } else if free.len() >= 2 {
                    self.at_most_one(&free);
                }
            }
            Group::Exactly => {
                if fixed_sat >= 2 {
                    return; // unsatisfiable → Level A
                }
                if fixed_sat == 1 {
                    for (node, sv, _) in &free {
                        self.force(node, Self::ON + Self::OFF - *sv);
                    }
                } else {
                    match free.len() {
                        0 => {} // unsatisfiable → Level A
                        1 => self.force(&free[0].0, free[0].1),
                        _ => {
                            self.at_least_one(&free);
                            self.at_most_one(&free);
                        }
                    }
                }
            }
        }
    }

    fn conditional(
        &mut self,
        flag: &Interned<DefaultInterner>,
        negated: bool,
        entries: &[RequiredUse],
    ) {
        match self.desired.get(*flag) {
            UseFlagState::SolverDecided { prefer } => {
                // Gate every consequent behind the guard: the context literal is
                // the guard's *escape* (its inactive value) — a clause holds when
                // any guard is inactive or the body is satisfied.
                let guard = use_decision_package(self.cpn_str, flag);
                let active_ver = if negated { Self::OFF } else { Self::ON };
                let mut ctx = vec![(guard, Self::ON + Self::OFF - active_ver, u64::from(prefer))];
                for e in entries {
                    self.guarded(e, &mut ctx);
                }
            }
            state => {
                // Fixed guard: if active, the entries are mandatory clauses.
                let on = matches!(state, UseFlagState::Enabled);
                let active = on != negated;
                if active {
                    for e in entries {
                        self.clause(e);
                    }
                }
            }
        }
    }

    /// Classify a list of children as bare-flag operands, or `None` if any child
    /// is not a bare `Flag` (a deeper nested group — still deferred).
    fn flag_operands(&self, children: &[RequiredUse]) -> Option<Vec<Operand>> {
        children
            .iter()
            .map(|c| match c {
                RequiredUse::Flag { name, negated } => Some(self.operand(name, *negated)),
                _ => None,
            })
            .collect()
    }

    /// Negate a ceded literal (the preference stays the flag's own).
    fn negate(l: &FreeLit) -> FreeLit {
        (l.0.clone(), Self::ON + Self::OFF - l.1, l.2)
    }

    /// Emit `(⋁ ctx) ∨ (⋁ body)` — the clause form of "all guards active ⇒
    /// body", where each `ctx` literal is a guard's *inactive* value (its
    /// escape). A single guard keeps the cheaper directional bucket form.
    ///
    /// Two or more guards — a Horn-style dependency edge can't express this
    /// — become a preference-ordered `Choice` over body-then-guard literals,
    /// so the solver tries satisfying the consequent before flipping a guard.
    fn emit_clause(&mut self, ctx: &[FreeLit], body: &[FreeLit]) {
        match (ctx, body) {
            ([], []) => {}
            ([], [single]) => self.force(&single.0, single.1),
            ([], many) => self.at_least_one(many),
            // Body unsatisfiable when all guards active ⇒ some guard inactive.
            ([g], []) => self.force(&g.0, g.1),
            ([g], [single]) => {
                self.imply(&g.0, Self::ON + Self::OFF - g.1, &single.0, single.1);
            }
            ([g], many) => self.imply_choice(&g.0, Self::ON + Self::OFF - g.1, many),
            (guards, body) => {
                let mut lits = body.to_vec();
                lits.extend_from_slice(guards);
                self.at_least_one(&lits);
            }
        }
    }

    /// Gate `expr`'s constraints behind the guard context `ctx` (one negated
    /// literal per enclosing ceded conditional). Constraints bind only when
    /// every guard takes its active value, so an inactive guard leaves the
    /// body's flags free — no gratuitous flips. Nested ceded guards push their
    /// own literal, turning `a? ( b? ( c ) )` into the clause `¬a ∨ ¬b ∨ c`.
    fn guarded(&mut self, expr: &RequiredUse, ctx: &mut Vec<FreeLit>) {
        match expr {
            RequiredUse::All(children) => {
                for c in children {
                    self.guarded(c, ctx);
                }
            }
            RequiredUse::Flag { name, negated } => match self.operand(name, *negated) {
                Operand::Free {
                    node,
                    sat_ver,
                    prefer_ver,
                } => self.emit_clause(ctx, &[(node, sat_ver, prefer_ver)]),
                Operand::Fixed { satisfied: true } => {}
                // All guards active ⇒ a fixed flag would be violated, so some
                // guard must take its inactive value.
                Operand::Fixed { satisfied: false } => self.emit_clause(ctx, &[]),
            },
            RequiredUse::AnyOf(ch) => self.guarded_any(ch, ctx),
            RequiredUse::ExactlyOne(ch) => {
                self.guarded_any(ch, ctx);
                self.guarded_at_most(ch, ctx);
            }
            RequiredUse::AtMostOne(ch) => self.guarded_at_most(ch, ctx),
            RequiredUse::UseConditional {
                flag,
                negated,
                entries,
            } => match self.desired.get(*flag) {
                UseFlagState::SolverDecided { prefer } => {
                    let node = use_decision_package(self.cpn_str, flag);
                    let active_ver = if *negated { Self::OFF } else { Self::ON };
                    ctx.push((node, Self::ON + Self::OFF - active_ver, u64::from(prefer)));
                    for e in entries {
                        self.guarded(e, ctx);
                    }
                    ctx.pop();
                }
                state => {
                    let on = matches!(state, UseFlagState::Enabled);
                    if on != *negated {
                        for e in entries {
                            self.guarded(e, ctx);
                        }
                    }
                }
            },
        }
    }

    /// `all guards active ⇒ at least one of the bare-flag operands satisfied`.
    fn guarded_any(&mut self, children: &[RequiredUse], ctx: &[FreeLit]) {
        let Some(ops) = self.flag_operands(children) else {
            return; // non-flag operands deferred
        };
        if ops
            .iter()
            .any(|o| matches!(o, Operand::Fixed { satisfied: true }))
        {
            return; // already satisfied regardless of the guards
        }
        let free: Vec<FreeLit> = free_operands(&ops);
        self.emit_clause(ctx, &free);
    }

    /// `all guards active ⇒ at most one of the bare-flag operands satisfied`.
    fn guarded_at_most(&mut self, children: &[RequiredUse], ctx: &[FreeLit]) {
        let Some(ops) = self.flag_operands(children) else {
            return;
        };
        let fixed_sat = ops
            .iter()
            .filter(|o| matches!(o, Operand::Fixed { satisfied: true }))
            .count();
        if fixed_sat >= 2 {
            // Two fixed-on operands ⇒ unsatisfiable when active ⇒ guard escape.
            self.emit_clause(ctx, &[]);
            return;
        }
        let free: Vec<FreeLit> = free_operands(&ops);
        if fixed_sat == 1 {
            // The fixed-on operand is "the" one; all free must be off when active.
            for lit in &free {
                self.emit_clause(ctx, &[Self::negate(lit)]);
            }
        } else {
            for i in 0..free.len() {
                for j in (i + 1)..free.len() {
                    // guards active ⇒ (¬free[i] ∨ ¬free[j]); each negated literal
                    // keeps its own preference so the choice can avoid a flip.
                    let pair = [Self::negate(&free[i]), Self::negate(&free[j])];
                    self.emit_clause(ctx, &pair);
                }
            }
        }
    }

    /// Register a `Choice` node (one branch per operand) that is pulled into the
    /// solve *only* when `guard@gv` is selected, by attaching it to the guard's
    /// version bucket rather than the package's always-on requirements. Branches
    /// are ordered preference-satisfied-first (as in `at_least_one`).
    fn imply_choice(&mut self, guard: &PortagePackage, gv: u64, ops: &[FreeLit]) {
        let ordered = Self::order_by_preference(ops);
        let id = next_choice_id();
        let pkg = PortagePackage::choice(id);
        let n = ordered.len();
        let mut versions = Vec::with_capacity(n);
        for (i, (node, sat_ver)) in ordered.iter().enumerate() {
            versions.push((
                Self::ver((n - i) as u64),
                vec![Self::singleton(node, *sat_ver)],
            ));
            self.touched.insert(node.clone());
        }
        self.virtual_choices.push(VirtualChoice {
            package: pkg.clone(),
            versions,
            branch_use_deps: Vec::new(),
            branch_blockers: Vec::new(),
        });
        // Pull the choice when the guard is active (and only then).
        self.touched.insert(guard.clone());
        let bucket = if gv == Self::ON {
            self.node_on.entry(guard.clone()).or_default()
        } else {
            self.node_off.entry(guard.clone()).or_default()
        };
        bucket.push((pkg, PortageVersionSet::any(), None));
    }

    fn finish(mut self) -> RequiredUseEncoding {
        // Materialise every touched node as a two-version UseDecision virtual,
        // carrying any implication/exclusion deps, and pull it into the package.
        let nodes: Vec<PortagePackage> = self.touched.iter().cloned().collect();
        for node in nodes {
            let on = self.node_on.remove(&node).unwrap_or_default();
            let off = self.node_off.remove(&node).unwrap_or_default();
            self.virtual_choices.push(VirtualChoice {
                package: node.clone(),
                versions: vec![(Self::ver(Self::OFF), off), (Self::ver(Self::ON), on)],
                branch_use_deps: Vec::new(),
                branch_blockers: Vec::new(),
            });
            self.requirements
                .push((node, PortageVersionSet::any(), None));
        }
        RequiredUseEncoding {
            requirements: self.requirements,
            virtual_choices: self.virtual_choices,
        }
    }
}

enum Group {
    Any,
    Exactly,
    AtMost,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_slots() -> SlotMap {
        HashMap::new()
    }

    #[test]
    fn convert_simple_atom() {
        let config = UseConfig::new();
        let entries = DepEntry::parse("dev-libs/openssl").unwrap();
        let result = convert_deps(&entries, "test/pkg", &config, &empty_slots());
        assert_eq!(result.requirements.len(), 1);
        assert_eq!(result.requirements[0].0.to_string(), "dev-libs/openssl");
        assert!(result.requirements[0].1.is_full());
    }

    #[test]
    fn convert_versioned_atom() {
        let config = UseConfig::new();
        let entries = DepEntry::parse(">=dev-libs/openssl-3.0.0").unwrap();
        let result = convert_deps(&entries, "test/pkg", &config, &empty_slots());
        assert_eq!(result.requirements.len(), 1);
        assert!(
            result.requirements[0]
                .1
                .contains(&portage_atom::Version::parse("3.0.0").unwrap())
        );
        assert!(
            result.requirements[0]
                .1
                .contains(&portage_atom::Version::parse("3.1.0").unwrap())
        );
        assert!(
            !result.requirements[0]
                .1
                .contains(&portage_atom::Version::parse("2.9.9").unwrap())
        );
    }

    #[test]
    fn convert_eager_use_enabled() {
        let mut config = UseConfig::new();
        config.enable(Interned::intern("ssl"));
        let entries = DepEntry::parse("ssl? ( dev-libs/openssl )").unwrap();
        let result = convert_deps(&entries, "test/pkg", &config, &empty_slots());
        assert_eq!(result.requirements.len(), 1);
    }

    #[test]
    fn convert_eager_use_disabled() {
        let config = UseConfig::new();
        let entries = DepEntry::parse("ssl? ( dev-libs/openssl )").unwrap();
        let result = convert_deps(&entries, "test/pkg", &config, &empty_slots());
        assert!(result.requirements.is_empty());
    }

    #[test]
    fn convert_blocker_collected() {
        let config = UseConfig::new();
        let entries = DepEntry::parse("!dev-libs/openssl-compat").unwrap();
        let result = convert_deps(&entries, "test/pkg", &config, &empty_slots());
        assert_eq!(result.blockers.len(), 1);
    }

    #[test]
    fn convert_any_of_creates_virtual_choice() {
        let config = UseConfig::new();
        let entries = DepEntry::parse("|| ( dev-libs/openssl dev-libs/libressl )").unwrap();
        let result = convert_deps(&entries, "test/pkg", &config, &empty_slots());

        // Should have 1 requirement: the virtual choice package
        assert_eq!(result.requirements.len(), 1);
        assert!(
            result.requirements[0]
                .0
                .to_string()
                .starts_with("__internal__/choice_")
        );

        // Should have 1 virtual choice with 2 versions (one per alternative)
        assert_eq!(result.virtual_choices.len(), 1);
        let vc = &result.virtual_choices[0];
        assert_eq!(vc.versions.len(), 2);

        // Version 0 deps = openssl, version 1 deps = libressl
        let v0_deps = &vc.versions[0].1;
        let v1_deps = &vc.versions[1].1;
        assert_eq!(v0_deps.len(), 1);
        assert_eq!(v1_deps.len(), 1);
        assert!(v0_deps[0].0.to_string().contains("openssl"));
        assert!(v1_deps[0].0.to_string().contains("libressl"));
    }

    #[test]
    fn convert_at_most_one_adds_empty_version() {
        let config = UseConfig::new();
        let entries = DepEntry::parse("?? ( dev-libs/openssl dev-libs/libressl )").unwrap();
        let result = convert_deps(&entries, "test/pkg", &config, &empty_slots());

        assert_eq!(result.virtual_choices.len(), 1);
        let vc = &result.virtual_choices[0];
        // 3 versions: 0 (empty), 1 (openssl), 2 (libressl)
        assert_eq!(vc.versions.len(), 3);
        assert!(vc.versions[0].1.is_empty());
    }

    #[test]
    fn convert_solver_decided_use_creates_virtual() {
        let mut config = UseConfig::new();
        config.solver_decide(Interned::intern("ssl"), false);
        let entries = DepEntry::parse("ssl? ( dev-libs/openssl )").unwrap();
        let result = convert_deps(&entries, "test/pkg", &config, &empty_slots());

        // Should have 1 requirement: the virtual USE package
        assert_eq!(result.requirements.len(), 1);
        assert!(
            result.requirements[0]
                .0
                .to_string()
                .starts_with("__internal__/USE_")
        );

        // Virtual has 2 versions: 0 (off, empty), 1 (on, openssl)
        assert_eq!(result.virtual_choices.len(), 1);
        let vc = &result.virtual_choices[0];
        assert_eq!(vc.versions.len(), 2);
        assert!(vc.versions[0].1.is_empty()); // off
        assert_eq!(vc.versions[1].1.len(), 1); // on → openssl
    }

    #[test]
    fn convert_single_slot_uses_direct_dep() {
        let config = UseConfig::new();
        let entries = DepEntry::parse("dev-libs/openssl").unwrap();
        let openssl_cpn = portage_atom::Cpn::parse("dev-libs/openssl").unwrap();
        let mut slots = HashMap::new();
        slots.insert(
            openssl_cpn,
            vec![(
                Interned::intern("0"),
                PortagePackage::slotted(openssl_cpn, Interned::intern("0")),
            )],
        );
        let result = convert_deps(&entries, "test/pkg", &config, &slots);
        assert_eq!(result.requirements.len(), 1);
        assert_eq!(result.requirements[0].0.to_string(), "dev-libs/openssl:0");
        assert!(result.virtual_choices.is_empty());
    }

    #[test]
    fn convert_multi_slot_creates_choice() {
        let config = UseConfig::new();
        let entries = DepEntry::parse("dev-lang/python").unwrap();
        let py_cpn = portage_atom::Cpn::parse("dev-lang/python").unwrap();
        let mut slots = HashMap::new();
        slots.insert(
            py_cpn,
            vec![
                (
                    Interned::intern("3.11"),
                    PortagePackage::slotted(py_cpn, Interned::intern("3.11")),
                ),
                (
                    Interned::intern("3.12"),
                    PortagePackage::slotted(py_cpn, Interned::intern("3.12")),
                ),
            ],
        );
        let result = convert_deps(&entries, "test/pkg", &config, &slots);
        assert_eq!(result.requirements.len(), 1);
        assert!(
            result.requirements[0]
                .0
                .to_string()
                .starts_with("__internal__/slot_")
        );
        assert_eq!(result.virtual_choices.len(), 1);
        assert_eq!(result.virtual_choices[0].versions.len(), 2);
    }

    #[test]
    fn slot_choice_numbers_lex_last_slot_highest() {
        let config = UseConfig::new();
        let entries = DepEntry::parse("sys-devel/gcc:*").unwrap();
        let gcc_cpn = portage_atom::Cpn::parse("sys-devel/gcc").unwrap();
        let mut slots = HashMap::new();
        slots.insert(
            gcc_cpn,
            vec![
                (
                    Interned::intern("11"),
                    PortagePackage::slotted(gcc_cpn, Interned::intern("11")),
                ),
                (
                    Interned::intern("16"),
                    PortagePackage::slotted(gcc_cpn, Interned::intern("16")),
                ),
            ],
        );
        let result = convert_deps(&entries, "test/pkg", &config, &slots);
        let versions: Vec<_> = result.virtual_choices[0]
            .versions
            .iter()
            .map(|(v, reqs)| (v.clone(), reqs[0].0.clone()))
            .collect();
        assert_eq!(versions[0].0, Version::new(&[1]));
        assert_eq!(versions[1].0, Version::new(&[2]));
        assert_eq!(
            versions[1].1,
            PortagePackage::slotted(gcc_cpn, Interned::intern("16"))
        );
    }

    #[test]
    fn convert_slot_operator_any_slot() {
        let config = UseConfig::new();
        let entries = DepEntry::parse("dev-libs/openssl:*").unwrap();
        let openssl_cpn = portage_atom::Cpn::parse("dev-libs/openssl").unwrap();
        let mut slots = HashMap::new();
        slots.insert(
            openssl_cpn,
            vec![(
                Interned::intern("0"),
                PortagePackage::slotted(openssl_cpn, Interned::intern("0")),
            )],
        );
        let result = convert_deps(&entries, "test/pkg", &config, &slots);
        assert_eq!(result.requirements.len(), 1);
        assert_eq!(result.requirements[0].0.to_string(), "dev-libs/openssl:0");
    }

    #[test]
    fn convert_slot_operator_equal() {
        let config = UseConfig::new();
        let entries = DepEntry::parse("dev-libs/openssl:=").unwrap();
        let openssl_cpn = portage_atom::Cpn::parse("dev-libs/openssl").unwrap();
        let mut slots = HashMap::new();
        slots.insert(
            openssl_cpn,
            vec![(
                Interned::intern("0"),
                PortagePackage::slotted(openssl_cpn, Interned::intern("0")),
            )],
        );
        let result = convert_deps(&entries, "test/pkg", &config, &slots);
        assert_eq!(result.requirements.len(), 1);
        assert_eq!(result.requirements[0].0.to_string(), "dev-libs/openssl:0");
    }

    #[test]
    fn convert_named_slot_with_operator() {
        let config = UseConfig::new();
        let entries = DepEntry::parse("dev-libs/openssl:0=").unwrap();
        let result = convert_deps(&entries, "test/pkg", &config, &empty_slots());
        assert_eq!(result.requirements.len(), 1);
        assert_eq!(result.requirements[0].0.to_string(), "dev-libs/openssl:0");
    }

    #[test]
    fn convert_use_dep_collected() {
        let config = UseConfig::new();
        let entries = DepEntry::parse("dev-libs/openssl[ssl]").unwrap();
        let result = convert_deps(&entries, "test/pkg", &config, &empty_slots());
        assert_eq!(result.requirements.len(), 1);
        assert_eq!(result.use_deps.len(), 1);
        assert_eq!(result.use_deps[0].use_deps.len(), 1);
        assert_eq!(result.use_deps[0].use_deps[0].flag.as_str(), "ssl");
    }

    #[test]
    fn convert_use_dep_disabled_collected() {
        let config = UseConfig::new();
        let entries = DepEntry::parse("dev-libs/openssl[-debug]").unwrap();
        let result = convert_deps(&entries, "test/pkg", &config, &empty_slots());
        assert_eq!(result.use_deps.len(), 1);
        assert_eq!(
            result.use_deps[0].use_deps[0].kind,
            portage_atom::UseDepKind::Disabled
        );
    }

    #[test]
    fn convert_use_dep_multiple_collected() {
        let config = UseConfig::new();
        let entries = DepEntry::parse("dev-libs/openssl[ssl,-debug]").unwrap();
        let result = convert_deps(&entries, "test/pkg", &config, &empty_slots());
        assert_eq!(result.use_deps.len(), 1);
        assert_eq!(result.use_deps[0].use_deps.len(), 2);
    }

    #[test]
    fn convert_no_use_deps() {
        let config = UseConfig::new();
        let entries = DepEntry::parse("dev-libs/openssl").unwrap();
        let result = convert_deps(&entries, "test/pkg", &config, &empty_slots());
        assert!(result.use_deps.is_empty());
    }

    #[test]
    fn convert_blocker_in_or_group_stays_on_branch() {
        let config = UseConfig::new();
        let entries = DepEntry::parse("|| ( dev-libs/a !dev-libs/b )").unwrap();
        let result = convert_deps(&entries, "test/pkg", &config, &empty_slots());
        assert!(
            result.blockers.is_empty(),
            "blocker inside || () must not promote to the parent package"
        );
        let vc = result
            .virtual_choices
            .iter()
            .find(|v| !v.branch_blockers.is_empty())
            .expect("choice virtual with branch blockers");
        let all: Vec<_> = vc
            .branch_blockers
            .iter()
            .flat_map(|(_, bs)| bs.iter())
            .collect();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].cpn.package.as_str(), "b");
    }

    #[test]
    fn convert_blocker_in_solver_decided_use_stays_on_branch() {
        let mut config = UseConfig::new();
        config.solver_decide(Interned::intern("ssl"), false);
        let entries = DepEntry::parse("ssl? ( !dev-libs/openssl-compat )").unwrap();
        let result = convert_deps(&entries, "test/pkg", &config, &empty_slots());
        assert!(
            result.blockers.is_empty(),
            "blocker inside solver-decided USE must not promote to the parent"
        );
        let vc = result
            .virtual_choices
            .iter()
            .find(|v| v.branch_blockers.iter().any(|(_, bs)| !bs.is_empty()))
            .expect("UseDecision virtual with branch blockers");
        // Flag ON (version 1) carries the blocker; OFF does not.
        let on = vc
            .branch_blockers
            .iter()
            .find(|(v, _)| v == &Version::new(&[1]))
            .expect("ON branch");
        assert_eq!(on.1.len(), 1);
        assert_eq!(on.1[0].cpn.package.as_str(), "openssl-compat");
        let off = vc
            .branch_blockers
            .iter()
            .find(|(v, _)| v == &Version::new(&[0]))
            .expect("OFF branch");
        assert!(off.1.is_empty());
    }

    // IUSE defaults are folded into the desired config by the caller (in
    // production, via `portage_solver::resolve_effective_use`) before
    // conversion; convert then reads the resolved state. These tests set the
    // already-folded result directly — a real `+`/`-` IUSE default with
    // nothing else configuring the flag is indistinguishable, at this layer,
    // from `resolve_effective_use`'s own output, since both just mean "this
    // flag ended up on/off".
    #[test]
    fn convert_iuse_default_enabled() {
        let mut config = UseConfig::new();
        config.enable(Interned::intern("ssl")); // resolved: +ssl default, nothing else set
        let entries = DepEntry::parse("ssl? ( dev-libs/openssl )").unwrap();
        let result = convert_deps(&entries, "test/pkg", &config, &empty_slots());
        assert_eq!(
            result.requirements.len(),
            1,
            "ssl? should include deps when IUSE default is +ssl and config is unset"
        );
    }

    #[test]
    fn convert_iuse_default_disabled() {
        let config = UseConfig::new(); // resolved: -ssl default (or unset), same effective state
        let entries = DepEntry::parse("ssl? ( dev-libs/openssl )").unwrap();
        let result = convert_deps(&entries, "test/pkg", &config, &empty_slots());
        assert!(
            result.requirements.is_empty(),
            "ssl? should skip deps when IUSE default is -ssl and config is unset"
        );
    }

    #[test]
    fn convert_iuse_default_overridden_by_config() {
        let mut config = UseConfig::new();
        config.disable(Interned::intern("ssl")); // explicit -ssl wins over a +ssl IUSE default
        let entries = DepEntry::parse("ssl? ( dev-libs/openssl )").unwrap();
        let result = convert_deps(&entries, "test/pkg", &config, &empty_slots());
        assert!(
            result.requirements.is_empty(),
            "explicit config should override IUSE default"
        );
    }

    #[test]
    fn convert_negated_use_conditional() {
        let mut config = UseConfig::new();
        config.disable(Interned::intern("ssl"));
        let entries = DepEntry::parse("!ssl? ( dev-libs/openssl )").unwrap();
        let result = convert_deps(&entries, "test/pkg", &config, &empty_slots());
        assert_eq!(
            result.requirements.len(),
            1,
            "!ssl? with ssl disabled should include deps"
        );

        let mut config2 = UseConfig::new();
        config2.enable(Interned::intern("ssl"));
        let result2 = convert_deps(&entries, "test/pkg", &config2, &empty_slots());
        assert!(
            result2.requirements.is_empty(),
            "!ssl? with ssl enabled should skip deps"
        );
    }

    #[test]
    fn convert_exactly_one_of_creates_choice() {
        let config = UseConfig::new();
        let entries = DepEntry::parse("^^ ( dev-libs/a dev-libs/b )").unwrap();
        let result = convert_deps(&entries, "test/pkg", &config, &empty_slots());
        assert_eq!(result.virtual_choices.len(), 1);
        assert_eq!(result.virtual_choices[0].versions.len(), 2);
    }

    #[test]
    fn convert_empty_input() {
        let config = UseConfig::new();
        let result = convert_deps(&[], "test/pkg", &config, &empty_slots());
        assert!(result.requirements.is_empty());
        assert!(result.blockers.is_empty());
        assert!(result.virtual_choices.is_empty());
    }

    #[test]
    fn convert_repo_constraint_collected() {
        let config = UseConfig::new();
        let entries = DepEntry::parse("dev-libs/openssl::gentoo").unwrap();
        let result = convert_deps(&entries, "test/pkg", &config, &empty_slots());
        assert_eq!(result.repo_constraints.len(), 1);
        assert_eq!(result.repo_constraints[0].repo.as_str(), "gentoo");
    }

    #[test]
    fn convert_strictly_greater_atom() {
        let config = UseConfig::new();
        let entries = DepEntry::parse(">dev-libs/openssl-3.0.0").unwrap();
        let result = convert_deps(&entries, "test/pkg", &config, &empty_slots());
        assert_eq!(result.requirements.len(), 1);
        assert!(
            !result.requirements[0]
                .1
                .contains(&Version::parse("3.0.0").unwrap())
        );
        assert!(
            result.requirements[0]
                .1
                .contains(&Version::parse("3.0.1").unwrap())
        );
    }

    #[test]
    fn convert_less_or_equal_atom() {
        let config = UseConfig::new();
        let entries = DepEntry::parse("<=dev-libs/openssl-3.0.0").unwrap();
        let result = convert_deps(&entries, "test/pkg", &config, &empty_slots());
        assert_eq!(result.requirements.len(), 1);
        assert!(
            result.requirements[0]
                .1
                .contains(&Version::parse("3.0.0").unwrap())
        );
        assert!(
            result.requirements[0]
                .1
                .contains(&Version::parse("2.9.9").unwrap())
        );
        assert!(
            !result.requirements[0]
                .1
                .contains(&Version::parse("3.0.1").unwrap())
        );
    }

    #[test]
    fn convert_bare_slot_operator_equals_collected() {
        let config = UseConfig::new();
        let entries = DepEntry::parse("dev-libs/openssl:=").unwrap();
        let openssl_cpn = portage_atom::Cpn::parse("dev-libs/openssl").unwrap();
        let mut slots = HashMap::new();
        slots.insert(
            openssl_cpn,
            vec![(
                Interned::intern("0"),
                PortagePackage::slotted(openssl_cpn, Interned::intern("0")),
            )],
        );
        let result = convert_deps(&entries, "test/pkg", &config, &slots);
        assert_eq!(result.slot_operator_deps.len(), 1);
        assert_eq!(result.slot_operator_deps[0].operator, SlotOperator::Equal);
        assert!(result.slot_operator_deps[0].slot.is_none());
    }

    #[test]
    fn convert_named_slot_operator_equals_collected() {
        let config = UseConfig::new();
        let entries = DepEntry::parse("dev-libs/openssl:0=").unwrap();
        let result = convert_deps(&entries, "test/pkg", &config, &empty_slots());
        assert_eq!(result.slot_operator_deps.len(), 1);
        assert_eq!(result.slot_operator_deps[0].operator, SlotOperator::Equal);
        assert_eq!(
            result.slot_operator_deps[0].slot,
            Some(Interned::intern("0"))
        );
    }

    #[test]
    fn convert_slot_star_not_collected() {
        let config = UseConfig::new();
        let entries = DepEntry::parse("dev-libs/openssl:*").unwrap();
        let result = convert_deps(&entries, "test/pkg", &config, &empty_slots());
        assert!(
            result.slot_operator_deps.is_empty(),
            ":* should not produce slot-operator dep"
        );
    }
}
