//! Gentoo Portage command-line library backing the `em` binary.

// Structured merge activity events (library channel + on-disk sinks).
// Activity status design is documented in todo/activity-status.md.
/// Persistent active `--prefix`/`--local` registration (`em active`).
pub(crate) mod active;
pub mod activity;
pub(crate) mod binpkg;
pub mod cli;
pub(crate) mod crossdev;
pub(crate) mod depclean;
pub mod diag;
pub(crate) mod dispatch;
pub(crate) mod ebuild;
/// Install-image ELF scan (`NEEDED` / `NEEDED.ELF.2` generation).
pub mod elfscan;
pub(crate) mod emerge;
pub(crate) mod error;
/// Pure-gix helpers (hard-reset composition; candidate for upstream gitoxide).
/// Only built with feature `sync-gix`.
#[cfg(feature = "sync-gix")]
pub(crate) mod gix_ext;
pub(crate) mod maint;
pub(crate) mod merge;
pub(crate) mod mirrordist;
pub(crate) mod pkg;
pub(crate) mod postprocess;
pub(crate) mod preflight;
pub(crate) mod preserve_libs;
pub mod privilege;
pub(crate) mod query;
pub(crate) mod quickpkg;
pub(crate) mod regen;
/// Open repos with em's durable user md5-cache root.
pub(crate) mod repo_open;
pub(crate) mod revdep;
pub(crate) mod search;
pub(crate) mod select;
pub(crate) mod setup;
pub(crate) mod style;
#[cfg(test)]
pub(crate) mod test_support;
pub(crate) mod use_flags;
pub(crate) mod util;
pub(crate) mod vdb;
/// Minimal XDG Base Directory helpers (`$XDG_CACHE_HOME` / `$XDG_STATE_HOME`).
pub(crate) mod xdg;

pub use activity::{
    ActivityBus, ActivityEvent, ActivitySessionOpts, DurationStore, LiveProjection, RecordingSink,
    estimate_remaining, estimate_remaining_with_blockers,
};
pub(crate) use emerge::{EmergeOpts, emerge_atoms};
pub use error::{ConfigChangesNeeded, NoValidAtoms};

/// Dispatch one parsed invocation to its applet or the default emerge path.
pub async fn run(cli: &cli::Cli) -> error::Result<()> {
    dispatch::run(cli).await
}

/// Print a fatal, top-level `anyhow::Error` the same way every other
/// `!!!`-styled diagnostic in `em` looks (`style::error_line`) — `main.rs`'s
/// sink for whatever an applet's `?`/`bail!` propagated all the way up.
/// `style` itself is `pub(crate)`, so `main.rs` (a separate binary crate)
/// needs this thin public wrapper rather than reaching in directly.
///
/// Formats with `{:#}` (anyhow's alternate form), not `Display::to_string`,
/// so the full `Caused by: …` context chain still prints, not just the
/// top-level message.
pub fn print_fatal_error(e: &anyhow::Error) {
    style::error_line!("{e:#}");
}

/// Same styling as [`print_fatal_error`], for a startup failure (no
/// `anyhow::Error` yet to hand it — e.g. the tokio runtime itself failed to
/// build) where a plain [`std::fmt::Display`] is all there is.
pub fn print_fatal_error_display(e: impl std::fmt::Display) {
    style::error_line!("{}", e);
}
