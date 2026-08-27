//! Resolution-policy and plan layer for the
//! [`em`](https://github.com/lu-zero/portage-cli) Portage CLI
//!
//! Turns repository facts + configuration into a solved, ordered merge plan
//! via the [`portage-atom-pubgrub`](https://crates.io/crates/portage-atom-pubgrub)
//! solver bridge — USE/keyword/mask policy, root-aware post-solve trimming,
//! and plan assembly — currently being migrated out of `portage-cli`'s
//! `query::depgraph` module in stages.
//! Computes policy; renders nothing (no clap, no anstream/anstyle dependency
//! — that boundary is deliberate).
//!
//! Unpublishable past its placeholder `v0.0.1` (see `Cargo.toml`): depends on
//! `portage-repo`, which pulls in the brush fork via git.
#![warn(missing_docs)]

/// Toolchain-sysroot build copies (the board-root topology, `--target T
/// --root R`): a post-solve closure walk inserting `MergeRoot::Base`
/// `DEPEND` copies into the shared crossdev sysroot, sibling to
/// `host_copies`
pub mod base_copies;
mod bdepend_avail;
/// Post-solve trim: drop plan entries only pulled for `BDEPEND` already
/// satisfied on BROOT or by earlier within-run merges
pub mod bdepend_trim;
#[cfg(test)]
mod c7;
/// Post-solve reverse-dependency conflict detection against installed
/// packages the plan doesn't replace
pub mod conflicts;
/// Post-solve trim: drop plan entries only pulled for `DEPEND` already
/// satisfied on the sysroot (`ESYSROOT`)
pub mod depend_trim;
/// Per-package download size (bytes not already present in `DISTDIR`) for the
/// `-v`/verbose `-p` report
pub mod download_size;
/// Effective per-package USE after profile/env overrides, IUSE defaults, and
/// `--autosolve-use` ceded flags
pub mod effective_use;
/// Profile USE `use.force`/`use.mask` (global and per-package), applied as
/// the unconditional post-fold step real portage uses
pub mod force_mask;
/// Native-offset host build-copies (Tier 1 `--root` for a Gentoo host): a
/// post-solve closure walk inserting `MergeRoot::Host` build-time copies the
/// solver's single-rooted Target solve can't itself account for
pub mod host_copies;
/// VDB-backed installed-package views (target ROOT, build-host BROOT, a
/// fixed sysroot) and the emerge-style action-tag computation
pub mod installed;
/// `package.use` entry synthesis and the cross-package `[flag]` USE-dep
/// co-solve fixpoint
pub mod package_use;
/// Repository-fact adaptation: the `PackageRepository` impl the solver
/// bridge consumes, plus keyword/mask/license acceptance
pub mod repo;
/// Post-solve `REQUIRED_USE` violation check against a package's effective USE
pub mod required_use;
/// Cross-compilation context detection and merge-root display glue
pub mod root_aware;
mod roots;
/// Slot-operator (`:=`) rebuild detection
pub mod subslot;
/// Config/profile reading into the resolved [`use_env::UseEnv`] the rest of
/// this crate's policy folding runs on
pub mod use_env;
/// `--newuse` / `--changed-use` reinstall detection (USE/IUSE drift)
pub mod use_reinstall;

pub use bdepend_avail::{Avail, broot_vdb_packages, collect_unsatisfied, unsatisfied_cpns};
pub use roots::Roots;
