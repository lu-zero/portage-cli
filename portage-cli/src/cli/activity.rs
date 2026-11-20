//! Activity-output flags: where activity/progress events go during a merge
//! (`emerge.rs`) or metadata regen (`regen.rs`). As opposed to merge-*behaviour*
//! flags ([`super::MergeFlags`]) or root-model flags (`--root`, `--local`, …,
//! [`super::Topology`]/[`super::RootArg`]).
//!
//! Flattened onto each applet that drives an activity bus — `em emerge`,
//! `em regen`, and the staged-build applets. Not global. Env `EM_EMERGELOG`
//! is read once in [`super::Cli::effective_activity`].

/// Activity-output sink selection for a merge / regen run
#[derive(winnow_args::Args, Debug, Clone, Default, PartialEq)]
#[arg(
    next_help_heading = "Activity",
    heading("Activity", help = "Where live progress is written.")
)]
pub struct ActivityArgs {
    /// Write activity events as JSONL to file descriptor N (subprocess front-ends).
    /// The FD is duplicated, so the caller keeps it open.
    #[arg(long = "activity-fd", value_name = "N")]
    pub activity_fd: Option<i32>,

    /// Append activity events as JSONL to PATH (not `-`; use `--activity-fd`)
    #[arg(long = "activity-jsonl", value_name = "PATH")]
    pub activity_jsonl: Option<String>,

    /// Dual-write Portage-compatible emerge.log lines (opt-in; qlop/genlop)
    /// Path defaults to `<merge-root>/var/log/emerge.log` (or
    /// `/var/log/emerge.log`).
    #[arg(long = "emergelog")]
    pub emergelog: bool,
}
