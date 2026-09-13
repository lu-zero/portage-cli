//! `cargo ebuild` — Gentoo ebuild + cargo.eclass vendor tarball from a Cargo package.
//!
//! Standalone binary `cargo-ebuild` (Cargo applet lookup), not an `em` applet.

pub mod cargo;
pub mod ebuild;
pub mod fetch;
pub mod license;
pub mod vendor;

pub use cargo::{Crate, FileCrate, GitCrate, GitHost, PackageMetadata};
