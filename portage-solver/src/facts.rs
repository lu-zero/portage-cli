//! Solver-agnostic package facts vocabulary.
//!
//! These types describe the *facts* a [`PackageRepository`] hands a solver:
//! per-version slot/subslot/repo/IUSE/dependencies/`REQUIRED_USE`, plus the
//! resolved per-version *policy* via [`PackageRepository::desired_use`]. The
//! solver computes the *needed* set and never resolves policy.

use std::collections::HashMap;

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, DepEntry, Version};

use crate::{RequiredUse, UseConfig};

/// Default state for an IUSE flag, from the `+`/`-` prefix in IUSE.
///
/// See [PMS 7.2](https://projects.gentoo.org/pms/9/pms.html#mandatory-ebuilddefined-variables).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IUseDefault {
    /// `+flag` — enabled by default.
    Enabled,
    /// `-flag` — disabled by default.
    Disabled,
}

/// Structured dependency trees separated by PMS class.
#[derive(Clone, Debug, Default)]
pub struct PackageDeps {
    /// DEPEND — build-time dependencies.
    pub depend: Vec<DepEntry>,
    /// RDEPEND — runtime dependencies.
    pub rdepend: Vec<DepEntry>,
    /// BDEPEND — build-host dependencies (EAPI 7+).
    pub bdepend: Vec<DepEntry>,
    /// PDEPEND — post-merge dependencies.
    pub pdepend: Vec<DepEntry>,
    /// IDEPEND — install-time dependencies (EAPI 8+).
    pub idepend: Vec<DepEntry>,
}

impl PackageDeps {
    /// Iterate over all dependency classes and their entries, skipping empty
    /// classes. Yields in canonical class order (DEPEND, RDEPEND, BDEPEND,
    /// PDEPEND, IDEPEND).
    pub fn iter_classes(&self) -> impl Iterator<Item = (DepClass, &[DepEntry])> {
        [
            (DepClass::Depend, self.depend.as_slice()),
            (DepClass::Rdepend, self.rdepend.as_slice()),
            (DepClass::Bdepend, self.bdepend.as_slice()),
            (DepClass::Pdepend, self.pdepend.as_slice()),
            (DepClass::Idepend, self.idepend.as_slice()),
        ]
        .into_iter()
        .filter(|(_, entries)| !entries.is_empty())
    }
}

/// PMS dependency class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DepClass {
    /// `DEPEND` — build-time.
    Depend,
    /// `RDEPEND` — runtime.
    Rdepend,
    /// `BDEPEND` — build host (cross-compilation).
    Bdepend,
    /// `PDEPEND` — post-merge.
    Pdepend,
    /// `IDEPEND` — install-time.
    Idepend,
}

impl std::fmt::Display for DepClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DepClass::Depend => write!(f, "DEPEND"),
            DepClass::Rdepend => write!(f, "RDEPEND"),
            DepClass::Bdepend => write!(f, "BDEPEND"),
            DepClass::Pdepend => write!(f, "PDEPEND"),
            DepClass::Idepend => write!(f, "IDEPEND"),
        }
    }
}

/// Metadata for a single ebuild version, including its dependency trees.
///
/// Solver-agnostic equivalent of `portage-atom-pubgrub::PackageVersions`.
#[derive(Clone, Debug)]
pub struct VersionFacts {
    /// Slot name for this version.
    pub slot: Option<Interned<DefaultInterner>>,
    /// Subslot name for this version.
    pub subslot: Option<Interned<DefaultInterner>>,
    /// Repository this version comes from.
    pub repo: Option<Interned<DefaultInterner>>,
    /// IUSE flags for this version (USE flags the package defines).
    pub iuse: Vec<Interned<DefaultInterner>>,
    /// IUSE default states — flags prefixed with `+` in IUSE default to enabled.
    pub iuse_defaults: HashMap<Interned<DefaultInterner>, IUseDefault>,
    /// Dependency trees by class.
    pub deps: PackageDeps,
    /// `REQUIRED_USE` constraint (a *fact*, not policy). `None` when the ebuild
    /// declares no `REQUIRED_USE`.
    pub required_use: Option<RequiredUse>,
}

/// Trait for a package repository that the solver can query.
///
/// Implementations provide package metadata sourced from ebuild caches, and
/// the fully-resolved per-version desired USE (the caller's policy). The solver
/// consumes [`Self::desired_use`]; it never resolves policy itself.
///
/// See [PMS 7](https://projects.gentoo.org/pms/9/pms.html#mandatory-ebuilddefined-variables).
pub trait PackageRepository {
    /// Return all packages in the repository.
    fn all_packages(&self) -> Vec<Cpn>;

    /// Return all versions for a given CPN, with their metadata.
    fn versions_for(&self, cpn: &Cpn) -> Vec<(Cpv, VersionFacts)>;

    /// The resolved **desired** USE state for a specific version.
    ///
    /// This is the caller's policy fully resolved — global USE (profile +
    /// `make.conf`), `package.use` overrides, and the ebuild's IUSE defaults
    /// all folded into one config. The solver consumes this as-is.
    fn desired_use(&self, cpv: &Cpv) -> UseConfig;

    /// The slots of `cpn`'s (filtered) versions, **ordered by each slot's best
    /// (newest) available version, ascending** — so the slot holding the newest
    /// version sorts last. This mirrors portage's version-descending selection
    /// for `:*` deps (a lexicographic slot-name order would wrongly prefer an
    /// older compat slot like `app-shells/bash:5.1` over `:0`).
    ///
    /// The default projection rebuilds this from [`Self::versions_for`];
    /// implementations whose `versions_for` is expensive should override with a
    /// direct metadata read applying the same version filters.
    fn slots_for(&self, cpn: &Cpn) -> Vec<Interned<DefaultInterner>> {
        let mut best: HashMap<Interned<DefaultInterner>, Version> = HashMap::new();
        for (cpv, meta) in self.versions_for(cpn) {
            if let Some(slot) = meta.slot {
                best.entry(slot)
                    .and_modify(|v| {
                        if cpv.version > *v {
                            *v = cpv.version.clone();
                        }
                    })
                    .or_insert(cpv.version);
            }
        }
        rank_slots_by_version(best)
    }
}

/// Order slots by their best version (ascending; the newest-version slot sorts
/// last). The comparison is total without a tie-break: each cpv is unique and
/// lives in exactly one slot, so two distinct slots can never share a best
/// `Version`.
pub fn rank_slots_by_version(
    best: HashMap<Interned<DefaultInterner>, Version>,
) -> Vec<Interned<DefaultInterner>> {
    let mut slots: Vec<(Interned<DefaultInterner>, Version)> = best.into_iter().collect();
    slots.sort_by(|a, b| a.1.cmp(&b.1));
    slots.into_iter().map(|(s, _)| s).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use portage_atom::{Cpv, Dep};

    fn v(s: &str) -> Version {
        Cpv::parse(s).unwrap().version
    }

    #[test]
    fn rank_slots_by_version_ascending() {
        let mut best = HashMap::new();
        best.insert(Interned::intern("0"), v("dev-libs/a-5.0"));
        best.insert(Interned::intern("5.1"), v("dev-libs/a-3.0"));
        let order = rank_slots_by_version(best);
        // 5.1 (best 3.0) sorts before 0 (best 5.0) — version, not name, wins.
        assert_eq!(order, vec![Interned::intern("5.1"), Interned::intern("0")]);
    }

    #[test]
    fn iter_classes_skips_empty() {
        let deps = PackageDeps {
            rdepend: vec![DepEntry::Atom(Dep::parse("dev-libs/x-1").unwrap())],
            ..PackageDeps::default()
        };
        let classes: Vec<_> = deps.iter_classes().map(|(c, _)| c).collect();
        assert_eq!(classes, vec![DepClass::Rdepend]);
    }

    #[test]
    fn depclass_display() {
        assert_eq!(DepClass::Depend.to_string(), "DEPEND");
        assert_eq!(DepClass::Bdepend.to_string(), "BDEPEND");
    }
}
