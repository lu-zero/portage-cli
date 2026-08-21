//! Gentoo ebuild metadata cache types and parser based on [PMS]
//!
//! This crate provides types for representing ebuild metadata and a
//! parser for the `md5-cache` format used by Gentoo's package manager.
//!
//! [PMS]: https://projects.gentoo.org/pms/9/pms.html
//!
//! # Overview
//!
//! Ebuild files are bash scripts that require a full shell interpreter to
//! evaluate. The **metadata cache** (`metadata/md5-cache/`) stores
//! pre-computed metadata in a simple `KEY=VALUE` format, which is what tools
//! consume day-to-day. This crate reads and writes that format.
//!
//! # Examples
//!
//! Parse a cache entry:
//!
//! ```
//! use portage_metadata::CacheEntry;
//!
//! let input = "\
//! EAPI=7
//! DESCRIPTION=Example package
//! SLOT=0
//! KEYWORDS=~amd64
//! DEFINED_PHASES=compile install
//! ";
//! let entry = CacheEntry::parse(input).unwrap();
//! assert_eq!(entry.metadata.description, "Example package");
//! assert_eq!(entry.metadata.eapi.to_string(), "7");
//! ```
#![warn(missing_docs)]

mod cache;
mod diagnostic;
mod eapi;
mod error;
mod iuse;
mod iuse_effective;
mod keyword;
mod license;
mod metadata;
mod phase;
mod required_use;
mod restrict;
mod src_uri;

// Re-export public types
pub use cache::{CacheEntry, RawCacheEntry};
pub use diagnostic::ParseDiagnostic;
pub use eapi::Eapi;
pub use error::{Error, Result};
pub use iuse::{IUse, IUseDefault};
pub use iuse_effective::{iuse_effective, iuse_effective_profile_part};
pub use keyword::{Keyword, Stability};
pub use license::LicenseExpr;
pub use metadata::EbuildMetadata;
pub use phase::Phase;
pub use required_use::RequiredUseExpr;
pub use restrict::RestrictExpr;
pub use src_uri::SrcUriEntry;

// Re-export interner module so downstream crates can use the same types
pub use portage_atom::interner;
