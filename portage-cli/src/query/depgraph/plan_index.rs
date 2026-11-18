//! Plan positions that tell apart the two builds of a package split to open a dependency cycle

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom_pubgrub::{DepEdge, PortagePackage, SplitBuild};

/// Where each package sits in a plan, keyed by package (slot and merge root included)
pub(super) struct PlanIndex<'a> {
    first: HashMap<&'a PortagePackage, usize>,
    /// `(flags-off build, rebuild, flags)` for each split package the plan lists twice
    splits: Vec<(usize, usize, &'a [Interned<DefaultInterner>])>,
}

impl<'a> PlanIndex<'a> {
    pub(super) fn new(
        plan: impl Iterator<Item = &'a PortagePackage>,
        splits: &'a [SplitBuild],
    ) -> Self {
        let mut first = HashMap::new();
        let mut second = HashMap::new();
        for (i, pkg) in plan.enumerate() {
            match first.entry(pkg) {
                Entry::Vacant(slot) => {
                    slot.insert(i);
                }
                Entry::Occupied(_) => {
                    second.entry(pkg).or_insert(i);
                }
            }
        }
        let splits = splits
            .iter()
            .filter_map(|s| {
                let pkg = &s.package.0;
                Some((*first.get(pkg)?, *second.get(pkg)?, s.flags.as_slice()))
            })
            .collect();
        Self { first, splits }
    }

    /// The package's position; its first build when it is listed twice
    pub(super) fn get(&self, pkg: &PortagePackage) -> Option<usize> {
        self.first.get(pkg).copied()
    }

    /// `(from, to)` positions of `edge`
    ///
    /// A split package's flag-gated edges belong to its rebuild, and so do dependents placed after the rebuild.
    pub(super) fn endpoints(&self, edge: &DepEdge) -> Option<(usize, usize)> {
        let mut from = self.get(&edge.from.0)?;
        let mut to = self.get(&edge.to.0)?;
        for &(flags_off, rebuild, flags) in &self.splits {
            if from == flags_off && edge.via_use_flag.is_some_and(|f| flags.contains(&f)) {
                from = rebuild;
            }
            if to == flags_off && from > rebuild {
                to = rebuild;
            }
        }
        Some((from, to))
    }

    /// The flags the entry at `i` is built without, when it is a split package's first build
    pub(super) fn flags_off(&self, i: usize) -> Option<&'a [Interned<DefaultInterner>]> {
        self.splits.iter().find(|s| s.0 == i).map(|s| s.2)
    }

    /// Whether the entry at `i` is a split package's rebuild
    pub(super) fn is_rebuild(&self, i: usize) -> bool {
        self.splits.iter().any(|s| s.1 == i)
    }

    /// `(flags-off build, rebuild)` positions of every split package
    pub(super) fn split_pairs(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.splits.iter().map(|s| (s.0, s.1))
    }
}
