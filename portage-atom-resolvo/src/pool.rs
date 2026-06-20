//! Arena-based pool mapping resolvo IDs to portage-atom types.
//!
//! [`PortagePool`] provides the storage that backs every resolvo identifier
//! ([`NameId`], [`SolvableId`], [`VersionSetId`], etc.) with a concrete
//! portage-atom value. The provider and interner implementations index into
//! this pool.

use std::collections::{HashMap, HashSet};

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, DepEntry, Operator, Version};
use resolvo::{
    ConditionId, DenseIndex, NameId, SolvableId, StringId, VersionSetId, VersionSetUnionId,
};

/// A labeled dependency edge between two solvables in a solution.
///
/// Produced by
/// [`PortageDependencyProvider::dependency_graph`](crate::PortageDependencyProvider::dependency_graph)
/// to give callers the full dep-class–annotated graph needed for install
/// ordering and cycle analysis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepEdge {
    /// The depending solvable.
    pub from: SolvableId,
    /// The dependency target.
    pub to: SolvableId,
    /// Which dependency class this edge comes from.
    pub class: DepClass,
}

/// Configuration for USE flag evaluation.
///
/// Controls how USE-conditional dependency groups (`use? ( deps )`) are
/// handled during provider construction:
///
/// - **`enabled`** — flags eagerly included (normal `use?` includes children,
///   `!use?` skips them).
/// - **`disabled`** — flags eagerly excluded (`use?` skips children, `!use?`
///   includes them). This is the implicit default for any flag not listed.
/// - **`solver_decided`** — the SAT solver decides whether the flag is active.
///   A virtual solvable `virtual/USE_<flag>` is created; when the solver
///   selects it the corresponding `use? ( deps )` become active.  Negated
///   `!use? ( deps )` on solver-decided flags are included **unconditionally**
///   (conservative: resolvo conditions have no NOT operator).
#[derive(Debug, Clone, Default)]
pub struct UseConfig {
    /// Flags forced ON: `use? ( deps )` active, `!use? ( deps )` skipped.
    pub enabled: HashSet<Interned<DefaultInterner>>,
    /// Flags forced OFF: `use? ( deps )` skipped, `!use? ( deps )` active.
    pub disabled: HashSet<Interned<DefaultInterner>>,
    /// Flags left for the SAT solver to decide (via a `virtual/USE_<flag>`).
    pub solver_decided: HashSet<Interned<DefaultInterner>>,
}

impl From<HashSet<Interned<DefaultInterner>>> for UseConfig {
    fn from(enabled: HashSet<Interned<DefaultInterner>>) -> Self {
        Self {
            enabled,
            disabled: HashSet::new(),
            solver_decided: HashSet::new(),
        }
    }
}

/// Package name used as the resolvo name axis.
///
/// Slots are encoded into the name so that packages in different slots
/// (e.g. `dev-lang/python:3.11` vs `dev-lang/python:3.12`) are treated
/// as independent names by the solver.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PackageName {
    /// Category/package name.
    pub cpn: Cpn,
    /// Slot encoded into the name axis, or `None` when slot-agnostic.
    pub slot: Option<Interned<DefaultInterner>>,
}

impl std::fmt::Display for PackageName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.cpn)?;
        if let Some(slot) = &self.slot {
            write!(f, ":{}", slot)?;
        }
        Ok(())
    }
}

/// A single candidate version and the metadata resolvo needs to solve it.
#[derive(Debug, Clone)]
pub struct PackageMetadata {
    /// Category/package/version of this candidate.
    pub cpv: Cpv,
    /// Declared slot, if any.
    pub slot: Option<Interned<DefaultInterner>>,
    /// Declared subslot, if any.
    pub subslot: Option<Interned<DefaultInterner>>,
    /// Flags declared in `IUSE`.
    pub iuse: Vec<Interned<DefaultInterner>>,
    /// Flags effectively enabled for this candidate.
    pub use_flags: HashSet<Interned<DefaultInterner>>,
    /// Originating repository, if known.
    pub repo: Option<Interned<DefaultInterner>>,
    /// Dependencies separated by PMS dependency class.
    pub dependencies: PackageDeps,
}

/// Dependency trees separated by PMS dependency class.
///
/// Each field corresponds to one ebuild variable:
/// - `depend` — `DEPEND`: build-time dependencies
/// - `rdepend` — `RDEPEND`: runtime dependencies
/// - `bdepend` — `BDEPEND`: build host dependencies (cross-compilation)
/// - `pdepend` — `PDEPEND`: post-merge dependencies (allows circular deps)
/// - `idepend` — `IDEPEND`: install-time dependencies
///
/// The solver currently treats all classes as hard requirements.  `PDEPEND`
/// entries are flagged so the package manager can schedule them after the
/// dependent package.
#[derive(Debug, Clone, Default)]
pub struct PackageDeps {
    /// Build-time dependencies (`DEPEND`).
    pub depend: Vec<DepEntry>,
    /// Runtime dependencies (`RDEPEND`).
    pub rdepend: Vec<DepEntry>,
    /// Build host dependencies for cross-compilation (`BDEPEND`).
    pub bdepend: Vec<DepEntry>,
    /// Post-merge dependencies (`PDEPEND`).
    pub pdepend: Vec<DepEntry>,
    /// Install-time dependencies (`IDEPEND`).
    pub idepend: Vec<DepEntry>,
}

impl PackageDeps {
    /// Iterate over all dependency classes and their entries.
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

/// Version constraint derived from a [`DepEntry`].
///
/// For normal (non-blocker) dependencies the constraint is used directly:
/// `filter_candidates` returns candidates whose version **matches**
/// `(operator, version)`.
///
/// For blocker dependencies (`!atom` / `!!atom`) the constraint is stored
/// with [`inverted`](Self::inverted) `= true`.  resolvo processes
/// `constrains` entries by calling `filter_candidates(…, inverse=true)` and
/// **forbidding** the returned candidates.  With `inverted = true` the
/// match result is flipped *before* resolvo's own `inverse` flag is
/// applied, so the net effect is:
///
/// > A candidate is forbidden when it **matches** the original operator.
///
/// This allows every PMS operator — including `=`, `~`, and `=*` whose
/// complements cannot be expressed as a single range — to work correctly
/// for blockers.
///
/// See [`crate::PortageDependencyProvider`]'s `filter_candidates` for the
/// evaluation logic.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VersionConstraint {
    /// Category/package the constraint applies to.
    pub cpn: Cpn,
    /// Comparison operator (`=`, `>=`, `~`, …).
    pub operator: Operator,
    /// Version operand the operator compares against.
    pub version: Version,
    /// Whether the version operand is a `=*` glob (`=foo-1.2*`).
    pub glob: bool,
    /// Required slot, if the atom pins one.
    pub slot: Option<Interned<DefaultInterner>>,
    /// Required subslot, if the atom pins one.
    pub subslot: Option<Interned<DefaultInterner>>,
    /// Required `::repo`, if the atom pins one.
    pub repo: Option<Interned<DefaultInterner>>,
    /// USE constraints `[flag]`/`[-flag]` as `(flag, enabled)` pairs.
    pub use_constraints: Vec<(Interned<DefaultInterner>, bool)>,
    /// When `true`, the match result is flipped before resolvo applies its own
    /// `inverse` flag — used to express blockers (see the type docs).
    pub inverted: bool,
}

impl std::fmt::Display for VersionConstraint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.inverted {
            write!(f, "!")?;
        }
        write!(f, "{}{}-{}", self.operator, self.cpn, self.version)?;
        if self.glob {
            write!(f, "*")?;
        }
        if let Some(slot) = &self.slot {
            write!(f, ":{}", slot)?;
            if let Some(subslot) = &self.subslot {
                write!(f, "/{}", subslot)?;
            }
        }
        if !self.use_constraints.is_empty() {
            write!(f, "[")?;
            for (i, (flag, enabled)) in self.use_constraints.iter().enumerate() {
                if i > 0 {
                    write!(f, ",")?;
                }
                if *enabled {
                    write!(f, "{}", flag)?;
                } else {
                    write!(f, "-{}", flag)?;
                }
            }
            write!(f, "]")?;
        }
        if let Some(repo) = &self.repo {
            write!(f, "::{}", repo)?;
        }
        Ok(())
    }
}

/// Arena-based storage for all resolvo-interned objects.
///
/// Every resolvo ID type is backed by a `Vec` here, indexed by the ID's
/// inner `usize`. Reverse-lookup `HashMap`s prevent duplicate interning.
pub struct PortagePool {
    // NameId arena
    pub(crate) names: Vec<PackageName>,
    pub(crate) names_rev: HashMap<PackageName, NameId>,

    // SolvableId arena
    pub(crate) solvables: Vec<PackageMetadata>,
    pub(crate) solvable_names: Vec<NameId>,

    // VersionSetId arena
    pub(crate) version_sets: Vec<VersionConstraint>,
    pub(crate) version_set_names: Vec<NameId>,
    pub(crate) version_sets_rev: HashMap<(NameId, VersionConstraint), VersionSetId>,

    // VersionSetUnionId arena
    pub(crate) version_set_unions: Vec<Vec<VersionSetId>>,

    // ConditionId arena (reserved for future USE-as-conditions)
    pub(crate) conditions: Vec<resolvo::Condition>,

    // StringId arena
    pub(crate) strings: Vec<String>,
}

impl PortagePool {
    /// Create an empty pool.
    pub fn new() -> Self {
        Self {
            names: Vec::new(),
            names_rev: HashMap::new(),
            solvables: Vec::new(),
            solvable_names: Vec::new(),
            version_sets: Vec::new(),
            version_set_names: Vec::new(),
            version_sets_rev: HashMap::new(),
            version_set_unions: Vec::new(),
            conditions: Vec::new(),
            strings: Vec::new(),
        }
    }

    // --- NameId ---

    /// Intern a package name, returning the existing ID if already interned.
    pub fn intern_name(&mut self, name: PackageName) -> NameId {
        if let Some(&id) = self.names_rev.get(&name) {
            return id;
        }
        let id = NameId::from_index(self.names.len());
        self.names_rev.insert(name.clone(), id);
        self.names.push(name);
        id
    }

    /// Look up the [`PackageName`] for a [`NameId`].
    pub fn resolve_name(&self, id: NameId) -> &PackageName {
        &self.names[id.to_index()]
    }

    // --- SolvableId ---

    /// Add a solvable (concrete package version) to the pool.
    pub fn intern_solvable(&mut self, name_id: NameId, meta: PackageMetadata) -> SolvableId {
        let id = SolvableId::from_index(self.solvables.len());
        self.solvables.push(meta);
        self.solvable_names.push(name_id);
        id
    }

    /// Look up the metadata for a [`SolvableId`].
    pub fn resolve_solvable(&self, id: SolvableId) -> &PackageMetadata {
        &self.solvables[id.to_index()]
    }

    /// Look up the [`NameId`] for a [`SolvableId`].
    pub fn solvable_name(&self, id: SolvableId) -> NameId {
        self.solvable_names[id.to_index()]
    }

    // --- VersionSetId ---

    /// Intern a version constraint, deduplicating by value.
    pub fn intern_version_set(
        &mut self,
        name_id: NameId,
        constraint: VersionConstraint,
    ) -> VersionSetId {
        let key = (name_id, constraint.clone());
        if let Some(&id) = self.version_sets_rev.get(&key) {
            return id;
        }
        let id = VersionSetId::from_index(self.version_sets.len());
        self.version_sets_rev.insert(key, id);
        self.version_sets.push(constraint);
        self.version_set_names.push(name_id);
        id
    }

    /// Look up the constraint for a [`VersionSetId`].
    pub fn resolve_version_set(&self, id: VersionSetId) -> &VersionConstraint {
        &self.version_sets[id.to_index()]
    }

    /// Look up the [`NameId`] for a [`VersionSetId`].
    pub fn version_set_name(&self, id: VersionSetId) -> NameId {
        self.version_set_names[id.to_index()]
    }

    /// Return the number of interned version sets.
    pub fn version_set_count(&self) -> usize {
        self.version_sets.len()
    }

    // --- VersionSetUnionId ---

    /// Intern a union (OR) of version sets.
    pub fn intern_version_set_union(&mut self, sets: Vec<VersionSetId>) -> VersionSetUnionId {
        let id = VersionSetUnionId::from_index(self.version_set_unions.len());
        self.version_set_unions.push(sets);
        id
    }

    /// Look up the version sets in a union.
    pub fn resolve_version_set_union(&self, id: VersionSetUnionId) -> &[VersionSetId] {
        &self.version_set_unions[id.to_index()]
    }

    // --- ConditionId ---

    /// Intern a condition.
    pub fn intern_condition(&mut self, condition: resolvo::Condition) -> ConditionId {
        let id = ConditionId::from_index(self.conditions.len());
        self.conditions.push(condition);
        id
    }

    /// Look up a condition.
    pub fn resolve_condition(&self, id: ConditionId) -> &resolvo::Condition {
        &self.conditions[id.to_index()]
    }

    // --- StringId ---

    /// Intern a string (used for solver error messages).
    pub fn intern_string(&mut self, s: String) -> StringId {
        let id = StringId::from_index(self.strings.len());
        self.strings.push(s);
        id
    }

    /// Look up an interned string.
    pub fn resolve_string(&self, id: StringId) -> &str {
        &self.strings[id.to_index()]
    }
}

impl Default for PortagePool {
    fn default() -> Self {
        Self::new()
    }
}

/// Policy for how the solver treats an installed package.
///
/// See [`InstalledSet`] for usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstalledPolicy {
    /// Solver prefers this version but may choose a different one.
    Favored,
    /// Solver MUST keep this exact version; solve fails if impossible.
    Locked,
}

/// Packages currently installed on the system.
///
/// Installed packages not found in the repository are injected into the
/// candidate pool automatically so the solver can reference them.
///
/// # Example
///
/// ```ignore
/// let mut installed = InstalledSet::new();
/// installed.add_favored(some_meta);
/// installed.add_locked(other_meta);
/// let provider = PortageDependencyProvider::with_installed(&repo, &use_config, &installed);
/// ```
#[derive(Debug, Clone, Default)]
pub struct InstalledSet {
    pub(crate) packages: Vec<(PackageMetadata, InstalledPolicy)>,
}

impl InstalledSet {
    /// Create an empty installed set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a package with an explicit policy.
    pub fn add(&mut self, meta: PackageMetadata, policy: InstalledPolicy) {
        self.packages.push((meta, policy));
    }

    /// Add a package as [`InstalledPolicy::Favored`] (soft preference).
    pub fn add_favored(&mut self, meta: PackageMetadata) {
        self.packages.push((meta, InstalledPolicy::Favored));
    }

    /// Add a package as [`InstalledPolicy::Locked`] (hard constraint).
    pub fn add_locked(&mut self, meta: PackageMetadata) {
        self.packages.push((meta, InstalledPolicy::Locked));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_name_roundtrip() {
        let mut pool = PortagePool::new();
        let name = PackageName {
            cpn: Cpn::new("dev-lang", "rust"),
            slot: None,
        };
        let id = pool.intern_name(name.clone());
        assert_eq!(pool.resolve_name(id), &name);
    }

    #[test]
    fn intern_name_dedup() {
        let mut pool = PortagePool::new();
        let name = PackageName {
            cpn: Cpn::new("dev-lang", "rust"),
            slot: None,
        };
        let id1 = pool.intern_name(name.clone());
        let id2 = pool.intern_name(name);
        assert_eq!(id1, id2);
    }

    #[test]
    fn intern_name_different_slots() {
        let mut pool = PortagePool::new();
        let a = pool.intern_name(PackageName {
            cpn: Cpn::new("dev-lang", "python"),
            slot: Some(Interned::intern("3.11")),
        });
        let b = pool.intern_name(PackageName {
            cpn: Cpn::new("dev-lang", "python"),
            slot: Some(Interned::intern("3.12")),
        });
        assert_ne!(a, b);
    }

    #[test]
    fn intern_solvable_roundtrip() {
        let mut pool = PortagePool::new();
        let name_id = pool.intern_name(PackageName {
            cpn: Cpn::new("dev-lang", "rust"),
            slot: None,
        });
        let meta = PackageMetadata {
            cpv: Cpv::parse("dev-lang/rust-1.75.0").unwrap(),
            slot: Some(Interned::intern("0")),
            subslot: None,
            iuse: vec![],
            use_flags: HashSet::new(),
            repo: None,
            dependencies: PackageDeps::default(),
        };
        let sid = pool.intern_solvable(name_id, meta);
        assert_eq!(pool.solvable_name(sid), name_id);
        assert_eq!(
            pool.resolve_solvable(sid).cpv,
            Cpv::parse("dev-lang/rust-1.75.0").unwrap()
        );
    }

    #[test]
    fn intern_version_set_dedup() {
        let mut pool = PortagePool::new();
        let name_id = pool.intern_name(PackageName {
            cpn: Cpn::new("dev-lang", "rust"),
            slot: None,
        });
        let c = VersionConstraint {
            cpn: Cpn::new("dev-lang", "rust"),
            operator: Operator::GreaterOrEqual,
            version: Version::parse("1.75.0").unwrap(),
            glob: false,
            slot: None,
            subslot: None,
            repo: None,
            use_constraints: vec![],
            inverted: false,
        };
        let id1 = pool.intern_version_set(name_id, c.clone());
        let id2 = pool.intern_version_set(name_id, c);
        assert_eq!(id1, id2);
    }

    #[test]
    fn intern_string_roundtrip() {
        let mut pool = PortagePool::new();
        let id = pool.intern_string("hello".into());
        assert_eq!(pool.resolve_string(id), "hello");
    }
}
