//! Merge-behavior flags: everything `emerge_atoms`/`run_merge_plan` read to
//! decide *how* to resolve and build, as opposed to root-model flags
//! ([`super::Topology`]/[`super::RootArg`]) or depgraph-shape flags
//! ([`super::DepgraphFlags`]).
//!
//! Flattened onto [`super::Cli`] (prefix-position emerge) and each merge-shaped
//! applet (`EmergeArgs`, `CrossdevArgs`, `ToolchainArgs`, `StagesArgs`,
//! `SetupArgs`, `Revdep`, `Depclean`). Not global: `-a` must not reach `use`.
//!
//! `--search`/`--searchdesc`/`--nodeps` are not here: they select a different
//! emerge *action* ([`super::EmergeModeArgs`]), or a per-step `EmergeOpts`
//! override in the staged bootstrap.
#[derive(usage::Args, Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
#[usage(
    next_help_heading = "Merge",
    heading("Merge", help = "How the solver and build scheduler behave.")
)]
pub struct MergeFlags {
    /// Ask for confirmation before performing actions
    #[usage(short = 'a', long)]
    pub ask: bool,

    /// Update installed packages to newest available versions
    #[usage(short = 'u', long)]
    pub update: bool,

    /// Write required USE changes to /etc/portage/package.use/
    #[usage(long)]
    pub autounmask_write: bool,

    /// Build and install packages but do not add them to the world file
    #[usage(short = '1', long = "oneshot")]
    pub oneshot: bool,

    /// Only fetch distfiles, do not build or install
    #[usage(short = 'f', long)]
    pub fetchonly: bool,

    /// Instead of building, just fetch every SRC_URI file (regardless of
    /// USE setting) for the resolved packages.
    #[usage(short = 'F', long)]
    pub fetch_all_uri: bool,

    /// Build binary packages for all merged packages
    #[usage(short = 'b', long)]
    pub buildpkg: bool,

    /// Build binary packages without merging/installing them
    ///
    /// All build-time dependencies must already be satisfied on the system -- this does not
    /// resolve or install anything to make that true.
    #[usage(short = 'B', long)]
    pub buildpkgonly: bool,

    /// Use binary packages if available, otherwise fall back to source
    #[usage(short = 'k', long)]
    pub usepkg: bool,

    /// Only use binary packages, fail if none available
    #[usage(short = 'K', long)]
    pub usepkgonly: bool,

    /// Fetch binary packages for all requested packages
    #[usage(short = 'g', long)]
    pub getbinpkg: bool,

    /// Only fetch binary packages, do not install
    #[usage(short = 'G', long)]
    pub getbinpkgonly: bool,

    /// Treat every atom as not-yet-installed, rebuilding the whole dependency
    /// tree from scratch rather than only what is missing or outdated.
    #[usage(short = 'e', long)]
    pub emptytree: bool,

    /// Show the dependency tree, indenting each package under the one that
    /// pulled it in, before merging.
    #[usage(short = 't', long)]
    pub tree: bool,

    /// Emit the depgraph as machine-parsable JSON instead of pretend text
    ///
    /// Takes precedence over `--tree`. Works with `-p` (including `-e`).
    #[usage(long)]
    pub json: bool,

    /// Only merge dependencies, not the specified packages themselves
    #[usage(short = 'o', long)]
    pub onlydeps: bool,

    /// Do not replace installed packages that are already the same version
    #[usage(short = 'n', long)]
    pub noreplace: bool,

    /// Build up to N packages in parallel, respecting build-dependency order (merges are still
    /// serialised)
    ///
    /// Default 1 (sequential).
    #[usage(short = 'j', long, value_name = "N")]
    pub jobs: Option<u32>,

    /// Maximum 1-minute load average allowed when starting additional parallel builds (`--jobs`
    /// > 1)
    ///
    /// Once at least one job is running, further starts wait until load drops below LOAD
    /// (Portage `PollScheduler._can_add_job`). The first concurrent job is always allowed.
    /// Displayed on the `Jobs:` status line regardless.
    #[usage(short = 'l', long, value_name = "LOAD")]
    pub load_average: Option<f64>,

    /// Continue merging as much as possible even if some packages fail
    #[usage(
        long,
        warning = "Exists for portage parity; do not use it. A failed package must stop the run."
    )]
    pub keep_going: bool,

    /// Automatically add required USE flags and package unmask entries to config files
    #[usage(long)]
    pub autounmask: bool,

    /// Let the solver choose USE flags to satisfy REQUIRED_USE (Level C) rather than only
    /// reporting violations
    ///
    /// Off by default; flips are reported.
    #[usage(long)]
    pub autosolve_use: bool,

    /// With `-p`/`--pretend` or `-a`/`--ask`, print an "Expected time of
    /// completion" for the plan alongside the merge list, estimated from
    /// activity history (median of recent successful merges per package;
    /// wall uses the build graph + `--jobs` when blockers are available).
    /// Shown even when the plan needs USE/mask changes to proceed.
    #[usage(long = "eta")]
    pub eta: bool,

    /// With `-u`/`--update` `-D`/`--deep`: when moving a version-pinned
    /// family (e.g. upgrading `llvm` pulls `clang` along) would leave a
    /// retained package's pin broken (e.g. `lldb` still pinned to the old
    /// `llvm`), pull that package into the plan too instead of stopping
    /// halfway. Off by default: this can revert the upgrade instead if the
    /// retained package has no version satisfying the new pin.
    #[usage(long)]
    pub complete_graph: bool,

    /// Include build-time dependencies (BDEPEND) in the resolution.
    /// Default is false (exclude BDEPEND), matching emerge's default.
    /// When enabled, BDEPEND are included but filtered by what's already
    /// installed on the build host (BROOT).
    #[usage(long)]
    pub with_bdeps: bool,

    /// Exclude the specified atom from being merged
    #[usage(short = 'X', long, value_name = "ATOM")]
    pub exclude: Vec<String>,

    /// Only require RDEPEND (not DEPEND) to be satisfied in the merge target.
    /// Work-around for cross-compilation bootstrap: a still-empty target sysroot
    /// cannot yet satisfy plain DEPEND (e.g. virtual/os-headers, acct-group/root)
    /// while its own toolchain is being built. `em crossdev --setup` always applies
    /// this unconditionally; elsewhere it defaults off.
    #[usage(long = "root-deps")]
    pub root_deps: bool,
}

impl MergeFlags {
    /// Overlay a dual-mounted copy: bools OR, `Option` prefers applet, `Vec` applet-wins.
    pub(crate) fn overlay(&self, applet: &Self) -> Self {
        Self {
            ask: self.ask || applet.ask,
            update: self.update || applet.update,
            autounmask_write: self.autounmask_write || applet.autounmask_write,
            oneshot: self.oneshot || applet.oneshot,
            fetchonly: self.fetchonly || applet.fetchonly,
            fetch_all_uri: self.fetch_all_uri || applet.fetch_all_uri,
            buildpkg: self.buildpkg || applet.buildpkg,
            buildpkgonly: self.buildpkgonly || applet.buildpkgonly,
            usepkg: self.usepkg || applet.usepkg,
            usepkgonly: self.usepkgonly || applet.usepkgonly,
            getbinpkg: self.getbinpkg || applet.getbinpkg,
            getbinpkgonly: self.getbinpkgonly || applet.getbinpkgonly,
            emptytree: self.emptytree || applet.emptytree,
            tree: self.tree || applet.tree,
            json: self.json || applet.json,
            onlydeps: self.onlydeps || applet.onlydeps,
            noreplace: self.noreplace || applet.noreplace,
            jobs: applet.jobs.or(self.jobs),
            load_average: applet.load_average.or(self.load_average),
            keep_going: self.keep_going || applet.keep_going,
            autounmask: self.autounmask || applet.autounmask,
            autosolve_use: self.autosolve_use || applet.autosolve_use,
            eta: self.eta || applet.eta,
            complete_graph: self.complete_graph || applet.complete_graph,
            with_bdeps: self.with_bdeps || applet.with_bdeps,
            exclude: if applet.exclude.is_empty() {
                self.exclude.clone()
            } else {
                applet.exclude.clone()
            },
            root_deps: self.root_deps || applet.root_deps,
        }
    }
}
