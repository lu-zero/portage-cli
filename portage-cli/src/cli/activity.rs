//! Activity-output flags: where activity/progress events go during a merge
//! (`emerge.rs`) or metadata regen (`regen.rs`). As opposed to merge-*behaviour*
//! flags ([`super::MergeFlags`]) or root-model flags (`--root`, `--local`, …,
//! [`super::Topology`]/[`super::RootArg`]).
//!
//! Flattened into each applet that actually drives an activity bus — `em
//! emerge`, `em regen`, and the crossdev staged-build applets
//! (`ToolchainArgs`/`CrossdevArgs`/`StagesArgs`/`SetupArgs`, whose driver runs
//! each step through the same `emerge_atoms` chain) — exactly once each;
//! [`super::Cli::effective_activity`] just selects the active applet's copy,
//! there being no second, top-level copy left to reconcile against.
//!
//! Lives here (not `global = true` on `Cli`): these flags only mean something
//! to a command that spins up an activity bus. Making them global inherited
//! that meaninglessness into every subcommand's args — `em news --activity-fd`
//! or `em grep --emergelog` parsed fine but did nothing. Keeping them on the
//! relevant applets' own flattened copy removes that trap (same reasoning as
//! `--ask`'s move off `global = true`, see [`super::MergeFlags::ask`]).

/// Activity-output sink selection for a merge / regen run
#[derive(clap::Args, Debug, Clone, Default)]
pub struct ActivityArgs {
    /// Write activity events as JSONL to file descriptor N (subprocess front-ends)
    ///
    /// Takes ownership of the FD.
    #[arg(long = "activity-fd", value_name = "N")]
    pub activity_fd: Option<i32>,

    /// Append activity events as JSONL to PATH (not `-`; use `--activity-fd`)
    #[arg(long = "activity-jsonl", value_name = "PATH")]
    pub activity_jsonl: Option<String>,

    /// Dual-write Portage-compatible emerge.log lines (opt-in; qlop/genlop)
    /// Path defaults to `<merge-root>/var/log/emerge.log` (or
    /// `/var/log/emerge.log`).
    #[arg(long = "emergelog", env = "EM_EMERGELOG")]
    pub emergelog: bool,
}
