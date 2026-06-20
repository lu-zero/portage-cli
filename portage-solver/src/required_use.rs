//! `REQUIRED_USE` as a *fact* in the solver's own vocabulary.
//!
//! This mirrors `portage_metadata::RequiredUseExpr` but uses interned flag
//! names (the type the solver works in) instead of `String`, so the solver
//! layer stays decoupled from the md5-cache parser: the caller translates the
//! parsed metadata grammar into this type when it builds [`VersionFacts`],
//! exactly as it already turns metadata strings into [`crate::PackageDeps`].
//!
//! It is a **fact** (intrinsic ebuild metadata), not policy — so it rides on
//! [`VersionFacts`] alongside `iuse` and `deps`, never on
//! [`crate::PackageRepository::desired_use`].

use portage_atom::interner::{DefaultInterner, Interned};

/// A node in a `REQUIRED_USE` constraint tree, in interned-flag form.
///
/// See [PMS 7.3.4](https://projects.gentoo.org/pms/9/pms.html#use-state-constraints).
/// The variants match `portage_metadata::RequiredUseExpr` one-for-one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequiredUse {
    /// A single USE flag, possibly negated with `!`.
    Flag {
        /// Flag name.
        name: Interned<DefaultInterner>,
        /// `true` if prefixed with `!`.
        negated: bool,
    },
    /// `|| ( ... )` — at least one child must be satisfied.
    AnyOf(Vec<RequiredUse>),
    /// `^^ ( ... )` — exactly one child must be satisfied.
    ExactlyOne(Vec<RequiredUse>),
    /// `?? ( ... )` — at most one child may be satisfied.
    AtMostOne(Vec<RequiredUse>),
    /// `flag? ( ... )` / `!flag? ( ... )` — children guarded by a flag.
    UseConditional {
        /// Guard flag name.
        flag: Interned<DefaultInterner>,
        /// `true` for `!flag?` (negated guard).
        negated: bool,
        /// Children guarded by this flag.
        entries: Vec<RequiredUse>,
    },
    /// Top-level grouping: all children must be satisfied.
    All(Vec<RequiredUse>),
}
