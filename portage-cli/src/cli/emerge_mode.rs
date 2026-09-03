//! `em emerge`'s own mode switches — `-s`/`-S`/`-O`/`-C`/`-c`/`-P`/`-W`/`-r`.
//!
//! Flattened onto [`super::Cli`] (prefix-position) and [`super::EmergeArgs`]
//! only. Not global: `-s`/`-C`/`-c` are emerge actions.

/// Emerge's own action-selecting mode switches.
#[derive(usage::Args, Debug, Clone, Default, PartialEq)]
#[usage(next_help_heading = "Merge")]
pub struct EmergeModeArgs {
    /// Search package names (each argument is a pattern)
    ///
    /// Deliberately separate from the `em search` applet: this is emerge's
    /// own `-s`, emerge-style output; `em search` is the equery-style
    /// applet (`--all`/`--desc`/`--name-only`/`--homepage`). Same split as
    /// real Portage's `emerge -s` vs `equery`, not accidental duplication.
    // Drives `crate::search::run_emerge_style`; the applet drives `crate::search::run`.
    #[usage(short = 's', long)]
    pub search: bool,

    /// Search package names and descriptions
    ///
    /// Same split as `-s`/`em search` above: emerge-style output here,
    /// `em search --desc` is the equery-style applet.
    #[usage(short = 'S', long)]
    pub searchdesc: bool,

    /// Skip dependency resolution and only merge specified packages
    #[usage(short = 'O', long)]
    pub nodeps: bool,

    /// Remove the matching installed packages completely, without regard to dependencies
    ///
    /// Matches every installed slot/version of each atom. For removing unneeded dependencies
    /// too, use `depclean` instead.
    #[usage(short = 'C', long, effect = "destructive")]
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
    #[usage(short = 'c', long, effect = "destructive")]
    pub depclean: bool,

    /// Remove all but the highest installed version of each atom given,
    /// ignoring dependencies (real emerge's own historical caveat applies —
    /// prefer `--depclean` for a dependency-aware clean).
    #[usage(short = 'P', long, effect = "destructive")]
    pub prune: bool,

    /// Remove atoms and/or `@set`s from the world file, without unmerging
    /// anything.
    #[usage(short = 'W', long)]
    pub deselect: bool,

    /// Resume the last saved merge (see `em maint cleanresume` to discard it instead)
    ///
    /// Atoms are not accepted together with this flag — the package list comes from the saved
    /// state. Combine with other flags (e.g. `-r -X stuck/atom`) to adjust the resumed run.
    #[usage(short = 'r', long)]
    pub resume: bool,
}

impl EmergeModeArgs {
    /// Overlay a dual-mounted copy: bools OR.
    pub(crate) fn overlay(&self, applet: &Self) -> Self {
        Self {
            search: self.search || applet.search,
            searchdesc: self.searchdesc || applet.searchdesc,
            nodeps: self.nodeps || applet.nodeps,
            unmerge: self.unmerge || applet.unmerge,
            depclean: self.depclean || applet.depclean,
            prune: self.prune || applet.prune,
            deselect: self.deselect || applet.deselect,
            resume: self.resume || applet.resume,
        }
    }
}
