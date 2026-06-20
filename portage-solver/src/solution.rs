//! Solver-agnostic solution and plan vocabulary.
//!
//! These are the types a [`crate::Solver`] implementation produces after a
//! resolve, expressed in plain Portage terms (`Cpn`, `Version`, slot) rather
//! than a solver's internal IDs. Consumers iterate these without knowing which
//! algorithm produced them.

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Operator, Version};
use thiserror::Error;

/// Where a real package instance is merged — host `BROOT` or target `ROOT`.
///
/// Under cross-compilation the same CPV can appear twice (native host tool +
/// cross target runtime). Native builds are always [`MergeRoot::Target`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord, Default)]
pub enum MergeRoot {
    /// Native build merged to the build host (`BROOT`, `/`).
    Host,
    /// Cross (or native target) build merged to `ROOT` / `EROOT`.
    #[default]
    Target,
}

/// A resolved package in a plan: identity + selected version.
///
/// This is the solver-agnostic counterpart of pubgrub's
/// `(PortagePackage, Version)` solution entry, with the virtual
/// (`UseDecision`/`Choice`/`SlotChoice`) nodes stripped — only real packages
/// appear.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct SelectedPackage {
    /// Category/package name.
    pub cpn: Cpn,
    /// Selected version.
    pub version: Version,
    /// Bound slot, if the package is slotted.
    pub slot: Option<Interned<DefaultInterner>>,
    /// Merge destination.
    pub merge_root: MergeRoot,
}

impl SelectedPackage {
    /// Create a target-root selected package.
    pub fn new(cpn: Cpn, version: Version, slot: Option<Interned<DefaultInterner>>) -> Self {
        Self {
            cpn,
            version,
            slot,
            merge_root: MergeRoot::Target,
        }
    }
}

impl std::fmt::Display for SelectedPackage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.slot, self.merge_root) {
            (Some(slot), MergeRoot::Target) => write!(f, "{}-{}:{}", self.cpn, self.version, slot),
            (Some(slot), MergeRoot::Host) => {
                write!(f, "{}-{}:{}@host", self.cpn, self.version, slot)
            }
            (None, MergeRoot::Target) => write!(f, "{}-{}", self.cpn, self.version),
            (None, MergeRoot::Host) => write!(f, "{}-{}@host", self.cpn, self.version),
        }
    }
}

/// A labeled dependency edge in the plan graph.
///
/// Solver-agnostic counterpart of pubgrub's `DepEdge`, keyed on
/// [`SelectedPackage`] rather than solver-internal package IDs.
#[derive(Clone, Debug)]
pub struct DepEdge {
    /// The package that declares the dependency.
    pub from: SelectedPackage,
    /// The package that is depended upon.
    pub to: SelectedPackage,
    /// Which dependency class this edge belongs to.
    pub class: crate::DepClass,
    /// The USE flag in `from` that gates this dep, if it was inside
    /// `flag? ( dep )`.
    pub via_use_flag: Option<Interned<DefaultInterner>>,
}

/// Policy for how the solver treats an installed package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstalledPolicy {
    /// Solver prefers this version but may choose a different one.
    Favor,
    /// Solver MUST keep this exact version; solve fails if impossible.
    Lock,
    /// Treat as a rebuild source (native `--emptytree`): the installed version
    /// is not favoured, the full deep closure is expanded.
    Rebuild,
}

/// A package currently installed on the system, fed to the solver before
/// resolve so it can prefer (or pin) installed versions and compute action
/// tags / rebuilds.
#[derive(Clone, Debug)]
pub struct InstalledPackage {
    /// Category/package name.
    pub cpn: Cpn,
    /// Installed version.
    pub version: Version,
    /// Bound slot, if slotted.
    pub slot: Option<Interned<DefaultInterner>>,
    /// How the solver treats this installed package.
    pub policy: InstalledPolicy,
    /// Active USE flags on the installed instance.
    pub active_use: Vec<Interned<DefaultInterner>>,
    /// IUSE flags declared by the installed instance.
    pub iuse: Vec<Interned<DefaultInterner>>,
}

/// A resolve target, in already-resolved form (the consumer's slot/version
/// pinning, e.g. keyword/mask-aware best-slot selection, is done before this).
///
/// Each [`crate::Solver::resolve_targets`] call solves all targets in one joint
/// solve over a synthetic root.
#[derive(Clone, Debug)]
pub struct TargetSpec {
    /// Category/package name.
    pub cpn: Cpn,
    /// Bound slot the target pins, if any.
    pub slot: Option<Interned<DefaultInterner>>,
    /// Version operator for `version`, or `None` for "any".
    pub op: Option<Operator>,
    /// Version operand for `op`, or `None` for "any".
    pub version: Option<Version>,
    /// Whether `version` is a `=*` glob (only meaningful with
    /// [`Operator::Equal`]).
    pub glob: bool,
}

impl TargetSpec {
    /// Any version of `cpn` (optionally in `slot`).
    pub fn any_in(cpn: Cpn, slot: Option<Interned<DefaultInterner>>) -> Self {
        Self {
            cpn,
            slot,
            op: None,
            version: None,
            glob: false,
        }
    }
}

/// A dependency the solver had to drop because no candidate satisfied it in the
/// reachable closure (e.g. an atom referencing a package absent from the
/// repository). Reported for diagnostics; the plan is still produced.
#[derive(Clone, Debug)]
pub struct DroppedDep {
    /// CPN of the dropped dependency.
    pub cpn: Cpn,
}

/// A USE flag the solver was ceded (Level-C `REQUIRED_USE`) and the value it
/// picked, for display as autounmask-style output.
#[derive(Clone, Debug)]
pub struct CededFlag {
    /// Package whose flag was ceded.
    pub cpn: Cpn,
    /// The ceded flag.
    pub flag: Interned<DefaultInterner>,
    /// Value the solver chose.
    pub value: bool,
    /// `true` if this differs from the caller's configured value.
    pub flipped: bool,
}

/// A per-target USE-flag requirement the solve derived (the "needed" set),
/// surfaced as autounmask `package.use` suggestions.
#[derive(Clone, Debug)]
pub struct UseFlagRequirement {
    /// Package whose flags are constrained.
    pub cpn: Cpn,
    /// Version the constraint applies to.
    pub version: Version,
    /// If the post-solve fixpoint upgraded the version, the upgraded target.
    pub upgrade_to: Option<Version>,
    /// Flags that must be enabled.
    pub required_enabled: Vec<Interned<DefaultInterner>>,
    /// Flags that must be disabled.
    pub required_disabled: Vec<Interned<DefaultInterner>>,
    /// CPNs of the packages driving this requirement.
    pub required_by: Vec<String>,
}

/// A post-solve advisory violation (reported after the plan, as portage does).
#[derive(Clone, Debug, Error)]
pub enum Violation {
    /// A blocker (`!foo` / `!!foo`) conflict.
    #[error("{strength} blocker conflict: {pkg} blocks {blocker}")]
    Blocker {
        /// The package declaring the blocker.
        pkg: String,
        /// The blocker atom string.
        blocker: String,
        /// `"weak"` for `!`, `"strong"` for `!!`.
        strength: &'static str,
    },
    /// A USE-dep constraint (`[flag]` etc.) was violated.
    #[error("USE-dep conflict: {0}: {1}")]
    UseDep(String, String),
    /// A `::repo` constraint was violated.
    #[error("repo constraint conflict: {0}: {1}")]
    Repo(String, String),
}

/// Error returned by [`crate::Solver::resolve_targets`].
#[derive(Debug, Error)]
pub enum SolveError {
    /// The target set has no satisfying solution. The string carries a
    /// solver-specific human-readable derivation/report.
    #[error("no solution: {0}")]
    NoSolution(String),
    /// The provider could not satisfy the request for another reason.
    #[error("{0}")]
    Provider(String),
}
