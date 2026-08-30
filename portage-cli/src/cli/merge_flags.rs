//! Merge-behavior flags: everything `emerge_atoms`/`run_merge_plan` read to
//! decide *how* to resolve and build, as opposed to root-model flags
//! ([`super::Topology`]/[`super::RootArg`]) or depgraph-shape flags
//! ([`super::DepgraphFlags`]).
//!
//! Flattened exactly once into each merge-shaped applet (`EmergeArgs`,
//! `CrossdevArgs`, `ToolchainArgs`, `StagesArgs`, `SetupArgs`, `Revdep`,
//! `Depclean`). Bare `em -j8 cat/pkg` works because [`super::parse_cli_from`]
//! retries those invocations as `em emerge …`.
//!
//! `--search`/`--searchdesc`/`--nodeps` are not here: they select a different
//! emerge *action* ([`super::EmergeModeArgs`]), or a per-step `EmergeOpts`
//! override in the staged bootstrap.
#[derive(clap::Args, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct MergeFlags {
    /// Ask for confirmation before performing actions
    ///
    /// Lives here (not `global = true` on `Cli`): unlike `--root`/
    /// `--privilege`, `--ask` only means anything to a merge-shaped command
    /// — a config-only command like `em use`/`em pkg use add` never reads
    /// it. Making it global inherited that meaninglessness into every
    /// subcommand's args, which is also what caused `-a` to collide with
    /// `use`'s own `-a`/`--add` (a real crash — `em use --help` panicked).
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
    /// (Portage `PollScheduler._can_add_job`). The first concurrent job is always allowed.
    /// Displayed on the `Jobs:` status line regardless.
    #[arg(short = 'l', long, value_name = "LOAD")]
    pub load_average: Option<f64>,

    /// Continue merging as much as possible even if some packages fail
    #[arg(long)]
    pub keep_going: bool,

    /// Automatically add required USE flags and package unmask entries to config files
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
    ///
    /// Lives here (not `global = true` on `Cli`): like `--ask`, `--eta` only
    /// means something to a merge-shaped command — `em news --eta` or
    /// `em grep --eta` parsed fine but did nothing.
    #[arg(long = "eta")]
    pub eta: bool,

    /// With `-u`/`--update` `-D`/`--deep`: when moving a version-pinned
    /// family (e.g. upgrading `llvm` pulls `clang` along) would leave a
    /// retained package's pin broken (e.g. `lldb` still pinned to the old
    /// `llvm`), pull that package into the plan too instead of stopping
    /// halfway. Off by default: this can revert the upgrade instead if the
    /// retained package has no version satisfying the new pin.
    #[arg(long)]
    pub complete_graph: bool,

    /// Include build-time dependencies (BDEPEND) in the resolution
    /// Default is false (exclude BDEPEND), matching emerge's default.
    /// When enabled, BDEPEND are included but filtered by what's already
    /// installed on the build host (BROOT).
    #[arg(long)]
    pub with_bdeps: bool,

    /// Exclude the specified atom from being merged
    #[arg(short = 'X', long, value_name = "ATOM")]
    pub exclude: Vec<String>,

    /// Only require RDEPEND (not DEPEND) to be satisfied in the merge target
    /// Work-around for cross-compilation bootstrap: a still-empty target sysroot
    /// cannot yet satisfy plain DEPEND (e.g. virtual/os-headers, acct-group/root)
    /// while its own toolchain is being built. `em crossdev --setup` always applies
    /// this unconditionally; elsewhere it defaults off.
    #[arg(long = "root-deps")]
    pub root_deps: bool,
}
