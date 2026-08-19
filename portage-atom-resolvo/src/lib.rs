//! Bridge between [`portage_atom`] and the [`resolvo`] dependency solver
//!
//! This crate maps Portage package atoms, versions, and dependency trees onto
//! resolvo's generic solver interface, enabling SAT-based dependency resolution
//! for Gentoo-style package managers.
#![warn(missing_docs)]

mod pool;
mod provider;
mod repository;
mod version_match;

pub use pool::{
    DepClass, DepEdge, InstalledPolicy, InstalledSet, PackageDeps, PackageMetadata, PackageName,
    PortagePool, UseConfig, VersionConstraint,
};
pub use portage_atom::DepEntry;
pub use portage_atom::interner;
pub use provider::PortageDependencyProvider;
pub use repository::{InMemoryRepository, PackageRepository};
pub use version_match::version_matches;

#[cfg(test)]
mod solver_tests;
