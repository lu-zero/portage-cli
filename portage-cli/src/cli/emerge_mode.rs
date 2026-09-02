//! `em emerge`'s own mode switches — `-s`/`-S`/`-O`/`-C`/`-c`/`-P`/`-W`/`-r`.
//!
//! Flattened only into [`super::EmergeArgs`]: these select which *action*
//! `em emerge` takes (search, unmerge, depclean, prune, deselect, resume)
//! rather than describing how a merge behaves ([`super::MergeFlags`]) —
//! `merge_flags.rs`'s own doc comment already explains why `--search`/
//! `--nodeps` don't belong there. Meaningless anywhere else, so — unlike
//! `--pretend`/`--verbose`/`--quiet` — these are not `global = true` on
//! `Cli`.

/// Emerge's own action-selecting mode switches.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct EmergeModeArgs {
    /// Search package names (each argument is a pattern)
    ///
    /// Deliberately separate from the `em search` applet: this is emerge's
    /// own `-s`, emerge-style output; `em search` is the equery-style
    /// applet (`--all`/`--desc`/`--name-only`/`--homepage`). Same split as
    /// real Portage's `emerge -s` vs `equery`, not accidental duplication.
    // Drives `crate::search::run_emerge_style`; the applet drives `crate::search::run`.
    #[arg(short = 's', long)]
    pub search: bool,

    /// Search package names and descriptions
    ///
    /// Same split as `-s`/`em search` above: emerge-style output here,
    /// `em search --desc` is the equery-style applet.
    #[arg(short = 'S', long)]
    pub searchdesc: bool,

    /// Skip dependency resolution and only merge specified packages
    #[arg(short = 'O', long)]
    pub nodeps: bool,

    /// Remove the matching installed packages completely, without regard to dependencies
    ///
    /// Matches every installed slot/version of each atom. For removing unneeded dependencies
    /// too, use `depclean` instead.
    #[arg(short = 'C', long)]
    pub unmerge: bool,

    /// Remove installed packages that are not needed by @world (with no
    /// atoms, cleans everything unreachable; with atoms, only considers
    /// removing those, protecting everything else). Unlike `-C`, this walks
    /// the installed dependency graph first — matches real emerge's safe
    /// alternative to `-C`.
    ///
    /// Same behavior as the `em depclean [atoms]` applet — this flag exists
    /// for scripting convenience within a single `emerge`-style invocation.
    // crate::depclean::run forwards to run_with_targets — identical
    // implementation, not merely similar behavior.
    #[arg(short = 'c', long)]
    pub depclean: bool,

    /// Remove all but the highest installed version of each atom given,
    /// ignoring dependencies (real emerge's own historical caveat applies —
    /// prefer `--depclean` for a dependency-aware clean).
    #[arg(short = 'P', long)]
    pub prune: bool,

    /// Remove atoms and/or `@set`s from the world file, without unmerging
    /// anything.
    #[arg(short = 'W', long)]
    pub deselect: bool,

    /// Resume the last saved merge (see `em maint cleanresume` to discard it instead)
    ///
    /// Atoms are not accepted together with this flag — the package list comes from the saved
    /// state. Combine with other flags (e.g. `-r --keep-going`, `-r -X stuck/atom`) to adjust
    /// the resumed run.
    #[arg(short = 'r', long)]
    pub resume: bool,
}
