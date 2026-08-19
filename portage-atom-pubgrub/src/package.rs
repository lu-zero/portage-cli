//! Package identity types for the PubGrub solver.
//!
//! ## Solver-internal bookkeeping nodes
//!
//! PubGrub requires every dependency constraint to be expressed as a
//! *package + version range* pair, and it must start resolution from a single
//! root package.  Portage has no equivalent concepts, so we use enum variants
//! to distinguish real packages from synthetic solver nodes:
//!
//! | Variant | Purpose |
//! |---|---|
//! | `Real` | An actual Gentoo package (CPN + optional slot) |
//! | `Root` | Synthetic root; its "deps" are the caller's resolve targets |
//! | `UseDecision` | USE-flag decision node; version 1 = enabled, 0 = disabled |
//! | `Choice` | OR-group (`||`, `^^`, `??`) node; each version is one alternative |
//! | `SlotChoice` | Slot-star (`:*`) node; synthetic versions rise with slot order |
//!
//! Virtual variants exist only inside the solver and are **always stripped**
//! from [`PortageDependencyProvider::resolve_targets`] before the result is
//! returned.  No caller ever sees them.

use std::fmt;

use portage_atom::Cpn;
use portage_atom::interner::{DefaultInterner, Interned};

// `MergeRoot` is defined once in `portage-solver`. Solver nodes are keyed by
// `(CPN, slot, merge_root)` so the same CPV can appear twice under cross
// (native host tool + cross target runtime).
pub use portage_solver::MergeRoot;

/// A PubGrub-compatible package identifier.
///
/// Real packages carry a CPN and optional slot.  Virtual variants
/// (`Root`, `UseDecision`, `Choice`, `SlotChoice`) are solver-internal
/// bookkeeping nodes.
///
/// Implements `Clone + Eq + Hash + Debug + Display`, satisfying pubgrub's
/// `Package` trait via blanket implementation.
///
/// See [PMS 8.3.3](https://projects.gentoo.org/pms/9/pms.html#slot_deps).
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum PortagePackage {
    /// A real Gentoo package, identified by CPN + optional slot.
    Real {
        /// Category/package name.
        cpn: Cpn,
        /// Bound slot, or `None` for a slot-agnostic node.
        slot: Option<Interned<DefaultInterner>>,
        /// Merge destination; defaults to [`MergeRoot::Target`].
        merge_root: MergeRoot,
    },
    /// Synthetic root node for the solver.
    Root,
    /// USE-flag decision node.  Version 1 = enabled, 0 = disabled.
    UseDecision {
        /// Interned identifier of the decision node (`cpn` + flag).
        name: Interned<DefaultInterner>,
    },
    /// OR-group choice node.  Each version is one alternative.
    Choice {
        /// Unique id of the synthetic choice group (from a process-global
        /// counter — never repeats, so it's never interned: unlike
        /// `UseDecision`'s name, which recurs identically every USE-dep
        /// co-solve fixpoint iteration for the same (cpn, flag), a `Choice`
        /// id is minted once and never looked up again — interning it would
        /// only leak process memory for zero reuse benefit.
        id: u64,
    },
    /// Slot-choice node.  Each version selects one slot candidate.
    SlotChoice {
        /// Unique id of the synthetic slot-choice group. Same rationale as
        /// [`Choice::id`](Self::Choice) for not interning it.
        id: u64,
    },
}

impl PortagePackage {
    /// Create a real package from a CPN and optional slot.
    pub fn new(cpn: Cpn, slot: Option<Interned<DefaultInterner>>) -> Self {
        Self::Real {
            cpn,
            slot,
            merge_root: MergeRoot::Target,
        }
    }

    /// Create an unslotted real package at `merge_root`.
    pub fn unslotted(cpn: Cpn) -> Self {
        Self::unslotted_at(cpn, MergeRoot::Target)
    }

    /// Create an unslotted real package at `merge_root`.
    pub fn unslotted_at(cpn: Cpn, merge_root: MergeRoot) -> Self {
        Self::Real {
            cpn,
            slot: None,
            merge_root,
        }
    }

    /// Create a slotted real package at the target root.
    pub fn slotted(cpn: Cpn, slot: Interned<DefaultInterner>) -> Self {
        Self::slotted_at(cpn, slot, MergeRoot::Target)
    }

    /// Create a slotted real package at `merge_root`.
    pub fn slotted_at(cpn: Cpn, slot: Interned<DefaultInterner>, merge_root: MergeRoot) -> Self {
        Self::Real {
            cpn,
            slot: Some(slot),
            merge_root,
        }
    }

    /// Returns the merge root for real packages (target when virtual).
    pub fn merge_root(&self) -> MergeRoot {
        match self {
            Self::Real { merge_root, .. } => *merge_root,
            _ => MergeRoot::Target,
        }
    }

    /// Clone a real package at another merge root; virtual nodes are unchanged.
    pub fn at_merge_root(&self, merge_root: MergeRoot) -> Self {
        match self {
            Self::Real { cpn, slot, .. } => Self::Real {
                cpn: *cpn,
                slot: *slot,
                merge_root,
            },
            other => other.clone(),
        }
    }

    /// Create the solver-internal root node.
    pub(crate) fn synthetic_root() -> Self {
        Self::Root
    }

    /// Create a USE-flag decision node.
    pub(crate) fn use_decision(name: Interned<DefaultInterner>) -> Self {
        Self::UseDecision { name }
    }

    /// Create an OR-group choice node.
    pub(crate) fn choice(id: u64) -> Self {
        Self::Choice { id }
    }

    /// Create a slot-choice node.
    pub(crate) fn slot_choice(id: u64) -> Self {
        Self::SlotChoice { id }
    }

    /// `true` for **solver-internal** bookkeeping nodes only
    /// ([`Root`](Self::Root), [`UseDecision`](Self::UseDecision),
    /// [`Choice`](Self::Choice), [`SlotChoice`](Self::SlotChoice)).
    ///
    /// **Not** Gentoo category `virtual/*` packages (`virtual/libcrypt`, …):
    /// those are ordinary [`Real`](Self::Real) nodes and return `false`.
    /// Misreading this flag has already caused wrong live root-causes (e.g.
    /// blaming a depgraph edge filter for dropping `virtual/*` RDEPEND).
    pub fn is_virtual(&self) -> bool {
        !matches!(self, Self::Real { .. })
    }

    /// Returns the CPN for real packages.
    ///
    /// # Panics
    /// Panics if called on a virtual variant. Use [`Self::is_virtual`] to check first,
    /// or use [`Self::cpn_opt`] for a non-panicking version.
    pub fn cpn(&self) -> &Cpn {
        match self {
            Self::Real { cpn, .. } => cpn,
            _ => panic!(
                "cpn() called on virtual package {:?}; use is_virtual() to check or cpn_opt() for Option",
                self
            ),
        }
    }

    /// Returns the CPN for real packages, or None for virtual packages.
    ///
    /// This is the non-panicking version of [`Self::cpn`].
    pub fn cpn_opt(&self) -> Option<&Cpn> {
        match self {
            Self::Real { cpn, .. } => Some(cpn),
            _ => None,
        }
    }

    /// Returns the slot for real packages.
    pub fn slot(&self) -> Option<Interned<DefaultInterner>> {
        match self {
            Self::Real { slot, .. } => *slot,
            _ => None,
        }
    }

    /// Returns the CPN as a string for real packages.
    ///
    /// # Panics
    /// Panics if called on a virtual variant. Use [`Self::is_virtual`] to check first.
    pub fn cpn_str(&self) -> String {
        self.cpn().to_string()
    }

    /// Returns the CPN as a string for real packages, or None for virtual packages.
    pub fn cpn_str_opt(&self) -> Option<String> {
        self.cpn_opt().map(|c| c.to_string())
    }
}

impl Ord for PortagePackage {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match (self, other) {
            (
                Self::Real {
                    cpn: a_cpn,
                    slot: a_slot,
                    merge_root: a_root,
                },
                Self::Real {
                    cpn: b_cpn,
                    slot: b_slot,
                    merge_root: b_root,
                },
            ) => a_cpn
                .cmp(b_cpn)
                .then_with(|| match (a_slot, b_slot) {
                    (Some(a), Some(b)) => a.as_str().cmp(b.as_str()),
                    (Some(_), None) => Ordering::Greater,
                    (None, Some(_)) => Ordering::Less,
                    (None, None) => Ordering::Equal,
                })
                .then(a_root.cmp(b_root)),
            (Self::Real { .. }, _) => Ordering::Less,
            (_, Self::Real { .. }) => Ordering::Greater,
            // Internal variants: order by discriminant, then by the node's
            // own identifier so `Ord` stays consistent with `Eq`/`Hash`
            // (otherwise distinct nodes collapse in a `BTreeSet`/`BTreeMap`).
            (Self::UseDecision { name: a }, Self::UseDecision { name: b }) => {
                a.as_str().cmp(b.as_str())
            }
            (Self::Choice { id: a }, Self::Choice { id: b })
            | (Self::SlotChoice { id: a }, Self::SlotChoice { id: b }) => a.cmp(b),
            _ => self.discriminant_ord().cmp(&other.discriminant_ord()),
        }
    }
}

impl PartialOrd for PortagePackage {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PortagePackage {
    fn discriminant_ord(&self) -> u8 {
        match self {
            Self::Real { .. } => 0,
            Self::Root => 1,
            Self::UseDecision { .. } => 2,
            Self::Choice { .. } => 3,
            Self::SlotChoice { .. } => 4,
        }
    }
}

impl fmt::Display for PortagePackage {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Real {
                cpn,
                slot: Some(slot),
                merge_root: MergeRoot::Target,
            } => write!(f, "{}:{}", cpn, slot),
            Self::Real {
                cpn,
                slot: Some(slot),
                merge_root: MergeRoot::Host,
            } => write!(f, "{}:{}@host", cpn, slot),
            Self::Real {
                cpn,
                slot: None,
                merge_root: MergeRoot::Target,
            } => write!(f, "{}", cpn),
            Self::Real {
                cpn,
                slot: None,
                merge_root: MergeRoot::Host,
            } => write!(f, "{}@host", cpn),
            Self::Root => write!(f, "__internal__/root"),
            Self::UseDecision { name } => write!(f, "__internal__/{}", name),
            Self::Choice { id } => write!(f, "__internal__/choice_{id}"),
            Self::SlotChoice { id } => write!(f, "__internal__/slot_{id}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_unslotted() {
        let cpn = Cpn::parse("dev-lang/rust").unwrap();
        let pkg = PortagePackage::unslotted(cpn);
        assert_eq!(pkg.to_string(), "dev-lang/rust");
    }

    #[test]
    fn display_slotted() {
        let cpn = Cpn::parse("dev-lang/python").unwrap();
        let slot = Interned::intern("3.12");
        let pkg = PortagePackage::slotted(cpn, slot);
        assert_eq!(pkg.to_string(), "dev-lang/python:3.12");
    }

    #[test]
    fn different_slots_are_different_packages() {
        let cpn = Cpn::parse("dev-lang/python").unwrap();
        let p1 = PortagePackage::slotted(cpn, Interned::intern("3.11"));
        let p2 = PortagePackage::slotted(
            Cpn::parse("dev-lang/python").unwrap(),
            Interned::intern("3.12"),
        );
        assert_ne!(p1, p2);
    }

    #[test]
    fn different_merge_roots_are_different_packages() {
        let cpn = Cpn::parse("dev-libs/foo").unwrap();
        let host = PortagePackage::unslotted_at(cpn, MergeRoot::Host);
        let target = PortagePackage::unslotted_at(cpn, MergeRoot::Target);
        assert_ne!(host, target);
        use std::cmp::Ordering;
        assert_ne!(host.cmp(&target), Ordering::Equal);
    }

    #[test]
    fn same_slot_is_same_package() {
        let cpn = Cpn::parse("dev-lang/python").unwrap();
        let p1 = PortagePackage::slotted(cpn, Interned::intern("3.12"));
        let p2 = PortagePackage::slotted(
            Cpn::parse("dev-lang/python").unwrap(),
            Interned::intern("3.12"),
        );
        assert_eq!(p1, p2);
    }

    #[test]
    fn real_sorts_before_virtual() {
        let cpn = Cpn::parse("dev-lang/rust").unwrap();
        let real = PortagePackage::unslotted(cpn);
        let root = PortagePackage::synthetic_root();
        assert!(real < root);
    }

    #[test]
    fn is_virtual() {
        let cpn = Cpn::parse("dev-lang/rust").unwrap();
        assert!(!PortagePackage::unslotted(cpn).is_virtual());
        assert!(PortagePackage::synthetic_root().is_virtual());
        assert!(PortagePackage::use_decision(Interned::intern("test")).is_virtual());
    }

    // `Ord` must be consistent with `Eq`/`Hash`: distinct internal nodes that
    // are unequal must not compare `Equal`, or a `BTreeSet`/`BTreeMap` keyed on
    // them silently collapses them into one (the bug behind ceded nested groups).
    #[test]
    fn internal_nodes_order_consistent_with_eq() {
        use std::cmp::Ordering;
        let a = PortagePackage::use_decision(Interned::intern("USE_a"));
        let b = PortagePackage::use_decision(Interned::intern("USE_b"));
        assert_ne!(a, b);
        assert_ne!(a.cmp(&b), Ordering::Equal, "distinct nodes must not tie");
        assert_eq!(a.cmp(&a), Ordering::Equal);

        // The set must retain every distinct node.
        let mut set = std::collections::BTreeSet::new();
        for n in ["x", "y", "z"] {
            set.insert(PortagePackage::use_decision(Interned::intern(n)));
        }
        assert_eq!(set.len(), 3, "BTreeSet dropped distinct UseDecision nodes");

        // Same for the other id-bearing variants.
        let c1 = PortagePackage::choice(1);
        let c2 = PortagePackage::choice(2);
        assert_ne!(c1.cmp(&c2), Ordering::Equal);
        let s1 = PortagePackage::slot_choice(1);
        let s2 = PortagePackage::slot_choice(2);
        assert_ne!(s1.cmp(&s2), Ordering::Equal);
    }
}
