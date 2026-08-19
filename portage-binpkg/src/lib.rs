//! Gentoo binary package (GPKG) reading and writing
//!
//! Provides the [GLEP 78](https://www.gentoo.org/glep/glep-0078.html) GPKG
//! container **writer** ([`write_gpkg`]) and **metadata reader**
//! ([`read_metadata`]); the binhost `Packages` **index** format and
//! USE-reuse matching ([`index`]); container discovery/checksumming and
//! index regeneration ([`scan`], [`regen`]); and `PKGDIR` **maintenance**
//! operations — verify/list/prune ([`maint`]). Signing lives here as it
//! lands. Used by the [`em`](https://github.com/lu-zero/portage-cli) Portage
//! CLI, which owns everything that needs `&Cli`/`make.conf` (`PKGDIR`
//! resolution, `binrepos.conf`) — this crate deliberately has no such
//! dependency, so it stays usable standalone.
//!
//! The writer shells out to GNU `tar` and `zstd` (so file capabilities,
//! ACLs and device nodes in the image survive natively), matching
//! Portage's own approach.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod error;
pub mod gpg;
pub mod gpkg;
pub mod index;
pub mod maint;
pub mod regen;
pub mod scan;

pub use error::{Error, Result};
pub use gpg::{Keyring, SigningKey};
pub use gpkg::{
    GpkgInput, SignatureReport, VerifyPolicy, extract_image, read_metadata,
    verify_container_signature, write_gpkg,
};
pub use index::{
    BinpkgEntry, BinpkgIndex, RemoteBinpkgIndex, build_env_key, parse_index_blocks,
    parse_index_header, parse_packages_entries, use_compatible,
};
pub use regen::index_pkgdir;
pub use scan::{checksum, find_gpkg_containers, parse_build_id_from_name};
