//! Small, single-purpose flag mixins flattened onto the specific applets that
//! need each one — the same convention [`super::RootArg`] already follows,
//! extended to `--verbose`/`--quiet`/`--arch`/`--repo`. None of these live on
//! [`crate::cli::Cli`] itself: the root stays a minimal dispatcher (subcommand
//! list, `--color`, `--help`, `--version`), and each flag is recognized only
//! where an applet actually reads it. The inner field is `global` so it still
//! cascades into an applet's own nested subcommands (e.g. `em maint sync
//! --quiet`), matching `RootArg`'s `root` field.
//!
//! `em __worker` gets its own plain `quiet` field instead of [`QuietArg`] —
//! it's machine-generated argv from `privilege.rs`'s re-exec, not something a
//! person types, and already avoids sharing spellings with global concepts
//! elsewhere (`--worker-config-root` instead of reusing [`super::Topology`]).

use super::default_arch;
use gentoo_core::Arch;

/// `-v`/`--verbose` (count), for applets whose output has phases/logs worth
/// labeling.
#[derive(usage::Args, Debug, Clone, Default)]
pub struct VerboseArg {
    /// Increase verbosity: `-v` labels each build phase, `-vv`/`-vvv` add
    /// `em`'s own debug/trace logs (see also `RUST_LOG`).
    #[usage(short = 'v', long, count, global)]
    pub verbose: u8,
}

/// `-q`/`--quiet`, for applets whose output can be suppressed.
#[derive(usage::Args, Debug, Clone, Default)]
pub struct QuietArg {
    /// Suppress non-error output
    #[usage(short = 'q', long, global)]
    pub quiet: bool,
}

/// `-p`/`--pretend`, for applets that can preview their action instead of
/// performing it.
///
/// Prefix position (`em -p pkg`) only reaches the *default* subcommand once
/// this moves off root — see the caveat on `Cli` about a flag committing the
/// line before a named applet is considered. `em -p toolchain` no longer
/// selects `toolchain`; write `em toolchain -p` instead.
#[derive(usage::Args, Debug, Clone, Default)]
pub struct PretendArg {
    /// Show what would be done without actually performing any actions
    #[usage(short = 'p', long, global)]
    pub pretend: bool,
}

/// `--arch`, for applets whose logic branches on target architecture.
#[derive(usage::Args, Debug, Clone)]
pub struct ArchArg {
    /// Target architecture for operations
    #[usage(
        long,
        global,
        value_name = "ARCH",
        default_fn = default_arch,
        default_note = "current system architecture"
    )]
    pub arch: Arch,
}

impl Default for ArchArg {
    fn default() -> Self {
        Self {
            arch: default_arch(),
        }
    }
}

/// `--repo`, for applets that read from a single, possibly-overridden repository.
#[derive(usage::Args, Debug, Clone, Default)]
pub struct RepoArg {
    /// Pin search/query to a single repository
    ///
    /// When unset, repositories are auto-discovered from `repos.conf` (the main repo wins for
    /// single-repo applets; search walks all of them).
    #[usage(long, global, value_name = "PATH", value_hint = usage::ValueHint::DirPath)]
    pub repo: Option<String>,
}

/// `--vdb`, for applets that query an alternate installed-package database.
///
/// Deliberately narrow: this only overrides the *read-only query* path
/// (`vdb.rs`'s `open_cli_vdb`) used by applets that inspect installed
/// packages. The real install/merge write path (`ebuild.rs`'s
/// `vdb_root_for`) always computes the VDB location from `--root`/`--prefix`/
/// `--local` and never consults this — giving it to e.g. `em ebuild`/
/// `em toolchain` would parse but do nothing, since neither reads it.
#[derive(usage::Args, Debug, Clone, Default)]
pub struct VdbArg {
    /// Override VDB path (default: $ROOT/var/db/pkg)
    #[usage(long, global, value_name = "PATH", value_hint = usage::ValueHint::DirPath)]
    pub vdb: Option<String>,
}

/// `--format`, for subcommands whose output has a genuinely structured shape
/// (a matrix, a multi-field record, a list of pass/fail results) worth
/// offering as JSON alongside the human-readable default. A lighter sibling
/// of `query::depgraph`'s own `DepgraphFormat` (which also has `tree`, a
/// shape specific to dependency graphs) for the common two-way case;
/// extend `OutputFormat` with more variants here if another shape becomes
/// useful across more than one subcommand.
#[derive(usage::Args, Debug, Clone, Default)]
pub struct FormatArg {
    #[usage(long, value_enum, default = "pretty")]
    pub format: OutputFormat,
}

/// See [`FormatArg`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, usage::ValueEnum)]
pub enum OutputFormat {
    /// Human-readable text (default)
    #[default]
    Pretty,
    /// Machine-parsable JSON
    Json,
}
