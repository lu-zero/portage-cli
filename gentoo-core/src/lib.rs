//! Core Gentoo types and utilities
//!
//! This crate provides efficient types for working with Gentoo architecture
//! keywords and release media variants. It is designed for use in tools that
//! process large numbers of ebuilds or repository metadata, where string
//! deduplication provides measurable memory savings.
//!
//! # Architecture Handling
//!
//! [`Arch`] represents a CPU architecture, mapping to the corresponding Gentoo
//! keyword when the architecture is known (e.g., `"amd64"`, `"arm64"`). Unknown
//! or overlay-specific architectures are stored as opaque strings.
//!
//! [`KnownArch`] enumerates the 18 architectures officially supported by Gentoo,
//! providing zero-cost representation and additional metadata like bitness.
//!
//! # Release Media Variants
//!
//! [`Variant`] represents the `{arch}-{tag}` format used for Gentoo release
//! media (stage3 tarballs, ISO images). The tag typically encodes the init
//! system and profile (e.g., `"amd64-openrc"`, `"arm64-systemd"`).
//!
//! # Interning
//!
//! Both `Arch` and `Variant` use string interning to reduce memory usage when
//! processing many instances (e.g., parsing an entire ebuild repository).
//! With the default `interner` feature, identical strings share a single
//! allocation via a process-global interners.
#![warn(missing_docs)]

pub mod arch;
mod error;
pub mod interner {
    //! String interning for efficient string storage
    pub use gentoo_interner::*;
}
pub mod variant;

pub use error::Error;

pub use arch::KnownArch;

/// A Gentoo architecture, either well-known or overlay-defined
pub type Arch = arch::Arch<interner::DefaultInterner>;

/// A Gentoo release media variant (e.g., `amd64-openrc`, `arm64-systemd`)
pub type Variant = variant::Variant<interner::DefaultInterner>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arch_alias() {
        let a = Arch::from_chost("aarch64").unwrap();

        println!("{a:?}");
    }
}
