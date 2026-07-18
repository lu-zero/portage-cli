//! Gentoo Portage command-line library backing the `em` binary.

pub(crate) mod binpkg;
pub mod cli;
pub(crate) mod crossdev;
pub(crate) mod depclean;
pub(crate) mod dispatch;
pub(crate) mod ebuild;
/// Install-image ELF scan (`NEEDED` / `NEEDED.ELF.2` generation).
pub mod elfscan;
pub(crate) mod emerge;
pub(crate) mod error;
pub(crate) mod maint;
pub(crate) mod merge;
pub(crate) mod pkg;
pub(crate) mod postprocess;
pub(crate) mod preflight;
pub(crate) mod preserve_libs;
pub mod privilege;
pub(crate) mod query;
pub(crate) mod quickpkg;
pub(crate) mod regen;
pub(crate) mod search;
pub(crate) mod select;
pub(crate) mod setup;
pub(crate) mod style;
pub(crate) mod use_flags;
pub(crate) mod util;
pub(crate) mod vdb;

pub(crate) use emerge::{EmergeOpts, emerge_atoms};
pub use error::ConfigChangesNeeded;

/// Dispatch one parsed invocation to its applet or the default emerge path.
pub async fn run(cli: &cli::Cli) -> error::Result<()> {
    dispatch::run(cli).await
}
