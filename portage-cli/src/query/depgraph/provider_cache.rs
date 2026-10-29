//! Solver providers kept across the solve rounds of one resolve

use std::cell::RefCell;
use std::collections::HashSet;

use portage_atom::{Cpn, Dep};
use portage_atom_pubgrub::{PortageDependencyProvider, UseOverride};

/// What a provider was built from; two equal keys build the same provider
pub(super) struct ProviderKey {
    pub(super) package_use: Vec<(Dep, Vec<UseOverride>)>,
    pub(super) autosolve_use: bool,
    pub(super) widened: bool,
    pub(super) seeds: HashSet<Cpn>,
}

impl ProviderKey {
    /// `package.use` compares as a set: the co-solve appends its forced flags in hash order.
    fn matches(&self, other: &Self) -> bool {
        self.autosolve_use == other.autosolve_use
            && self.widened == other.widened
            && self.seeds == other.seeds
            && self.package_use.len() == other.package_use.len()
            && self
                .package_use
                .iter()
                .all(|e| other.package_use.contains(e))
    }
}

/// Providers already built this resolve, reused by a later solve with the same key
///
/// `resolve_targets` resets all per-solve state, so a provider can serve any number of solves.
#[derive(Default)]
pub(super) struct ProviderCache(RefCell<Vec<(ProviderKey, PortageDependencyProvider)>>);

impl ProviderCache {
    pub(super) fn take(&self, key: &ProviderKey) -> Option<PortageDependencyProvider> {
        let mut entries = self.0.borrow_mut();
        let index = entries.iter().position(|(k, _)| k.matches(key))?;
        Some(entries.swap_remove(index).1)
    }

    pub(super) fn put(&self, key: ProviderKey, provider: PortageDependencyProvider) {
        self.0.borrow_mut().push((key, provider));
    }
}
