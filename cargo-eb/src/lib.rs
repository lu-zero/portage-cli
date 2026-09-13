//! `cargo eb` — Gentoo ebuild + cargo.eclass vendor tarball from a Cargo package.
//!
//! Standalone binary `cargo-eb` (Cargo applet lookup), not an `em` applet.

pub mod cargo;
pub mod ebuild;
pub mod fetch;
pub mod license;
pub mod snapshot;
pub mod vendor;

pub use cargo::{Crate, FileCrate, GitCrate, GitHost, PackageMetadata};
