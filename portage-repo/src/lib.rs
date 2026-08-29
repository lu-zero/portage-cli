//! Gentoo ebuild repository layout reader based on the
//! [Package Manager Specification (PMS)](https://projects.gentoo.org/pms/9/pms.html).
//!
//! This crate provides types for reading and navigating a Gentoo ebuild
//! repository: `metadata/layout.conf`, category and package directory
//! enumeration, profiles, metadata cache access, and ebuild/eclass sourcing
//! via an embedded bash shell ([brush](https://crates.io/crates/brush-core)).
//!
//! # Quick start
//!
//! ```no_run
//! use portage_repo::Repository;
//!
//! let repo = Repository::builder()
//!     .in_memory_cache()
//!     .open("/var/db/repos/gentoo")
//!     .unwrap();
//! println!("repo: {} (masters: {:?})", repo.name(), repo.layout().masters);
//!
//! for cat in repo.categories() {
//!     for pkg in cat.packages() {
//!         for ebuild in pkg.ebuilds().unwrap() {
//!             println!("{}", ebuild.cpv());
//!         }
//!     }
//! }
//! ```
//!
//! # Crate family
//!
//! - [`portage-atom`](https://crates.io/crates/portage-atom) — PMS atom parser
//! - [`portage-metadata`](https://crates.io/crates/portage-metadata) — metadata cache types
//! - `portage-repo` (this crate) — repository layout reader
//!
//! > **Warning**: This codebase was largely AI-generated and has not yet been
//! > thoroughly audited. It may contain bugs, incomplete PMS coverage, or
//! > surprising edge-case behaviour. Use at your own risk.
#![warn(missing_docs)]

pub(crate) mod build;
pub mod cache;
pub mod entries;
mod error;
pub mod make_conf;
/// Abstract md5-cache storage (dir / memory) for [`Repository`]
pub mod metadata_cache;
pub mod package_conf;
pub mod package_env;
pub(crate) mod repo;
pub mod source;
pub mod userdb;

pub use build::ACTION_TARGET;
pub use build::EbuildEnv;
pub use build::inherit;

pub use error::{Error, Result};

// Re-export the most-used types at crate root for backwards compat
pub use build::{
    ConfSource, EbuildShell, PhaseSession, PortageColors, TerminalConfig, phase_path_dirs,
    run_helper,
};
pub use cache::{
    CacheReadOpts, RegenItem, RegenOpts, RegenStats, RegenWriteTarget, cache_cpvs,
    cache_entries_parallel, cache_entries_parallel_with_mtime, regen_cache,
};
pub use entries::{gap_entries, repo_entries};
pub use gentoo_core::arch::ExoticKey;
pub use gentoo_core::{Arch, KnownArch, arch};
pub use make_conf::{
    DEFAULT_MAKE_CONF, LEGACY_MAKE_CONF, MAKE_CONF_DIR_FALLBACK_FRAGMENT, MakeConf,
    expand_make_conf_paths,
};
pub use metadata_cache::{DirMetadataCache, MemoryMetadataCache, MetadataCache};
pub use package_conf::{PackageConf, Token as PackageToken};
pub use package_env::env_files_for;
pub use portage_metadata::EbuildMetadata;
pub use portage_metadata::interner::{
    DefaultInterner, GlobalInterner, Interned, Interner, NoInterner,
};
pub use repo::Ebuild;
pub use repo::LayoutConf;
pub use repo::Package;
pub use repo::UseDb;
pub use repo::UseExpand;
pub use repo::ini;
pub use repo::license_groups::{AcceptSet, LicenseGroupRegistry};
pub use repo::named_groups::{GROUP_PREFIX, group_ref_name, is_group_ref};
pub use repo::sets::{SetResolver, is_set_ref, is_world_candidate, set_name};
/// Directory-aware config line reader
///
/// PMS 5.2.4 dir-form: files concatenated in filename order, dotfiles and
/// `~` backups skipped. Shared with `/etc/portage` `package.*` consumers
/// so they match the profile stack exactly.
pub use repo::util::read_lines as read_config_lines;
/// Expand a Portage config path that may be a file or a directory of fragments
pub use repo::util::{
    ConfigFilesMode, config_basename_included, iter_config_files, list_config_files,
    resolve_repo_name,
};
pub use repo::{
    CacheEntries, CacheEntriesIter, Ebuilds, EbuildsIter, ProfileUpdate, Repository,
    RepositoryBuilder,
};
pub use repo::{Categories, CategoriesIter, Category, Packages, PackagesIter};
pub use repo::{EbuildIn, EbuildsAcross, EntryIn, RepoSet};
pub use repo::{Location, RepoEntry, ReposConf};
pub use repo::{Maintainer, MaintainerKind, PkgMetadata};
pub use repo::{Manifest, ManifestEntry};
pub use repo::{
    Profile, ProfileDesc, ProfileEnv, ProfileEnvLayer, ProfileStack, ProfileStatus, UseFlags,
};
pub use source::{SourceContext, SourceItem, SourceOpts, source_parallel, source_single};
