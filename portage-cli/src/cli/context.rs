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
