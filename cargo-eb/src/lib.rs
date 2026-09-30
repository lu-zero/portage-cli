//! `cargo eb`: a Gentoo ebuild plus cargo.eclass vendor tarball from a Cargo package.
//!
//! Standalone binary `cargo-eb` (Cargo applet lookup), not an `em` applet.

pub mod archive;
pub mod cargo;
pub mod ebuild;
pub mod fetch;
pub mod license;
pub mod vendor;
