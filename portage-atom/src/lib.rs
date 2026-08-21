//! Lightweight Portage package atom parser based on [PMS]
//!
//! This crate provides types and parsing for Gentoo/Portage package atoms
//! according to the [Package Manager Specification (PMS)][PMS].
//!
//! [PMS]: https://projects.gentoo.org/pms/9/pms.html
//!
//! # Examples
//!
//! Parse a simple unversioned atom:
//! ```
//! use portage_atom::Cpn;
//!
//! let cpn = Cpn::parse("dev-lang/rust").unwrap();
//! assert_eq!(cpn.category, "dev-lang");
//! assert_eq!(cpn.package, "rust");
//! ```
//!
//! Parse a versioned atom:
//! ```
//! use portage_atom::Cpv;
//!
//! let cpv = Cpv::parse("dev-lang/rust-1.75.0").unwrap();
//! assert_eq!(cpv.version.numbers[0], 1);
//! ```
//!
//! Parse a full dependency:
//! ```
//! use portage_atom::Dep;
//!
//! let dep = Dep::parse(">=dev-lang/rust-1.75.0:0[llvm_targets_AMDGPU]").unwrap();
//! assert!(dep.version.is_some());
//! assert!(dep.slot_dep.is_some());
//! assert!(dep.use_deps.is_some());
//! ```
#![warn(missing_docs)]

mod cpn;
mod cpv;
mod dep;
mod dep_entry;
mod error;
mod lazy;
mod parsers;
mod pf;
mod slot;
mod use_dep;
mod use_flag_lookup;
mod version;

pub use gentoo_interner as interner;

// Re-export main types
pub use cpn::Cpn;
pub use cpv::Cpv;
pub use dep::{Blocker, Dep};
pub use dep_entry::{DepEntry, DepList, LazyDepList};
pub use error::{Error, Result};
pub use lazy::Lazy;
pub use pf::Pf;
pub use slot::{Slot, SlotDep, SlotOperator};
pub use use_dep::{UseDefault, UseDep, UseDepKind};
pub use use_flag_lookup::UseFlagLookup;
pub use version::{Operator, Revision, Suffix, SuffixKind, Version};

// Re-export bon-derived builders when the feature is enabled
#[cfg(feature = "builder")]
pub use cpn::CpnBuilder;
#[cfg(feature = "builder")]
pub use cpv::CpvBuilder;
#[cfg(feature = "builder")]
pub use dep::DepBuilder;
#[cfg(feature = "builder")]
pub use slot::SlotBuilder;
#[cfg(feature = "builder")]
pub use use_dep::UseDepBuilder;
#[cfg(feature = "builder")]
pub use version::{SuffixBuilder, VersionBuilder};
