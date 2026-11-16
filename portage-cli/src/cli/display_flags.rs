//! Display switches `emerge` accepts that `em` has no use for.
//!
//! They parse so a command line written for `emerge` runs unchanged; none changes what `em` does.

/// Switches that shape `emerge`'s output or progress display, accepted and ignored
#[derive(winnow_args::Args, Debug, Clone, Default, PartialEq)]
#[arg(
    next_help_heading = "Accepted, no effect",
    heading(
        "Accepted, no effect",
        help = "Output switches `emerge` has, which `em` takes and ignores."
    )
)]
pub struct DisplayArgs {
    /// Sort the enabled and disabled USE flags of the plan together
    #[arg(long)]
    pub alphabetical: bool,

    /// Print the plan as columns
    #[arg(long)]
    pub columns: bool,

    /// Do not draw a progress spinner
    #[arg(long)]
    pub nospinner: bool,

    /// Do not show the repository of each package
    #[arg(long)]
    pub quiet_repo_display: bool,

    /// Do not order the plan for display
    #[arg(long)]
    pub unordered_display: bool,

    /// Explain every blocker in detail
    #[arg(long)]
    pub verbose_conflicts: bool,

    /// Report each missing ebuild, not only the first
    #[arg(long)]
    pub verbose_missing_ebuilds: bool,

    /// Send build output to the log
    #[arg(long, value_name = "y|n", default_missing = "y", require_equals)]
    pub quiet_build: Option<super::YesNo>,

    /// Print only the die message of a failed build
    #[arg(long, value_name = "y|n", default_missing = "y", require_equals)]
    pub quiet_fail: Option<super::YesNo>,

    /// Show the slot rebuilds a plan makes
    #[arg(long, value_name = "y|n", default_missing = "y", require_equals)]
    pub verbose_slot_rebuilds: Option<super::YesNo>,

    /// Suggest names for an atom that matches nothing
    #[arg(long, value_name = "y|n", default_missing = "y", require_equals)]
    pub misspell_suggestions: Option<super::YesNo>,
}
