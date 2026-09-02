//! cargo-ebuild — pycargoebuild replacement as a `portage-cli` workspace crate.
//!
//! Separate binary `pycargoebuild-rs` (not an `em` applet) that generates
//! `CRATES`/`GIT_CRATES` ebuilds and `cargo_home/gentoo` tarballs from
//! `Cargo.lock`/`Cargo.toml`. Uses `minijinja` to keep the Jinja2 template
//! verbatim from `pycargoebuild/ebuild.py:EBUILD_TEMPLATE`.

pub mod cargo;
pub mod ebuild;
pub mod fetch;
pub mod license;
pub mod vendor;

pub use cargo::{Crate, FileCrate, GitCrate, GitHost, PackageMetadata};
