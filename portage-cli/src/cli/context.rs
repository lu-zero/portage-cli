//! Small single-purpose flag mixins, each flattened only onto the applets that
//! read it (`--verbose`/`--quiet`/`--arch`/`--repo`, like [`super::RootArg`]
//! before them). The root [`crate::cli::Cli`] stays a minimal dispatcher:
//! subcommand list, `--color`, `--help`, `--version`. Inner fields are `global`
//! so they cascade into an applet's nested subcommands (`em maint sync --quiet`).
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
/// Deliberately narrow: overrides only the *read-only query* path
/// (`vdb.rs`'s `open_cli_vdb`) for applets that inspect installed packages.
/// The install/merge write path (`ebuild.rs`'s `vdb_root_for`) always derives
/// the VDB from `--root`/`--prefix`/`--local`, so this flag on `em ebuild` or
/// `em toolchain` would parse but do nothing.
#[derive(usage::Args, Debug, Clone, Default)]
pub struct VdbArg {
    /// Override VDB path (default: $ROOT/var/db/pkg)
    #[usage(long, global, value_name = "PATH", value_hint = usage::ValueHint::DirPath)]
    pub vdb: Option<String>,
}

/// `--format`, for subcommands whose output is a genuinely structured shape
/// (a matrix, a multi-field record, a list of pass/fail results) worth
/// offering as JSON beside the human-readable default. The two-way sibling
/// of `query::depgraph`'s `DepgraphFormat`, which also has `tree`.
#[derive(usage::Args, Debug, Clone, Default)]
#[usage(
    output("pretty", default, help = "Human-readable text"),
    output("json", framing = "json", help = "Machine-parsable JSON")
)]
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
