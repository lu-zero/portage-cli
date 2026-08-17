//! Bridge between [portage-atom](https://crates.io/crates/portage-atom) and the
//! [PubGrub](https://crates.io/crates/pubgrub) dependency solver.
//!
//! Maps PMS package atoms to PubGrub's `Package`, `Version`, and `VersionSet`
//! traits, and provides a `DependencyProvider` implementation backed by a
//! package repository.
//!
//! # USE Flag Handling
//!
//! The caller resolves the **desired** USE state per package version (profile,
//! `make.conf`, `package.use`, and IUSE defaults are *its* concern) and hands it
//! in; the solver never resolves policy itself. See
//! `docs/use-and-solver-boundary.md` for the current/desired/needed model.
//!
//! USE conditionals in dependency strings are then handled via a hybrid strategy:
//!
//! - **User-decided** flags (`enabled` / `disabled`) are eagerly evaluated at
//!   registration time — the solver never sees those branches.
//! - **Solver-decided** flags are encoded as virtual packages with two versions
//!   (0 = OFF, 1 = ON). PubGrub's one-version-per-package constraint provides
//!   mutual exclusion for free.
//!
//! ## `SolverDecided` drives Level-C `REQUIRED_USE` auto-satisfaction
//!
//! The solver-decided path lets PubGrub *choose* a flag's value to satisfy
//! constraints — strictly more powerful than portage, which freezes USE before
//! resolving. A flag the caller marks [`UseFlagState::SolverDecided`] (with a
//! `prefer` value — the greedy keep-configured bias `choose_version` applies)
//! becomes a two-version `UseDecision` node; the package's `REQUIRED_USE` is
//! encoded over those nodes at ingestion (`convert::encode_required_use`),
//! so the emitted plan satisfies it by construction.
//!
//! By default nothing is ceded: the cli hands the solver a fully fixed USE
//! set, `REQUIRED_USE` violations stay post-solve advisories ("Level A"), and
//! the plan matches portage's. The reference consumer cedes flags only under
//! its opt-in `--autosolve-use`, and only for packages whose `REQUIRED_USE`
//! the fixed config actually violates ("Level C"). The concern split (caller
//! decides *which* flags are free and the preference; the crate decides
//! *values*), encoding, and phasing live in `docs/required-use-level-c.md`
//! and `docs/use-and-solver-boundary.md`. Global minimal-flip optimisation is
//! out of scope — the preference is greedy, per flag.
#![warn(missing_docs)]

mod convert;
mod error;
mod graph;
mod package;
mod provider;
mod repository;
mod required_use;
mod solver_impl;
mod use_config;
mod validate;
mod version_set;

pub use convert::SlotMap;
pub use error::{Error, Result};
pub use graph::{DepClass, DepEdge};
pub use package::{MergeRoot, PortagePackage};
pub use portage_atom::interner::{DefaultInterner, Interned};
pub use provider::{
    CededFlag, DroppedDep, InstalledPackage, InstalledPolicy, PortageDependencyProvider,
    UseFlagRequirement, build_slot_map,
};
pub use repository::{
    IUseDefault, InMemoryRepository, PackageDeps, PackageRepository, PackageVersions,
    rank_slots_by_version,
};
pub use required_use::RequiredUse;
pub use use_config::{UseConfig, UseFlagState, UseLayer, UseOverride, resolve_effective_use};
pub use validate::{BlockerHit, BlockerVictim, HeldBackTarget, SlotOperatorBinding};
pub use version_set::PortageVersionSet;

/// Render a pubgrub `NoSolution` derivation tree into a human-readable
/// explanation.
///
/// The raw [`pubgrub::DerivationTree`] `Debug` output prints package names as
/// their internal interned string keys (e.g. `Cpn { category: Interned(3),
/// package: Interned(4) }`), which is unreadable. This routes the tree through
/// pubgrub's [`pubgrub::DefaultStringReporter`], which renders packages and version sets
/// via their [`Display`](std::fmt::Display) impls — producing real atoms like
/// `dev-lang/python:3.14`. It also collapses "no versions" derivations into
/// cleaner external messages first.
pub fn format_no_solution(
    mut tree: pubgrub::DerivationTree<
        PortagePackage,
        PortageVersionSet,
        <PortageDependencyProvider as pubgrub::DependencyProvider>::M,
    >,
) -> String {
    use pubgrub::Reporter;
    tree.collapse_no_versions();
    humanize_root(pubgrub::DefaultStringReporter::report(&tree))
}

/// Replace the synthetic root package's identity in a rendered derivation tree
/// with what it stands for. `PortagePackage::Root`'s own `Display` is left
/// alone — debug and tracing output name it by its real identity on purpose.
fn humanize_root(report: String) -> String {
    // The root is a single synthetic version, so the reporter always prints it
    // with the version suffix; strip that too rather than leaving a stray `0`.
    report
        .replace("__internal__/root 0", "the requested targets")
        .replace("__internal__/root", "the requested targets")
}

/// Render any [`pubgrub::PubGrubError`] from a resolve into a human-readable
/// string. `NoSolution` is explained via [`format_no_solution`]; other variants
/// fall back to `Debug`. Lets callers report resolver failures without pulling
/// in pubgrub types themselves.
pub fn format_solve_error(err: pubgrub::PubGrubError<PortageDependencyProvider>) -> String {
    match err {
        pubgrub::PubGrubError::NoSolution(tree) => format_no_solution(tree),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod report_tests {
    use super::humanize_root;

    #[test]
    fn root_package_is_named_by_what_it_stands_for() {
        assert_eq!(
            humanize_root("__internal__/root 0 depends on app-misc/asciinema".into()),
            "the requested targets depends on app-misc/asciinema"
        );
        assert_eq!(
            humanize_root("__internal__/root is forbidden".into()),
            "the requested targets is forbidden"
        );
        // Other synthetic nodes keep their own names.
        assert_eq!(
            humanize_root("__internal__/choice_2 0 is unavailable".into()),
            "__internal__/choice_2 0 is unavailable"
        );
    }
}
