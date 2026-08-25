use std::collections::HashMap;

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, DepList, Version};

use crate::required_use::RequiredUse;
use crate::use_config::UseConfig;

// `IUseDefault` (the `+`/`-` IUSE default state) is shared vocabulary, defined
// once in `portage-solver`.
pub use portage_solver::IUseDefault;

/// Metadata for a single ebuild version, including its dependency trees
#[derive(Clone)]
pub struct PackageVersions {
    /// Slot name for this version
    pub slot: Option<Interned<DefaultInterner>>,
    /// Subslot name for this version
    pub subslot: Option<Interned<DefaultInterner>>,
    /// Repository this version comes from
    pub repo: Option<Interned<DefaultInterner>>,
    /// IUSE flags for this version (USE flags the package defines)
    pub iuse: Vec<Interned<DefaultInterner>>,
    /// IUSE default states — flags prefixed with `+` in IUSE default to enabled
    pub iuse_defaults: HashMap<Interned<DefaultInterner>, IUseDefault>,
    /// Dependency trees by class
    pub deps: PackageDeps,
    /// `REQUIRED_USE` constraint, translated by the caller into the solver's
    /// interned-flag vocabulary ([`RequiredUse`])
    ///
    /// `None` when the ebuild declares no `REQUIRED_USE`. This is an
    /// intrinsic ebuild **fact**, not policy — as of Phase 0 it is stored
    /// but not yet consumed by the solver (Level-C auto-satisfaction is the
    /// future consumer — see `docs/required-use-level-c.md`).
    pub required_use: Option<RequiredUse>,
    /// PMS table 8.6: empty `||` / `^^` after USE strip matches (EAPI 0–6)
    pub empty_any_of_matches: bool,
    /// The version is outside the caller's keyword/mask/license acceptance and
    /// is offered only because the caller runs in widened (`--autounmask`)
    /// candidate-supply mode
    ///
    /// `false` for every normally-filtered repository. The solver prefers
    /// untagged candidates whenever one satisfies the range; a tagged
    /// selection is reported as an autounmask change after the solve.
    pub needs_unmask: bool,
}

/// Structured dependency trees separated by PMS class
///
/// Each class is a [`DepList`] (`Arc`-wrapped): a caller whose own
/// dependency-tree source is already a `DepList` can hand these off as a
/// cheap refcount bump instead of a deep clone. Matters because
/// `PackageRepository::versions_for` is called fresh for every package on
/// every provider rebuild (up to ~8x per invocation for USE-dep co-solve),
/// so hundreds of parsed atoms were being deep-cloned needlessly each time.
#[derive(Clone, Default)]
pub struct PackageDeps {
    /// DEPEND — build-time dependencies
    pub depend: DepList,
    /// RDEPEND — runtime dependencies
    pub rdepend: DepList,
    /// BDEPEND — build-host dependencies (EAPI 7+)
    pub bdepend: DepList,
    /// PDEPEND — post-merge dependencies
    pub pdepend: DepList,
    /// IDEPEND — install-time dependencies (EAPI 8+)
    pub idepend: DepList,
}

impl PackageDeps {
    /// Build from five dependency lists, converting each into a [`DepList`]
    /// (accepts a plain `Vec<DepEntry>`, an existing `DepList`, or anything
    /// else `Into<DepList>`)
    ///
    /// The ergonomic equivalent of the struct literal without repeating
    /// `.into()` on every field.
    pub fn new(
        depend: impl Into<DepList>,
        rdepend: impl Into<DepList>,
        bdepend: impl Into<DepList>,
        pdepend: impl Into<DepList>,
        idepend: impl Into<DepList>,
    ) -> Self {
        Self {
            depend: depend.into(),
            rdepend: rdepend.into(),
            bdepend: bdepend.into(),
            pdepend: pdepend.into(),
            idepend: idepend.into(),
        }
    }
}

/// Trait for a package repository that the solver can query
///
/// Implementations provide package metadata sourced from ebuild caches,
/// as described in [PMS 7](https://projects.gentoo.org/pms/9/pms.html#mandatory-ebuilddefined-variables).
pub trait PackageRepository {
    /// Return all packages in the repository
    fn all_packages(&self) -> Vec<Cpn>;

    /// Return all versions for a given CPN, with their metadata
    fn versions_for(&self, cpn: &Cpn) -> Vec<(Cpv, PackageVersions)>;

    /// The slots of `cpn`'s (filtered) versions, **ordered by each slot's
    /// best (newest) available version, ascending** — so the slot with the
    /// newest version sorts last, and `SlotChoice` numbering gives it the
    /// highest synthetic version so the solver's `max()` lands there
    ///
    /// Ordering by slot *name* instead would pick an older compat slot
    /// (mirrors portage's version-descending `:*` selection).
    ///
    /// A cheap whole-repository projection; override with a direct metadata
    /// read if `versions_for` is expensive.
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

    /// The resolved **desired** USE state for a specific version
    ///
    /// This is the caller's policy fully resolved — global USE (profile +
    /// `make.conf`), `package.use` overrides, and the ebuild's IUSE defaults all
    /// folded into one config.  The solver consumes this; it never resolves
    /// policy itself.  See `docs/use-and-solver-boundary.md`.
    fn desired_use(&self, cpv: &Cpv) -> UseConfig;
}

/// Order slots by their best version (ascending; the newest-version slot
/// sorts last)
///
/// Shared by `slots_for` impls so the `SlotChoice` numbering ranks slots by
/// version rather than slot name. The comparison is total without a
/// tie-break: each cpv is unique and lives in exactly one slot, so two
/// distinct slots can never share a best `Version`.
pub fn rank_slots_by_version(
    best: HashMap<Interned<DefaultInterner>, Version>,
) -> Vec<Interned<DefaultInterner>> {
    let mut slots: Vec<(Interned<DefaultInterner>, Version)> = best.into_iter().collect();
    slots.sort_by(|a, b| a.1.cmp(&b.1));
    slots.into_iter().map(|(s, _)| s).collect()
}

/// A simple in-memory repository for testing
#[derive(Clone)]
pub struct InMemoryRepository {
    packages: HashMap<Cpn, Vec<(Cpv, PackageVersions)>>,
    /// Global desired USE, used by `desired_use` (folded with each version's
    /// IUSE defaults)
    ///
    /// Tests set this instead of passing a config to the provider's
    /// constructor.
    use_config: UseConfig,
}

impl Default for InMemoryRepository {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryRepository {
    /// Create an empty repository with a default (empty) USE configuration
    pub fn new() -> Self {
        Self {
            packages: HashMap::new(),
            use_config: UseConfig::new(),
        }
    }

    /// Set the global desired USE returned (folded with IUSE defaults) by
    /// [`PackageRepository::desired_use`].
    pub fn set_use_config(&mut self, config: UseConfig) {
        self.use_config = config;
    }

    /// Add a package version with the given slot/subslot and dependencies
    ///
    /// Convenience wrapper over [`Self::add_version_full`] with no IUSE and no
    /// `REQUIRED_USE`.
    pub fn add_version(
        &mut self,
        cpv: Cpv,
        slot: Option<Interned<DefaultInterner>>,
        subslot: Option<Interned<DefaultInterner>>,
        deps: PackageDeps,
    ) {
        self.add_version_full(cpv, slot, subslot, None, vec![], deps);
    }

    /// Add a package version that also declares an IUSE set
    ///
    /// Like [`Self::add_version`] but records the version's `IUSE` flags.
    pub fn add_version_with_iuse(
        &mut self,
        cpv: Cpv,
        slot: Option<Interned<DefaultInterner>>,
        subslot: Option<Interned<DefaultInterner>>,
        iuse: Vec<Interned<DefaultInterner>>,
        deps: PackageDeps,
    ) {
        self.add_version_full(cpv, slot, subslot, None, iuse, deps);
    }

    /// Insert a fully-constructed [`PackageVersions`] for the given CPV
    ///
    /// Use this when you already have slot, subslot, repo, iuse, iuse_defaults,
    /// and deps assembled — e.g. when bridging from an external metadata cache.
    pub fn add_package_versions(&mut self, cpv: Cpv, versions: PackageVersions) {
        let cpn = cpv.cpn;
        self.packages.entry(cpn).or_default().push((cpv, versions));
    }

    /// Insert a version using the full set of fields (slot/subslot/repo/iuse/deps)
    ///
    /// Internal helper used by `add_version*` shims and by `InMemoryRepository`
    /// test fixtures. Callers that already have a `PackageVersions` should use
    /// `add_package_versions` instead.
    pub fn add_version_full(
        &mut self,
        cpv: Cpv,
        slot: Option<Interned<DefaultInterner>>,
        subslot: Option<Interned<DefaultInterner>>,
        repo: Option<Interned<DefaultInterner>>,
        iuse: Vec<Interned<DefaultInterner>>,
        deps: PackageDeps,
    ) {
        let cpn = cpv.cpn;
        self.packages.entry(cpn).or_default().push((
            cpv,
            PackageVersions {
                slot,
                subslot,
                repo,
                iuse,
                iuse_defaults: HashMap::new(),
                deps,
                required_use: None,
                empty_any_of_matches: true,
                needs_unmask: false,
            },
        ));
    }

    /// Insert a version carrying a `REQUIRED_USE` fact (for Level-C tests)
    pub fn add_version_with_required_use(
        &mut self,
        cpv: Cpv,
        slot: Option<Interned<DefaultInterner>>,
        iuse: Vec<Interned<DefaultInterner>>,
        deps: PackageDeps,
        required_use: RequiredUse,
    ) {
        let cpn = cpv.cpn;
        self.packages.entry(cpn).or_default().push((
            cpv,
            PackageVersions {
                slot,
                subslot: None,
                repo: None,
                iuse,
                iuse_defaults: HashMap::new(),
                deps,
                required_use: Some(required_use),
                empty_any_of_matches: true,
                needs_unmask: false,
            },
        ));
    }
}

impl PackageRepository for InMemoryRepository {
    fn all_packages(&self) -> Vec<Cpn> {
        self.packages.keys().cloned().collect()
    }

    fn desired_use(&self, cpv: &Cpv) -> UseConfig {
        // Test/benchmark fixture only (see the struct doc) — synthetic data
        // never needs the real make.conf-vs-environment wildcard distinction
        // `portage_solver::resolve_effective_use` models, so this just fills
        // in each IUSE default for a flag the fixture's base config left unset.
        let mut cfg = self.use_config.clone();
        if let Some(versions) = self.packages.get(&cpv.cpn)
            && let Some((_, meta)) = versions.iter().find(|(c, _)| c.version == cpv.version)
        {
            for (flag, def) in &meta.iuse_defaults {
                if cfg.get_opt(*flag).is_none() {
                    match def {
                        IUseDefault::Enabled => cfg.enable(*flag),
                        IUseDefault::Disabled => cfg.disable(*flag),
                    }
                }
            }
        }
        cfg
    }

    fn versions_for(&self, cpn: &Cpn) -> Vec<(Cpv, PackageVersions)> {
        self.packages
            .get(cpn)
            .map(|v| {
                v.iter()
                    .map(|(cpv, meta)| {
                        (
                            cpv.clone(),
                            PackageVersions {
                                slot: meta.slot,
                                subslot: meta.subslot,
                                repo: meta.repo,
                                iuse: meta.iuse.clone(),
                                iuse_defaults: meta.iuse_defaults.clone(),
                                deps: meta.deps.clone(),
                                required_use: meta.required_use.clone(),
                                empty_any_of_matches: meta.empty_any_of_matches,
                                needs_unmask: meta.needs_unmask,
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_repo() {
        let mut repo = InMemoryRepository::new();
        let cpv = Cpv::parse("dev-libs/openssl-3.0.0").unwrap();
        let cpn = Cpn::parse("dev-libs/openssl").unwrap();
        repo.add_version(
            cpv,
            Some(Interned::intern("0")),
            None,
            PackageDeps {
                depend: (vec![]).into(),
                rdepend: (vec![]).into(),
                bdepend: (vec![]).into(),
                pdepend: (vec![]).into(),
                idepend: (vec![]).into(),
            },
        );
        assert_eq!(repo.all_packages().len(), 1);
        assert_eq!(repo.versions_for(&cpn).len(), 1);
    }
}
