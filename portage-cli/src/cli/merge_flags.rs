//! Merge-behavior flags: everything `emerge_atoms`/`run_merge_plan` read to
//! decide *how* to resolve and build, as opposed to root-model flags
//! ([`super::Topology`]/[`super::RootArg`]) or depgraph-shape flags
//! ([`super::DepgraphFlags`]).
//!
//! Flattened onto each merge-shaped applet (`EmergeArgs`, `CrossdevArgs`,
//! `ToolchainArgs`, `StagesArgs`, `SetupArgs`, `Revdep`, `Depclean`). Not
//! global: `-a` must not reach `use`. Prefix-position `em -uD @world` reaches
//! these via the emerge retry, not a second flatten on [`super::Cli`].
//!
//! `--search`/`--searchdesc`/`--nodeps` are not here: they select a different
//! emerge *action* ([`super::EmergeModeArgs`]), or a per-step `EmergeOpts`
//! override in the staged bootstrap.
#[derive(
    winnow_args::Args, Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(default)]
#[arg(
    next_help_heading = "Merge",
    heading("Merge", help = "How the solver and build scheduler behave.")
)]
pub struct MergeFlags {
    /// Ask for confirmation before performing actions
    #[arg(short = 'a', long)]
    pub ask: bool,

    /// Update installed packages to newest available versions
    #[arg(short = 'u', long)]
    pub update: bool,

    /// Write required USE changes to /etc/portage/package.use/
    #[arg(long)]
    pub autounmask_write: bool,

    /// Build and install packages but do not add them to the world file
    #[arg(short = '1', long = "oneshot")]
    pub oneshot: bool,

    /// Only fetch distfiles, do not build or install
    #[arg(short = 'f', long)]
    pub fetchonly: bool,

    /// Instead of building, just fetch every SRC_URI file (regardless of
    /// USE setting) for the resolved packages.
    #[arg(short = 'F', long)]
    pub fetch_all_uri: bool,

    /// Build binary packages for all merged packages
    #[arg(short = 'b', long)]
    pub buildpkg: bool,

    /// Build binary packages without merging/installing them
    ///
    /// All build-time dependencies must already be satisfied on the system -- this does not
    /// resolve or install anything to make that true.
    #[arg(short = 'B', long)]
    pub buildpkgonly: bool,

    /// Use binary packages if available, otherwise fall back to source
    #[arg(short = 'k', long)]
    pub usepkg: bool,

    /// Only use binary packages, fail if none available
    #[arg(short = 'K', long)]
    pub usepkgonly: bool,

    /// Fetch binary packages for all requested packages
    #[arg(short = 'g', long)]
    pub getbinpkg: bool,

    /// Only fetch binary packages, do not install
    #[arg(short = 'G', long)]
    pub getbinpkgonly: bool,

    /// Treat every atom as not-yet-installed, rebuilding the whole dependency
    /// tree from scratch rather than only what is missing or outdated.
    #[arg(short = 'e', long)]
    pub emptytree: bool,

    /// Show the dependency tree, indenting each package under the one that
    /// pulled it in, before merging.
    #[arg(short = 't', long)]
    pub tree: bool,

    /// Emit the depgraph as machine-parsable JSON instead of pretend text
    ///
    /// Takes precedence over `--tree`. Works with `-p` (including `-e`).
    #[arg(long)]
    pub json: bool,

    /// Only merge dependencies, not the specified packages themselves
    #[arg(short = 'o', long)]
    pub onlydeps: bool,

    /// Do not replace installed packages that are already the same version
    #[arg(short = 'n', long)]
    pub noreplace: bool,

    /// Build up to N packages in parallel, respecting build-dependency order (merges are still
    /// serialised)
    ///
    /// Default 1 (sequential).
    #[arg(short = 'j', long, value_name = "N")]
    pub jobs: Option<u32>,

    /// Maximum 1-minute load average allowed when starting additional parallel builds (`--jobs`
    /// > 1)
    ///
    /// Once at least one job is running, further starts wait until load drops below LOAD
    /// (matches real Portage's own load-average throttling). The first concurrent job is
    /// always allowed. Displayed on the `Jobs:` status line regardless.
    #[arg(short = 'l', long, value_name = "LOAD")]
    pub load_average: Option<f64>,

    /// Continue merging as much as possible even if some packages fail
    #[arg(
        long,
        warning = "Exists for portage parity; do not use it. A failed package must stop the run."
    )]
    pub keep_going: bool,

    /// Accepted so existing command lines keep parsing. Config writes follow
    /// `--autounmask-write` and whether this run is pretend or `--ask`; this
    /// flag is not consulted.
    #[arg(long)]
    pub autounmask: bool,

    /// Let the solver choose USE flags to satisfy REQUIRED_USE (Level C) rather than only
    /// reporting violations
    ///
    /// Off by default; flips are reported.
    #[arg(long)]
    pub autosolve_use: bool,

    /// With `-p`/`--pretend` or `-a`/`--ask`, print an "Expected time of
    /// completion" for the plan alongside the merge list, estimated from
    /// activity history (median of recent successful merges per package;
    /// wall uses the build graph + `--jobs` when blockers are available).
    /// Shown even when the plan needs USE/mask changes to proceed.
    #[arg(long = "eta")]
    pub eta: bool,

    /// Re-solve with the installed packages whose dependencies the plan breaks, even with `--complete-graph-if-new-ver n`
    ///
    /// A broken installed package (e.g. `clang` pinned to the `llvm` being upgraded) is updated along
    /// with it, or holds the update back when no version of it fits. This is the default.
    #[arg(long)]
    pub complete_graph: bool,

    /// `n`: do not re-solve a plan that breaks installed packages' dependencies, only report them
    #[arg(long, value_name = "y|n")]
    pub complete_graph_if_new_ver: Option<YesNo>,

    /// Follow the build-time dependencies (DEPEND, BDEPEND) of installed packages that are kept
    ///
    /// With `--deep`, their upgrades and missing build tools enter the plan; `em depclean` keeps
    /// them installed. Unset, it is on for depclean, and for installs unless binary packages are
    /// used or `--with-bdeps-auto n` is given.
    #[arg(long, value_name = "y|n")]
    pub with_bdeps: Option<YesNo>,

    /// `n`: an install does not turn `--with-bdeps` on by itself
    #[arg(long, value_name = "y|n")]
    pub with_bdeps_auto: Option<YesNo>,

    /// Exclude the specified atom from being merged
    #[arg(short = 'X', long, value_name = "ATOM")]
    pub exclude: Vec<String>,

    /// Only require RDEPEND (not DEPEND) to be satisfied in the merge target.
    /// Work-around for cross-compilation bootstrap: a still-empty target sysroot
    /// cannot yet satisfy plain DEPEND (e.g. virtual/os-headers, acct-group/root)
    /// while its own toolchain is being built. `em crossdev --setup` always applies
    /// this unconditionally; elsewhere it defaults off.
    #[arg(long = "root-deps")]
    pub root_deps: bool,
}

/// A `y`/`n` flag value
#[derive(Clone, Copy, Debug, PartialEq, Eq, winnow_args::ValueEnum, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum YesNo {
    /// Yes
    Y,
    /// No
    N,
}

/// Also reads the plain booleans older resume files saved for `with_bdeps`
impl<'de> serde::Deserialize<'de> for YesNo {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum Saved {
            Bool(bool),
            Text(String),
        }
        match Saved::deserialize(deserializer)? {
            Saved::Bool(true) => Ok(Self::Y),
            Saved::Bool(false) => Ok(Self::N),
            Saved::Text(t) if t == "y" => Ok(Self::Y),
            Saved::Text(t) if t == "n" => Ok(Self::N),
            Saved::Text(t) => Err(serde::de::Error::custom(format!(
                "expected y or n, got {t}"
            ))),
        }
    }
}

impl MergeFlags {
    /// Whether an install follows kept packages' build-time deps: emerge's `--with-bdeps`, `auto`
    /// unless binary packages are in use
    pub fn install_bdeps(&self) -> bool {
        match self.with_bdeps {
            Some(value) => value == YesNo::Y,
            None => {
                let binpkgs =
                    self.usepkg || self.usepkgonly || self.getbinpkg || self.getbinpkgonly;
                self.with_bdeps_auto != Some(YesNo::N) && !binpkgs
            }
        }
    }

    /// Whether the solve is redone when the plan breaks an installed package's dependencies
    pub fn repair_broken_installed(&self) -> bool {
        self.complete_graph || self.complete_graph_if_new_ver != Some(YesNo::N)
    }

    /// Whether depclean keeps installed packages' build-time deps: all but `--with-bdeps n`
    pub fn depclean_bdeps(&self) -> bool {
        self.with_bdeps != Some(YesNo::N)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broken_installed_repair_is_on_unless_turned_off() {
        assert!(MergeFlags::default().repair_broken_installed());
        let off = MergeFlags {
            complete_graph_if_new_ver: Some(YesNo::N),
            ..MergeFlags::default()
        };
        assert!(!off.repair_broken_installed());
        let forced = MergeFlags {
            complete_graph: true,
            ..off
        };
        assert!(forced.repair_broken_installed());
    }

    #[test]
    fn with_bdeps_defaults_to_auto() {
        let unset = MergeFlags::default();
        assert!(unset.install_bdeps());
        assert!(unset.depclean_bdeps());

        let binpkgs = MergeFlags {
            getbinpkg: true,
            ..MergeFlags::default()
        };
        assert!(!binpkgs.install_bdeps());
        let forced = MergeFlags {
            with_bdeps: Some(YesNo::Y),
            ..binpkgs
        };
        assert!(forced.install_bdeps());

        let no_auto = MergeFlags {
            with_bdeps_auto: Some(YesNo::N),
            ..MergeFlags::default()
        };
        assert!(!no_auto.install_bdeps());
        assert!(no_auto.depclean_bdeps());

        let off = MergeFlags {
            with_bdeps: Some(YesNo::N),
            ..MergeFlags::default()
        };
        assert!(!off.install_bdeps());
        assert!(!off.depclean_bdeps());
    }

    #[test]
    fn an_old_resume_files_boolean_with_bdeps_still_loads() {
        let old: MergeFlags = serde_json::from_str(r#"{"with_bdeps": true}"#).unwrap();
        assert_eq!(old.with_bdeps, Some(YesNo::Y));
        let new: MergeFlags = serde_json::from_str(r#"{"with_bdeps": "n"}"#).unwrap();
        assert_eq!(new.with_bdeps, Some(YesNo::N));
    }
}
