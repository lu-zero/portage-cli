//! `em emerge`'s own mode switches — `-s`/`-S`/`-O`/`-C`/`-c`/`-P`/`-W`/`-r`.
//!
//! Flattened onto [`super::EmergeArgs`] only. Not global: `-s`/`-C`/`-c` are
//! emerge actions. Prefix-position `em -r` reaches these via the emerge retry.

/// Emerge's own action-selecting mode switches.
#[derive(winnow_args::Args, Debug, Clone, Default, PartialEq)]
#[arg(
    next_help_heading = "Actions",
    heading("Actions", help = "Pick a different operation than a merge.")
)]
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

    /// `n`: match descriptions from the md5-cache, not `metadata/pkg_desc_index`
    #[arg(long, value_name = "y|n")]
    pub search_index: Option<super::YesNo>,

    /// `n`: a name matches only by its text, not also by similarity
    #[arg(long, value_name = "y|n")]
    pub fuzzy_search: Option<super::YesNo>,

    /// The percentage of similarity, 0 to 100, at which a name matches (default 80)
    #[arg(long, value_name = "PERCENT")]
    pub search_similarity: Option<f64>,

    /// `n`: a key is a regular expression only after `%`, not when it looks like one
    #[arg(long, value_name = "y|n")]
    pub regex_search_auto: Option<super::YesNo>,

    /// Skip dependency resolution and only merge specified packages
    #[arg(short = 'O', long)]
    pub nodeps: bool,

    /// Remove the matching installed packages completely, without regard to dependencies
    ///
    /// Matches every installed slot/version of each atom. For removing unneeded dependencies
    /// too, use `depclean` instead.
    #[arg(short = 'C', long, effect = "destructive")]
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
    #[arg(short = 'c', long, effect = "destructive")]
    pub depclean: bool,

    /// Remove all but the highest installed version of each atom given,
    /// ignoring dependencies (real emerge's own historical caveat applies —
    /// prefer `--depclean` for a dependency-aware clean).
    #[arg(short = 'P', long, effect = "destructive")]
    pub prune: bool,

    /// Remove atoms and/or `@set`s from the world file, without unmerging
    /// anything.
    #[arg(short = 'W', long)]
    pub deselect: bool,

    /// Resume the last saved merge (see `em maint cleanresume` to discard it instead)
    ///
    /// Atoms are not accepted together with this flag — the package list comes from the saved
    /// state. Combine with other flags (e.g. `-r -X stuck/atom`) to adjust the resumed run.
    #[arg(short = 'r', long)]
    pub resume: bool,

    /// Print system/build info, and how installed atoms were built. Never merges.
    #[arg(
        long,
        long_help = "`emerge --info` workalike. Never merges. Atoms add how each installed match was built. Print system/build info: profile, CHOST/CFLAGS/FEATURES/USE (with USE_EXPAND groups like VIDEO_CARDS broken out), ACCEPT_KEYWORDS/ACCEPT_LICENSE, and configured repositories. Combine with `--json` for structured output, or `-v` to also list every known `@name` set and its resolved atoms."
    )]
    pub info: bool,
}

/// The operation an emerge run performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmergeAction {
    /// `-C`: remove matches without regard to dependencies
    Unmerge,
    /// `-c`: remove what `@world` does not need
    Depclean,
    /// `-P`: remove all but the highest installed version
    Prune,
    /// `-s`/`-S`: search, `descriptions` for `-S`
    Search {
        /// Whether descriptions are matched too
        descriptions: bool,
    },
    /// `-W`: edit the world file only
    Deselect,
    /// `-r`: replay the saved merge
    Resume,
    /// No action flag: merge the atoms
    Merge,
}

impl EmergeModeArgs {
    /// The selected action. When several flags are set, the first in [`EmergeAction`] order wins.
    pub fn action(&self) -> EmergeAction {
        if self.unmerge {
            EmergeAction::Unmerge
        } else if self.depclean {
            EmergeAction::Depclean
        } else if self.prune {
            EmergeAction::Prune
        } else if self.search || self.searchdesc {
            EmergeAction::Search {
                descriptions: self.searchdesc,
            }
        } else if self.deselect {
            EmergeAction::Deselect
        } else if self.resume {
            EmergeAction::Resume
        } else {
            EmergeAction::Merge
        }
    }

    /// The set flags among those emerge refuses to combine; `-r` and `-W` are options there.
    pub(super) fn exclusive_actions(&self) -> Vec<&'static str> {
        [
            (self.unmerge, "--unmerge"),
            (self.depclean, "--depclean"),
            (self.prune, "--prune"),
            (self.search || self.searchdesc, "--search"),
            (self.info, "--info"),
        ]
        .into_iter()
        .filter_map(|(set, flag)| set.then_some(flag))
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_follows_dispatch_precedence() {
        let mode = EmergeModeArgs {
            resume: true,
            unmerge: true,
            ..Default::default()
        };
        assert_eq!(mode.action(), EmergeAction::Unmerge);
        let mode = EmergeModeArgs {
            deselect: true,
            search: true,
            ..Default::default()
        };
        assert_eq!(
            mode.action(),
            EmergeAction::Search {
                descriptions: false
            }
        );
        let mode = EmergeModeArgs {
            resume: true,
            deselect: true,
            ..Default::default()
        };
        assert_eq!(mode.action(), EmergeAction::Deselect);
        let mode = EmergeModeArgs {
            search: true,
            searchdesc: true,
            ..Default::default()
        };
        assert_eq!(mode.action(), EmergeAction::Search { descriptions: true });
        assert_eq!(EmergeModeArgs::default().action(), EmergeAction::Merge);
    }

    #[test]
    fn only_emerge_actions_are_exclusive() {
        let mode = EmergeModeArgs {
            unmerge: true,
            depclean: true,
            resume: true,
            ..Default::default()
        };
        assert_eq!(mode.exclusive_actions(), ["--unmerge", "--depclean"]);
        let mode = EmergeModeArgs {
            search: true,
            searchdesc: true,
            deselect: true,
            ..Default::default()
        };
        assert_eq!(mode.exclusive_actions(), ["--search"]);
    }
}
