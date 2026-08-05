//! Activity-output flags: where activity/progress events go during a merge
//! (`emerge.rs`) or metadata regen (`regen.rs`). As opposed to merge-*behaviour*
//! flags ([`super::MergeFlags`]) or root-model flags (`--root`, `--local`, …,
//! already `global = true` on [`super::Cli`]).
//!
//! Flattened both into the top-level [`super::Cli`] (for the bare `em <atoms>`
//! merge path) and into the applets that actually drive an activity bus —
//! `em regen` and the crossdev staged-build applets (`ToolchainArgs`/
//! `CrossdevArgs`/`StagesArgs`, whose driver runs each step through the same
//! `emerge_atoms` chain). Mirrors [`super::MergeFlags`]'s own flattening, so a
//! flag works either before or after the subcommand name; the driver merges the
//! two via [`super::Cli::effective_activity`] (subcommand-wins-when-set), the
//! same precedence [`super::Cli`] uses for merge flags.
//!
//! Lives here (not `global = true` on `Cli`): these flags only mean something
//! to a command that spins up an activity bus. Making them global inherited
//! that meaninglessness into every subcommand's args — `em news --activity-fd`
//! or `em grep --emergelog` parsed fine but did nothing. Keeping them on the
//! relevant applets' own flattened copy removes that trap (same reasoning as
//! `--ask`'s move off `global = true`, see [`super::MergeFlags::ask`]).

/// Activity-output sink selection for a merge / regen run.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct ActivityArgs {
    /// Write activity events as JSONL to file descriptor N (subprocess
    /// front-ends). Takes ownership of the FD.
    #[arg(long = "activity-fd", value_name = "N")]
    pub activity_fd: Option<i32>,

    /// Append activity events as JSONL to PATH (not `-`; use `--activity-fd`).
    #[arg(long = "activity-jsonl", value_name = "PATH")]
    pub activity_jsonl: Option<String>,

    /// Dual-write Portage-compatible emerge.log lines (opt-in; qlop/genlop).
    /// Path defaults to `<merge-root>/var/log/emerge.log` (or
    /// `/var/log/emerge.log`).
    #[arg(long = "emergelog", env = "EM_EMERGELOG")]
    pub emergelog: bool,
}

/// Pure field-merge core: `over` takes precedence over `base` — `Option`s pick
/// the subcommand's value when set, `emergelog` is a plain OR. Factored out
/// (like [`super::merge_merge_flags_fields`]) so the merge has a single
/// definition shared by [`super::Cli::effective_activity`] and any future
/// caller that merges a persisted set with a live invocation's.
pub fn merge_activity_args_fields(base: &ActivityArgs, over: &ActivityArgs) -> ActivityArgs {
    ActivityArgs {
        activity_fd: over.activity_fd.or(base.activity_fd),
        activity_jsonl: over
            .activity_jsonl
            .clone()
            .or_else(|| base.activity_jsonl.clone()),
        emergelog: over.emergelog || base.emergelog,
    }
}
