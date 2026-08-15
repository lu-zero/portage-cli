//! Gentoo Portage VDB (installed package database) reader.
//!
//! Reads `/var/db/pkg` to provide access to installed package metadata,
//! USE flags, dependencies, and file ownership information.
//!
//! # Example
//!
//! ```no_run
//! use portage_vdb::Vdb;
//!
//! let vdb = Vdb::open("/var/db/pkg").unwrap();
//! for cat in vdb.categories() {
//!     for pkg in cat.packages() {
//!         println!("{}", pkg);
//!     }
//! }
//! ```

pub mod category;
mod collision;
mod contents;
mod error;
mod field_cache;
mod package;
mod vdb;
mod write;

pub use category::{Categories, CategoriesIter, Category, Packages, PackagesIter};
pub use collision::Collision;
pub use contents::{ContentsEntry, ContentsKind, ContentsRef, format_contents};
pub use error::Error;
pub use package::InstalledPackage;
pub use vdb::{AllPackages, AllPackagesIter, Vdb};
pub use write::MergeSpec;

/// Convenience alias for results in this crate.
pub type Result<T> = std::result::Result<T, Error>;
