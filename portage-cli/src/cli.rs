use gentoo_core::Arch;
#[cfg(test)]
use portage_atom_pubgrub::DepClass;
use portage_resolve::Roots;
use usage::ValidationError;

mod activity;
mod depgraph_flags;
mod emerge_mode;
mod merge_flags;
mod topology;
pub use activity::ActivityArgs;
pub use depgraph_flags::DepgraphFlags;
pub use emerge_mode::EmergeModeArgs;
pub use merge_flags::MergeFlags;
pub use topology::{RootArg, Topology};

fn default_arch() -> Arch {
    Arch::current()
}

/// When to colour terminal output (`--color`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, usage::ValueEnum)]
pub enum ColorChoice {
    /// Colour when stdout is a terminal.
    #[default]
    Auto,
    /// Always colour.
    Always,
    /// Never colour.
    Never,
}

impl ColorChoice {
    /// Apply this choice as the process-wide [`anstream`] colour setting.
    pub fn write_global(self) {
        let mapped = match self {
            Self::Auto => anstream::ColorChoice::Auto,
            Self::Always => anstream::ColorChoice::Always,
            Self::Never => anstream::ColorChoice::Never,
        };
        mapped.write_global();
    }
}

/// Parsed `em` invocation after `try_into` validation.
pub struct Validated(pub Cli);

impl TryFrom<Cli> for Validated {
    type Error = ValidationError;

    fn try_from(cli: Cli) -> Result<Self, Self::Error> {
        validate(&cli)?;
        Ok(Validated(cli))
    }
}

fn consumes_merge(applet: &Option<Applet>) -> bool {
    matches!(
        applet,
        None | Some(
            Applet::Emerge(_)
                | Applet::Crossdev(_)
                | Applet::Toolchain(_)
                | Applet::Stages(_)
                | Applet::Setup(_)
                | Applet::Revdep(_)
                | Applet::Depclean(_)
        )
    )
}

fn consumes_depgraph(applet: &Option<Applet>) -> bool {
    matches!(
        applet,
        None | Some(
            Applet::Emerge(_)
                | Applet::Crossdev(_)
                | Applet::Toolchain(_)
                | Applet::Stages(_)
                | Applet::Setup(_)
        )
    )
}

fn consumes_mode(applet: &Option<Applet>) -> bool {
    matches!(applet, None | Some(Applet::Emerge(_)))
}

fn consumes_activity(applet: &Option<Applet>) -> bool {
    matches!(
        applet,
        None | Some(
            Applet::Emerge(_)
                | Applet::Regen(_)
                | Applet::Crossdev(_)
                | Applet::Toolchain(_)
                | Applet::Stages(_)
                | Applet::Setup(_)
        )
    )
}

fn consumes_privilege(applet: &Option<Applet>) -> bool {
    matches!(
        applet,
        None | Some(
            Applet::Emerge(_)
                | Applet::Crossdev(_)
                | Applet::Toolchain(_)
                | Applet::Stages(_)
                | Applet::Setup(_)
        )
    )
}

fn validate(cli: &Cli) -> Result<(), ValidationError> {
    if cli.root.is_some() && matches!(&cli.applet, Some(Applet::Crossdev(_) | Applet::Active(_))) {
        return Err(ValidationError::field("--root").reason("not valid with this applet"));
    }
    if !consumes_merge(&cli.applet) && cli.merge_flags != MergeFlags::default() {
        if cli.merge_flags.ask {
            return Err(ValidationError::field("--ask").reason("not valid with this applet"));
        }
        return Err(ValidationError::field("emerge-mixin").reason("not valid with this applet"));
    }
    if !consumes_depgraph(&cli.applet) && cli.depgraph_flags != DepgraphFlags::default() {
        return Err(ValidationError::field("emerge-mixin").reason("not valid with this applet"));
    }
    if !consumes_mode(&cli.applet) && cli.mode != EmergeModeArgs::default() {
        return Err(ValidationError::field("emerge-mode").reason("not valid with this applet"));
    }
    if !consumes_activity(&cli.applet) && cli.activity != ActivityArgs::default() {
        return Err(ValidationError::field("emerge-mixin").reason("not valid with this applet"));
    }
    if !consumes_privilege(&cli.applet) && cli.privilege != Privilege::Auto {
        return Err(ValidationError::field("emerge-mixin").reason("not valid with this applet"));
    }
    Ok(())
}

fn overlay_root(applet: &RootArg, cli_root: Option<&str>) -> RootArg {
    RootArg {
        root: applet.root.clone().or_else(|| cli_root.map(str::to_string)),
    }
}

fn overlay_privilege(cli: Privilege, applet: Privilege) -> Privilege {
    if applet != Privilege::Auto {
        applet
    } else if cli != Privilege::Auto {
        cli
    } else {
        Privilege::Auto
    }
}

fn privilege_from_env() -> Option<Privilege> {
    let raw = std::env::var("EM_PRIVILEGE").ok()?;
    match raw.to_ascii_lowercase().as_str() {
        "auto" => Some(Privilege::Auto),
        "sudo" => Some(Privilege::Sudo),
        "none" => Some(Privilege::None),
        #[cfg(all(feature = "fakeroost", target_os = "linux"))]
        "fakeroost" => Some(Privilege::Fakeroost),
        #[cfg(all(feature = "pseudoroot", any(target_os = "linux", target_os = "macos")))]
        "pseudoroot" => Some(Privilege::Pseudoroot),
        #[cfg(all(feature = "hakoniwa", target_os = "linux"))]
        "hakoniwa" => Some(Privilege::Hakoniwa),
        _ => None,
    }
}

fn env_flag_true(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => matches!(
            v.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on" | "t"
        ),
        Err(_) => false,
    }
}

#[derive(usage::Cli, Debug)]
#[usage(
    bin = "em",
    version,
    about = "Gentoo Portage package manager workalike",
    arg_required_else_help,
    unknown_flags = "error",
    default_subcommand = "emerge",
    try_into = Validated
)]
pub struct Cli {
    #[usage(long, global, value_enum, default = "auto", value_name = "WHEN")]
    pub color: ColorChoice,

    /// Show what would be done without actually performing any actions
    #[usage(short = 'p', long, global)]
    pub pretend: bool,

    /// Print system/build info: profile, CHOST/CFLAGS/FEATURES/USE (with
    /// USE_EXPAND groups like VIDEO_CARDS broken out), ACCEPT_KEYWORDS/
    /// ACCEPT_LICENSE, and configured repositories — `emerge --info`
    /// workalike. Takes no atoms. Combine with `--json` for structured
    /// output, or `-v` to also list every known `@name` set and its
    /// resolved atoms (neither has a real-emerge equivalent).
    #[usage(long)]
    pub info: bool,

    /// Increase verbosity: `-v` labels each build phase, `-vv`/`-vvv` add
    /// `em`'s own debug/trace logs (see also `RUST_LOG`).
    #[usage(short = 'v', long, count, global)]
    pub verbose: u8,

    /// Suppress non-error output
    #[usage(short = 'q', long, global)]
    pub quiet: bool,

    /// Target architecture for operations
    #[usage(
        long,
        global,
        value_name = "ARCH",
        default_fn = default_arch,
        default_note = "current system architecture"
    )]
    pub arch: Arch,

    /// Pin search/query to a single repository
    ///
    /// When unset, repositories are auto-discovered from `repos.conf` (the main repo wins for
    /// single-repo applets; search walks all of them).
    #[usage(long, global, value_name = "PATH")]
    pub repo: Option<String>,

    #[usage(flatten)]
    pub topology: Topology,

    /// Prefix-position `--root` for default emerge. Not global; must not leak
    /// into crossdev/active/worker.
    #[usage(long, value_name = "PATH")]
    pub root: Option<String>,

    #[usage(flatten)]
    pub merge_flags: MergeFlags,
    #[usage(flatten)]
    pub depgraph_flags: DepgraphFlags,
    #[usage(flatten)]
    pub mode: EmergeModeArgs,
    #[usage(flatten)]
    pub activity: ActivityArgs,

    /// Privilege backend. Redeclared on merge applets; no `env` on the field.
    #[usage(long, value_enum, default = "auto")]
    pub privilege: Privilege,

    #[usage(subcommand)]
    pub applet: Option<Applet>,
}

impl Cli {
    fn applet_root_arg(&self) -> Option<&RootArg> {
        match &self.applet {
            Some(Applet::Emerge(a)) => Some(&a.root_arg),
            Some(Applet::Toolchain(a)) => Some(&a.root_arg),
            Some(Applet::Stages(a)) => Some(&a.root_arg),
            Some(Applet::Setup(a)) => Some(&a.root_arg),
            Some(Applet::Ebuild(a)) => Some(&a.root_arg),
            Some(Applet::Maint(a)) => Some(&a.root_arg),
            Some(Applet::Sync(a)) => Some(&a.root_arg),
            Some(Applet::Depclean(a)) => Some(&a.root_arg),
            Some(Applet::Regen(a)) => Some(&a.root_arg),
            Some(Applet::Quickpkg(a)) => Some(&a.root_arg),
            Some(Applet::MirrorDist(a)) => Some(&a.root_arg),
            Some(Applet::Clean(a)) => Some(&a.root_arg),
            Some(Applet::Etc(a)) => Some(&a.root_arg),
            Some(Applet::Query(a)) => Some(&a.root_arg),
            Some(Applet::Use(a)) => Some(&a.root_arg),
            Some(Applet::Pkg(a)) => Some(&a.root_arg),
            Some(Applet::Revdep(a)) => Some(&a.root_arg),
            Some(Applet::Read(a)) => Some(&a.root_arg),
            Some(Applet::Log(a)) => Some(&a.root_arg),
            Some(Applet::Search(a)) => Some(&a.root_arg),
            Some(Applet::Select(a)) => Some(&a.root_arg),
            Some(Applet::Env(a)) => Some(&a.root_arg),
            _ => None,
        }
    }

    fn topology_and_root(&self) -> (Topology, RootArg) {
        let root = match self.applet_root_arg() {
            Some(applet) => overlay_root(applet, self.root.as_deref()),
            None => RootArg {
                root: self.root.clone(),
            },
        };
        (self.topology.clone(), root)
    }

    /// Resolve the root model (docs/design/root-topology.md) for the active applet
    ///
    /// `--target <tuple>` layers on top of the base model: it targets the crossdev
    /// sysroot `<EROOT>/usr/<tuple>` as both config-root and root. See
    /// [`Topology::roots`] for the full resolution.
    pub fn roots(&self) -> Roots {
        let (topology, root) = self.topology_and_root();
        topology.roots(&root)
    }

    /// See [`Topology::outer_roots`].
    pub(crate) fn outer_roots(&self) -> Roots {
        let (topology, root) = self.topology_and_root();
        topology.outer_roots(&root)
    }

    /// See [`Topology::base_roots`].
    pub(crate) fn base_roots(&self) -> Roots {
        let (topology, root) = self.topology_and_root();
        topology.base_roots(&root)
    }

    /// See [`Topology::host_roots`].
    pub(crate) fn host_roots(&self) -> Roots {
        let (topology, root) = self.topology_and_root();
        topology.host_roots(&root)
    }

    /// See [`Topology::sysroot_roots`].
    pub(crate) fn sysroot_roots(&self) -> Option<Roots> {
        let (topology, root) = self.topology_and_root();
        topology.sysroot_roots(&root)
    }

    /// The active `--target` tuple, if any.
    pub(crate) fn target(&self) -> Option<String> {
        self.topology.target.clone()
    }

    /// The active `--vdb` override, if any.
    pub fn vdb(&self) -> Option<String> {
        self.topology.vdb.clone()
    }

    /// See [`Topology::require_root_distinct_from_host`].
    pub(crate) fn require_root_distinct_from_host(
        &self,
        resolved: &Roots,
        action: &str,
    ) -> anyhow::Result<()> {
        let (topology, root) = self.topology_and_root();
        topology.require_root_distinct_from_host(&root, resolved, action)
    }

    /// See [`Topology::require_destination_not_bare_host`].
    pub(crate) fn require_destination_not_bare_host(
        &self,
        resolved: &Roots,
        action: &str,
    ) -> anyhow::Result<()> {
        Topology::require_destination_not_bare_host(resolved, action)
    }

    /// Path used by single-repo applets
    ///
    /// Falls back to `/var/db/repos/gentoo` when neither `--repo` nor `repos.conf` is
    /// available.
    pub fn repo_path(&self) -> String {
        if let Some(p) = &self.repo {
            return p.clone();
        }
        if let Ok(rc) = self.roots().repos_conf()
            && let Some(main) = rc.main_repo()
        {
            return main
                .location
                .as_path()
                .map(|p| p.to_path_buf())
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
        }
        "/var/db/repos/gentoo".to_string()
    }

    /// Repositories this invocation's configuration actually names, or `None`
    /// when there is no usable `repos.conf`
    ///
    /// The distinction matters for anything destructive: `search_repos`
    /// substitutes the host tree so a query still returns something, but a
    /// command that *deletes* based on "what the tree references" must not
    /// quietly answer from a different system's tree than the one it was
    /// pointed at.
    pub(crate) fn configured_repos(&self) -> Option<Vec<std::path::PathBuf>> {
        if let Some(p) = &self.repo {
            return Some(vec![std::path::PathBuf::from(p)]);
        }
        match self.roots().repos_conf() {
            Ok(rc) if !rc.repos().is_empty() => Some(
                rc.repos()
                    .iter()
                    .filter_map(|e| e.location.as_path().map(std::path::PathBuf::from))
                    .collect(),
            ),
            _ => None,
        }
    }

    /// Repositories to walk for `em search`
    ///
    /// Honours `--repo` when set; otherwise every entry from `repos.conf`
    /// (main first), falling back to the host's own tree so a query on a
    /// misconfigured root still answers.
    //
    // That fallback is why a *deleting* caller must use `configured_repos`
    // instead: substituting the host tree there decides what is unreferenced
    // from a different system than the one it was pointed at.
    pub fn search_repos(&self) -> Vec<std::path::PathBuf> {
        self.configured_repos()
            .unwrap_or_else(|| vec![std::path::PathBuf::from("/var/db/repos/gentoo")])
    }

    /// Overlayed [`MergeFlags`] for the dispatched merge-shaped applet.
    pub fn merge_flags(&self) -> MergeFlags {
        let applet = match &self.applet {
            Some(Applet::Emerge(a)) => &a.merge_flags,
            Some(Applet::Crossdev(a)) => &a.merge_flags,
            Some(Applet::Toolchain(a)) => &a.merge_flags,
            Some(Applet::Stages(a)) => &a.merge_flags,
            Some(Applet::Setup(a)) => &a.merge_flags,
            Some(Applet::Revdep(a)) => &a.merge_flags,
            Some(Applet::Depclean(a)) => &a.merge_flags,
            _ => return self.merge_flags.clone(),
        };
        self.merge_flags.overlay(applet)
    }

    /// Overlayed [`DepgraphFlags`] for merge applets only — not query.
    pub fn depgraph_flags(&self) -> DepgraphFlags {
        let applet = match &self.applet {
            Some(Applet::Emerge(a)) => &a.depgraph_flags,
            Some(Applet::Crossdev(a)) => &a.depgraph_flags,
            Some(Applet::Toolchain(a)) => &a.depgraph_flags,
            Some(Applet::Stages(a)) => &a.depgraph_flags,
            Some(Applet::Setup(a)) => &a.depgraph_flags,
            _ => return self.depgraph_flags.clone(),
        };
        self.depgraph_flags.overlay(applet)
    }

    /// Overlayed activity-output flags, then `EM_EMERGELOG` once if still off.
    pub fn effective_activity(&self) -> ActivityArgs {
        let applet = match &self.applet {
            Some(Applet::Emerge(a)) => &a.activity,
            Some(Applet::Regen(a)) => &a.activity,
            Some(Applet::Crossdev(a)) => &a.activity,
            Some(Applet::Toolchain(a)) => &a.activity,
            Some(Applet::Stages(a)) => &a.activity,
            Some(Applet::Setup(a)) => &a.activity,
            _ => {
                let mut activity = self.activity.clone();
                if !activity.emergelog && env_flag_true("EM_EMERGELOG") {
                    activity.emergelog = true;
                }
                return activity;
            }
        };
        let mut activity = self.activity.overlay(applet);
        if !activity.emergelog && env_flag_true("EM_EMERGELOG") {
            activity.emergelog = true;
        }
        activity
    }

    /// Overlayed `--privilege`, then `EM_PRIVILEGE` once if still `Auto`.
    pub fn effective_privilege(&self) -> Privilege {
        let applet = match &self.applet {
            Some(Applet::Emerge(a)) => a.privilege,
            Some(Applet::Crossdev(a)) => a.privilege,
            Some(Applet::Toolchain(a)) => a.privilege,
            Some(Applet::Stages(a)) => a.privilege,
            Some(Applet::Setup(a)) => a.privilege,
            _ => Privilege::Auto,
        };
        let argv = overlay_privilege(self.privilege, applet);
        if argv != Privilege::Auto {
            argv
        } else {
            privilege_from_env().unwrap_or(Privilege::Auto)
        }
    }

    /// Overlayed emerge-mode switches, or the Cli copy on defaulted emerge.
    pub fn mode(&self) -> EmergeModeArgs {
        match &self.applet {
            Some(Applet::Emerge(a)) => self.mode.overlay(&a.mode),
            None => self.mode.clone(),
            _ => EmergeModeArgs::default(),
        }
    }

    /// `Applet::Emerge`'s own atom list, or empty when it isn't the active applet.
    pub fn atoms(&self) -> Vec<String> {
        match &self.applet {
            Some(Applet::Emerge(a)) => a.atoms.clone(),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
pub(crate) fn os_argv<'a>(argv: &'a [&'a str]) -> Vec<&'a std::ffi::OsStr> {
    argv.iter().copied().map(std::ffi::OsStr::new).collect()
}

/// Parse argv including argv0. Panics on failure — unit-test helper.
#[cfg(test)]
pub(crate) fn parse_cli(argv: &[&str]) -> Cli {
    let words = os_argv(argv);
    Cli::try_parse_from(&words).unwrap_or_else(|e| {
        panic!(
            "expected parse of {argv:?}: {}",
            Cli::render_failure(&words, &e)
        )
    })
}

#[cfg(test)]
pub(crate) fn parse_cli_into(argv: &[&str]) -> Result<Cli, String> {
    let words = os_argv(argv);
    Cli::try_parse_into_from(&words)
        .map(|Validated(cli)| cli)
        .map_err(|e| err_name(&e))
}

#[cfg(test)]
fn err_name(e: &usage::Error<'_, '_>) -> String {
    match e {
        usage::Error::UnknownFlag { token } => {
            format!("UnknownFlag {}", String::from_utf8_lossy(token))
        }
        usage::Error::Help { .. } => "Help".into(),
        usage::Error::Version { .. } => "Version".into(),
        usage::Error::MissingArgsHelp { .. } => "MissingArgsHelp".into(),
        usage::Error::InvalidValue(v) => format!("InvalidValue {} {}", v.name, v.reason),
        usage::Error::MissingRequired { name } => format!("MissingRequired {name}"),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
fn parse_err(argv: &[&str]) -> String {
    let words = os_argv(argv);
    match Cli::try_parse_from(&words) {
        Ok(cli) => panic!("expected error for {argv:?}, got {cli:?}"),
        Err(e) => err_name(&e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn emerge<'a>(argv: impl IntoIterator<Item = &'a str>) -> Cli {
        let v: Vec<&str> = ["em", "emerge"].into_iter().chain(argv).collect();
        parse_cli(&v)
    }

    #[test]
    fn spec_is_valid() {
        let _ = Cli::to_kdl();
        let _ = Cli::spec();
    }

    #[test]
    fn tree_and_target_have_distinct_short_flags() {
        let cli = emerge(["-t", "-T", "riscv64-unknown-linux-gnu", "sys-libs/zlib"]);
        assert!(cli.merge_flags().tree);
        assert_eq!(cli.target().as_deref(), Some("riscv64-unknown-linux-gnu"));
    }

    #[test]
    fn cross_targets_sysroot_under_eroot() {
        // A bare `--root R` alongside `--target T` (no `--prefix`/`--local`)
        // is a board-root override: config/base stay at the toolchain's own
        // bare `/usr/<tuple>` (so `build_sysroot()` keeps returning it —
        // dropping it broke `sys-libs/zlib`'s own sysroot header search,
        // confirmed live), while target/merge_root become `R`.
        let cli = emerge([
            "--root",
            "/srv/x",
            "--target",
            "riscv64-unknown-linux-gnu",
            "-p",
            "sys-libs/zlib",
        ]);
        let r = cli.roots();
        let sysroot = "/usr/riscv64-unknown-linux-gnu";
        assert_eq!(r.config().unwrap().as_str(), sysroot);
        assert_eq!(r.base().unwrap().as_str(), sysroot);
        assert_eq!(r.merge_root().as_str(), "/srv/x");
        assert_eq!(r.target().unwrap().as_str(), "/srv/x");
        assert_eq!(r.build_sysroot().unwrap().as_str(), sysroot);
    }

    // `outer_roots()` (what every `use_outer_eroot` merge step — host-side
    // `cross-*` tool installs — actually resolves against) must keep
    // landing on the real host `/`, not a bare `--root` board destination,
    // once `--target` is also set. Regression for a real duplicate-install
    // bug: `stages --root R --target T` would otherwise double-plan the
    // cross-compiler refresh step into `R` alongside the actual board
    // packages.
    #[test]
    fn outer_roots_ignores_bare_root_under_target() {
        let (_tmp, _g) = crate::test_support::isolate_active_state();
        let cli = emerge([
            "--root",
            "/board",
            "--target",
            "riscv64-unknown-linux-gnu",
            "-p",
            "sys-libs/zlib",
        ]);
        let outer = cli.outer_roots();
        assert_eq!(outer.merge_root().as_str(), "/");
        assert_eq!(outer.base(), None);

        // `--target` alone (no `--root`) is unaffected either way.
        let no_root = emerge([
            "--target",
            "riscv64-unknown-linux-gnu",
            "-p",
            "sys-libs/zlib",
        ]);
        assert_eq!(no_root.outer_roots().merge_root().as_str(), "/");

        // No `--target`: a bare `--root` still redirects outer_roots() as
        // always (ordinary, non-cross `--root` usage).
        let no_target = emerge(["--root", "/board", "-p", "sys-libs/zlib"]);
        assert_eq!(no_target.outer_roots().merge_root().as_str(), "/board");
    }

    // Same bug as `outer_roots_ignores_bare_root_under_target`, for the
    // overlay branch: `--root` under `--target` must not move the
    // toolchain's own outer location away from the prefix either.
    #[test]
    fn outer_roots_ignores_prefix_root_under_target() {
        let (_tmp, _g) = crate::test_support::isolate_active_state();
        let cli = emerge([
            "--prefix",
            "/tmp/a",
            "--root",
            "/tmp/b",
            "--target",
            "riscv64-unknown-linux-gnu",
            "-p",
            "sys-libs/zlib",
        ]);
        let outer = cli.outer_roots();
        assert_eq!(outer.merge_root().as_str(), "/tmp/a");
        assert_eq!(outer.eprefix().map(|p| p.as_str()), Some("/tmp/a"));
        assert_eq!(
            outer.config_overlay().map(|p| p.as_str()),
            Some("/tmp/a/etc/portage")
        );

        // No `--target`: ordinary `--prefix`+`--root` still redirects.
        let no_target = emerge(["--prefix", "/tmp/a", "--root", "/tmp/b"]);
        assert_eq!(no_target.outer_roots().merge_root().as_str(), "/tmp/b");
    }

    // `--local` is not `is_overlay()` (it has its own `base`), so it never
    // enters the overlay branch above — a naive fix gating only that branch
    // on `--target` would miss this row entirely. `base_roots()`'s own
    // `Local` arm bakes an explicit `--root` into `target` unconditionally,
    // so `outer_roots()` must undo that when `--target` is also set.
    #[test]
    fn outer_roots_ignores_local_root_under_target() {
        let (_tmp, _g) = crate::test_support::isolate_active_state();
        let cli = emerge([
            "--local",
            "/tmp/a",
            "--root",
            "/tmp/b",
            "--target",
            "riscv64-unknown-linux-gnu",
            "-p",
            "sys-libs/zlib",
        ]);
        let outer = cli.outer_roots();
        assert_eq!(outer.merge_root().as_str(), "/tmp/a");
        assert_eq!(outer.base().map(|p| p.as_str()), Some("/tmp/a"));

        // No `--target`: ordinary `--local`+`--root` still redirects.
        let no_target = emerge(["--local", "/tmp/a", "--root", "/tmp/b"]);
        assert_eq!(no_target.outer_roots().merge_root().as_str(), "/tmp/b");

        // `--local` alone under `--target` (no `--root`) is unaffected.
        let no_root = emerge([
            "--local",
            "/tmp/a",
            "--target",
            "riscv64-unknown-linux-gnu",
            "-p",
            "sys-libs/zlib",
        ]);
        assert_eq!(no_root.outer_roots().merge_root().as_str(), "/tmp/a");
    }

    // An `em active`-registered local, with no topology flag at all, must
    // behave the same as an explicit `--local` under `--target` — the old
    // bare-only guard keyed on raw flags (`self.local.is_none()`), which is
    // true here even though `topology_source()` resolves to `Local`, so it
    // wrongly stripped to the real host `/` with no `--root` in sight.
    #[test]
    fn outer_roots_honours_an_active_local_under_target() {
        let (tmp, _g) = crate::test_support::isolate_active_state();
        let local = tmp.path().join("loc");
        std::fs::create_dir_all(&local).unwrap();
        let local_s = local.to_str().unwrap();
        let cli_set = parse_cli(&["em", "active", "--local", local_s, "set"]);
        crate::active::run(
            cli_set.applet.as_ref().and_then(|a| match a {
                Applet::Active(a) => a.command.as_ref(),
                _ => None,
            }),
            &cli_set,
        )
        .unwrap();

        let cli = emerge([
            "--target",
            "riscv64-unknown-linux-gnu",
            "-p",
            "sys-libs/zlib",
        ]);
        let canon = local.canonicalize().unwrap();
        let canon_s = canon.to_str().unwrap();
        assert_eq!(cli.outer_roots().merge_root().as_str(), canon_s);
    }

    #[test]
    fn cross_defaults_to_root_eroot() {
        // Isolate active state so a developer-registered prefix cannot
        // rewrite bare-host topology under us.
        let (_tmp, _g) = crate::test_support::isolate_active_state();
        // No `--root`: EROOT is `/`, so the sysroot is `/usr/<tuple>`.
        let cli = emerge(["--target", "riscv64-unknown-linux-gnu", "-p", "zlib"]);
        assert_eq!(
            cli.roots().merge_root().as_str(),
            "/usr/riscv64-unknown-linux-gnu"
        );
    }

    // `--prefix P --target T` must keep distfiles/work under P (relocate +
    // eprefix), not fall back to host paths or nest under the sysroot.
    #[test]
    fn prefix_plus_target_preserves_overlay_relocate() {
        let cli = emerge([
            "--prefix",
            "/tmp/p",
            "--target",
            "riscv64-unknown-linux-gnu",
            "-p",
            "sys-libs/zlib",
        ]);
        let r = cli.roots();
        let sysroot = "/tmp/p/usr/riscv64-unknown-linux-gnu";
        assert_eq!(r.merge_root().as_str(), sysroot);
        assert!(r.relocate(), "overlay relocate must survive --target");
        assert_eq!(r.eprefix().map(|p| p.as_str()), Some("/tmp/p"));
        assert_eq!(
            r.config_overlay().map(|p| p.as_str()),
            Some("/tmp/p/etc/portage")
        );
        assert_eq!(
            r.relocate_root().map(|p| p.as_str()),
            Some("/tmp/p"),
            "distfiles/work must anchor under the outer prefix, not the sysroot"
        );
        assert_eq!(
            r.build_eprefix(),
            None,
            "a package merging into the sysroot builds unprefixed — eprefix() \
             stays Some for relocate_root()'s benefit, but build_eprefix() must \
             be None or its .pc/.la files bake in a prefix that doesn't exist \
             inside the sysroot (live-verified: libffi's .pc under this exact \
             topology, todo/for-sonnet.md 2026-08-08)"
        );
    }

    // Same sysroot substitution, now with an explicit `--root B` on top
    // (`stages --stage1 --prefix P --root B --target T`'s exact shape):
    // `--root` redirects *only* the destination `stages` installs into (`B`
    // directly, same as bare `--target T --root B`), never the toolchain's
    // own sysroot (`P/usr/T`, unmoved — the crossdev toolchain is one
    // prefix-wide install, matching real crossdev's own ROOT-varies-
    // toolchain-doesn't model). `build_eprefix()` stays `None` regardless —
    // live-verified (zlib, real merge, sandbox) to also hold for the
    // `--root`-without-`--target` case (`explicit_root_overrides_prefix_destination_only`
    // below is the non-cross twin of this test).
    #[test]
    fn prefix_plus_root_plus_target_sysroot_still_builds_unprefixed() {
        let cli = emerge([
            "--prefix",
            "/tmp/p",
            "--root",
            "/tmp/b",
            "--target",
            "riscv64-unknown-linux-gnu",
            "-p",
            "sys-libs/zlib",
        ]);
        let r = cli.roots();
        assert_eq!(r.merge_root().as_str(), "/tmp/b");
        assert_eq!(
            r.config().map(|p| p.as_str()),
            Some("/tmp/p/usr/riscv64-unknown-linux-gnu"),
            "the toolchain's own config/sysroot stays anchored at the prefix, unmoved by --root"
        );
        assert_eq!(
            r.base().map(|p| p.as_str()),
            Some("/tmp/p/usr/riscv64-unknown-linux-gnu")
        );
        assert_eq!(r.build_eprefix(), None);
    }

    // Positive control: a plain `--prefix`/`--local` build (no `--root`
    // redirect, no `--target` substitution) still gets `build_eprefix() ==
    // eprefix()` — guards against over-clearing. The PMS invariant `EROOT
    // == ROOT + EPREFIX` holds here (`merge_root() == eprefix()`), so the
    // package genuinely is the thing living at that prefix and its `.pc`
    // files must say so.
    #[test]
    fn plain_prefix_and_local_still_report_build_eprefix() {
        let prefix = emerge(["--prefix", "/tmp/p", "-p", "sys-libs/zlib"]);
        let pr = prefix.roots();
        assert_eq!(pr.build_eprefix().map(|p| p.as_str()), Some("/tmp/p"));

        let local = emerge(["--local", "/tmp/a", "-p", "sys-libs/zlib"]);
        let lr = local.roots();
        assert_eq!(lr.build_eprefix().map(|p| p.as_str()), Some("/tmp/a"));
    }

    // An explicit `--root B` alongside `--prefix A` redirects only the
    // destination (`merge_root()`) — EPREFIX/config-overlay/relocate stay
    // anchored to `A`, matching `--prefix`'s own build-context role.
    // Previously `B` was silently discarded the moment `--prefix` matched in
    // `topology_source()` (todo/for-sonnet.md 2026-08-08).
    #[test]
    fn explicit_root_overrides_prefix_destination_only() {
        let cli = emerge([
            "--prefix",
            "/tmp/a",
            "--root",
            "/tmp/b",
            "-p",
            "sys-libs/zlib",
        ]);
        let r = cli.outer_roots();
        assert_eq!(r.merge_root().as_str(), "/tmp/b");
        assert_eq!(r.eprefix().map(|p| p.as_str()), Some("/tmp/a"));
        assert_eq!(
            r.config_overlay().map(|p| p.as_str()),
            Some("/tmp/a/etc/portage")
        );
        assert_eq!(
            r.build_eprefix(),
            None,
            "a package merging into the --root-redirected destination builds \
             unprefixed too, not just the --target sysroot case — live-verified \
             (zlib, real merge, sandbox): /tmp/a has no install of this package \
             at all, so baking it into .pc/.la would point nowhere real"
        );
    }

    // Same override, now also combined with `--target`: `--root` still
    // redirects only the destination (`B` directly); the toolchain's own
    // sysroot stays at the prefix (`A/usr/T`), unmoved.
    #[test]
    fn explicit_root_overrides_prefix_destination_under_target() {
        let cli = emerge([
            "--prefix",
            "/tmp/a",
            "--root",
            "/tmp/b",
            "--target",
            "riscv64-unknown-linux-gnu",
            "-p",
            "sys-libs/zlib",
        ]);
        let r = cli.roots();
        assert_eq!(r.merge_root().as_str(), "/tmp/b");
        assert_eq!(
            r.config().map(|p| p.as_str()),
            Some("/tmp/a/usr/riscv64-unknown-linux-gnu")
        );
    }

    // Same idea for `--local`: an explicit, genuinely different `--root B`
    // redirects the destination; BROOT/EPREFIX stay the local prefix itself
    // (still self-hosting for build-context purposes).
    #[test]
    fn explicit_root_overrides_local_destination_only() {
        let cli = emerge([
            "--local",
            "/tmp/a",
            "--root",
            "/tmp/b",
            "-p",
            "sys-libs/zlib",
        ]);
        let r = cli.roots();
        assert_eq!(r.merge_root().as_str(), "/tmp/b");
        assert_eq!(r.broot().map(|p| p.as_str()), Some("/tmp/a"));
        assert_eq!(r.eprefix().map(|p| p.as_str()), Some("/tmp/a"));
    }

    // Same combination as `prefix_plus_root_plus_target_sysroot_still_builds_unprefixed`,
    // for `--local`: `roots()`'s `has_own_build_context` branch covers both,
    // so `--root` must redirect only the destination here too, never the
    // toolchain's own sysroot (`A/usr/T`).
    #[test]
    fn explicit_root_overrides_local_destination_under_target() {
        let cli = emerge([
            "--local",
            "/tmp/a",
            "--root",
            "/tmp/b",
            "--target",
            "riscv64-unknown-linux-gnu",
            "-p",
            "sys-libs/zlib",
        ]);
        let r = cli.roots();
        assert_eq!(r.merge_root().as_str(), "/tmp/b");
        assert_eq!(
            r.config().map(|p| p.as_str()),
            Some("/tmp/a/usr/riscv64-unknown-linux-gnu")
        );
    }

    // `--root` set to the *same* path as `--local`/`--prefix` is a no-op
    // (not a distinct override) — this is exactly the degenerate case
    // `require_root_distinct_from_host` must still reject.
    #[test]
    fn root_matching_local_is_not_a_distinct_override() {
        let cli = emerge([
            "--local",
            "/tmp/a",
            "--root",
            "/tmp/a",
            "-p",
            "sys-libs/zlib",
        ]);
        let r = cli.roots();
        assert_eq!(r.merge_root(), cli.host_roots().merge_root());
    }

    // The actual guard: bare `--local`, bare `--prefix`, and bare host all
    // resolve to the same place their own build tools live and must be
    // rejected; `--root DIR` alone and `--prefix P --target T` genuinely
    // differ and must pass.
    #[test]
    fn require_root_distinct_from_host_rejects_the_degenerate_cases() {
        let (_tmp, _g) = crate::test_support::isolate_active_state();

        // `--local` is exempt: self-contained by construction, bootstrapping
        // directly into it (no separate --root) is the intended recipe, not
        // a footgun — see the doc comment on the function under test.
        let local = emerge(["--local", "/tmp/a", "-p", "sys-libs/zlib"]);
        assert!(
            local
                .require_root_distinct_from_host(&local.roots(), "test")
                .is_ok()
        );

        let prefix = emerge(["--prefix", "/tmp/a", "-p", "sys-libs/zlib"]);
        assert!(
            prefix
                .require_root_distinct_from_host(&prefix.roots(), "test")
                .is_err()
        );

        let bare = emerge(["-p", "sys-libs/zlib"]);
        assert!(
            bare.require_root_distinct_from_host(&bare.roots(), "test")
                .is_err()
        );

        let root = emerge(["--root", "/tmp/a", "-p", "sys-libs/zlib"]);
        assert!(
            root.require_root_distinct_from_host(&root.roots(), "test")
                .is_ok()
        );

        let prefix_target = emerge([
            "--prefix",
            "/tmp/a",
            "--target",
            "riscv64-unknown-linux-gnu",
            "-p",
            "sys-libs/zlib",
        ]);
        assert!(
            prefix_target
                .require_root_distinct_from_host(&prefix_target.roots(), "test")
                .is_ok()
        );

        let prefix_root = emerge([
            "--prefix",
            "/tmp/a",
            "--root",
            "/tmp/b",
            "-p",
            "sys-libs/zlib",
        ]);
        assert!(
            prefix_root
                .require_root_distinct_from_host(&prefix_root.outer_roots(), "test")
                .is_ok()
        );

        // The bug this guard is for: `--prefix P --root P --target T` — the
        // degenerate root-equals-prefix case, now also under `--target`.
        // `resolved.is_overlay()` is always false once `--target` sets
        // `base = Some(sysroot)`, so this used to sail straight past
        // undetected; `self.base_roots().is_overlay()` catches it.
        let prefix_root_target = emerge([
            "--prefix",
            "/tmp/a",
            "--root",
            "/tmp/a",
            "--target",
            "riscv64-unknown-linux-gnu",
            "-p",
            "sys-libs/zlib",
        ]);
        assert!(
            prefix_root_target
                .require_root_distinct_from_host(&prefix_root_target.roots(), "test")
                .is_err()
        );
    }

    // `toolchain --setup`'s own, narrower guard: bare `--prefix`/`--local`
    // (no separate `--root`) are the intended recipe for giving that
    // overlay/tree its own compiler and must pass — unlike `stages`'s
    // guard above, which rejects bare `--prefix` specifically. Only a true
    // bare host (nothing given at all) is rejected.
    #[test]
    fn require_destination_not_bare_host_only_rejects_true_bare_host() {
        let (_tmp, _g) = crate::test_support::isolate_active_state();

        let local = emerge(["--local", "/tmp/a", "-p", "sys-libs/zlib"]);
        assert!(
            local
                .require_destination_not_bare_host(&local.roots(), "test")
                .is_ok()
        );

        let prefix = emerge(["--prefix", "/tmp/a", "-p", "sys-libs/zlib"]);
        assert!(
            prefix
                .require_destination_not_bare_host(&prefix.roots(), "test")
                .is_ok()
        );

        let root = emerge(["--root", "/tmp/a", "-p", "sys-libs/zlib"]);
        assert!(
            root.require_destination_not_bare_host(&root.roots(), "test")
                .is_ok()
        );

        let bare = emerge(["-p", "sys-libs/zlib"]);
        assert!(
            bare.require_destination_not_bare_host(&bare.roots(), "test")
                .is_err()
        );
    }

    #[test]
    fn no_cross_keeps_base_roots() {
        let (_tmp, _g) = crate::test_support::isolate_active_state();
        let cli = emerge(["-p", "sys-libs/zlib"]);
        let r = cli.roots();
        assert_eq!(r.config(), None);
        assert_eq!(r.merge_root().as_str(), "/");
    }

    // A registered active `--prefix` is applied when no explicit topology
    // flag is given (dogfooding path for `em active set`).
    #[test]
    fn active_prefix_applies_when_no_explicit_flag() {
        let (tmp, _g) = crate::test_support::isolate_active_state();
        let prefix = tmp.path().join("pfx");
        std::fs::create_dir_all(&prefix).unwrap();
        let prefix_s = prefix.to_str().unwrap();
        let cli_set = parse_cli(&["em", "active", "--prefix", prefix_s, "set"]);
        crate::active::run(
            cli_set.applet.as_ref().and_then(|a| match a {
                Applet::Active(a) => a.command.as_ref(),
                _ => None,
            }),
            &cli_set,
        )
        .unwrap();

        let bare = emerge(["-p", "sys-libs/zlib"]);
        let r = bare.roots();
        let canon = prefix.canonicalize().unwrap();
        let canon_s = canon.to_str().unwrap();
        assert_eq!(r.eprefix().map(|p| p.as_str()), Some(canon_s));
        assert_eq!(r.merge_root().as_str(), canon_s);
        // Overlay: BROOT satisfaction stays the host.
        assert_eq!(bare.base_roots().merge_root().as_str(), "/");
    }

    // Explicit `--root` wins over a registered active prefix
    #[test]
    fn explicit_root_overrides_active_prefix() {
        let (tmp, _g) = crate::test_support::isolate_active_state();
        let prefix = tmp.path().join("pfx");
        std::fs::create_dir_all(&prefix).unwrap();
        let prefix_s = prefix.to_str().unwrap();
        let cli_set = parse_cli(&["em", "active", "--prefix", prefix_s, "set"]);
        crate::active::run(
            cli_set.applet.as_ref().and_then(|a| match a {
                Applet::Active(a) => a.command.as_ref(),
                _ => None,
            }),
            &cli_set,
        )
        .unwrap();

        let cli = emerge(["--root", "/srv/x", "-p", "sys-libs/zlib"]);
        let r = cli.roots();
        assert_eq!(r.merge_root().as_str(), "/srv/x");
        assert_eq!(
            r.eprefix(),
            None,
            "active prefix must not leak under --root"
        );
    }

    // `--local` is a standalone prefix: base == target == ~/.gentoo (full
    // closure, own VDB), not an overlay (base would be the host). Previously
    // base was None (host) — wrong for cross on a foreign host, where there's
    // no host VDB to seed the plan. See docs/design/root-topology.md § "Override
    // semantics".
    #[test]
    fn local_is_standalone_not_overlay() {
        // HOME is process-global; lock against other tests reading/writing
        // it, and save/restore its value. (Edition 2024 makes set_var unsafe.)
        let _home_lock = crate::test_support::home_lock();
        let saved = std::env::var("HOME").ok();
        // SAFETY: no other thread in this test process touches HOME.
        unsafe {
            std::env::set_var("HOME", "/tmp/fake-home");
        }
        let cli = emerge(["--local", "-p", "sys-libs/zlib"]);
        let r = cli.base_roots();
        assert_eq!(
            r.base().unwrap().as_str(),
            "/tmp/fake-home/.gentoo",
            "--local base must be the prefix (standalone), not the host"
        );
        assert_eq!(
            r.base(),
            r.target(),
            "--local base == target (full closure)"
        );
        // No make.profile yet → config stays host-default (None).
        assert_eq!(r.config(), None);
        // Restore.
        unsafe {
            match &saved {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[test]
    fn local_uses_prefix_config_when_make_profile_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let prefix = tmp.path().join("local-prefix");
        std::fs::create_dir_all(prefix.join("etc/portage")).unwrap();
        // make.profile can be a symlink or empty dir; existence is enough.
        std::fs::create_dir_all(prefix.join("etc/portage/make.profile")).unwrap();
        let prefix_s = prefix.to_str().unwrap();
        let cli = emerge(["--local", prefix_s, "-p", "sys-libs/zlib"]);
        let r = cli.base_roots();
        assert_eq!(
            r.config().map(|p| p.as_str()),
            Some(prefix_s),
            "--local with make.profile must use the prefix as config root"
        );
    }

    // `--prefix` sets EPREFIX: the installed tree is relocatable, so ebuilds
    // bake ${EPREFIX}/usr/bin/pythonX.Y into shebangs. The overlay then
    // symlinks host python there (setup.rs) to satisfy them without building
    // a prefix python. See docs/design/root-topology.md § "Override semantics".
    #[test]
    fn prefix_sets_eprefix_for_relocatable_overlay() {
        let cli = emerge(["--prefix", "/opt/p", "-p", "sys-libs/zlib"]);
        let r = cli.base_roots();
        assert_eq!(
            r.eprefix().unwrap().as_str(),
            "/opt/p",
            "--prefix must set EPREFIX (relocatable installed tree)"
        );
        // Overlay: base is the host (None), not the prefix.
        assert_eq!(r.base(), None, "--prefix base is the host (overlay)");
    }

    // `--prefix` BROOT is the host: `base_roots().merge_root()` (BROOT, where
    // preflight checks BDEPEND) is `/`, while `roots().merge_root()` (the
    // actual install target) is the prefix. These two genuinely differ for an
    // overlay; conflating them made preflight check jinja2's BDEPEND against
    // the empty prefix VDB instead of the host, failing the build.
    // See docs/design/root-topology.md § "Override semantics".
    #[test]
    fn prefix_overlay_broot_is_host_not_prefix() {
        let cli = emerge(["--prefix", "/opt/p", "-p", "sys-libs/zlib"]);
        // BROOT (base_roots) → host `/`.
        assert_eq!(
            cli.base_roots().merge_root().as_str(),
            "/",
            "base_roots().merge_root() must be the host (BROOT) under --prefix"
        );
        // Install target (roots) → the prefix.
        assert_eq!(
            cli.roots().merge_root().as_str(),
            "/opt/p",
            "roots().merge_root() must be the prefix (install target) under --prefix"
        );
    }

    // `--prefix` is an unprivileged overlay: it cannot write the real host
    // `/`, so an unsatisfied `MergeRoot::Host` plan entry (`entry_roots()`
    // in `merge/mod.rs`, fed by `Cli::host_roots()`) must merge into the prefix
    // instead — unlike `--root`, where the same entry correctly lands on
    // the real host because that invocation has root. `host_roots()`'s `.broot`
    // field (the *satisfaction* root) stays the host either way; only the
    // merge destination (`merge_root()`) differs here.
    #[test]
    fn prefix_overlay_broot_merges_into_prefix_not_host() {
        let cli = emerge(["--prefix", "/opt/p", "-p", "sys-libs/zlib"]);
        let broot = cli.host_roots();
        assert_eq!(
            broot.merge_root().as_str(),
            "/opt/p",
            "an unsatisfied Host-routed BDEPEND must merge into the prefix under --prefix"
        );
        assert_eq!(
            broot.satisfaction_root(DepClass::Bdepend).as_str(),
            "/",
            "BDEPEND satisfaction must still be checked against the host under --prefix"
        );
    }

    // Portage `ROOT=`/`{target}-emerge` parity: `--root R`'s BROOT is the
    // real host `/`, not `R`. `R` only receives the *install*; BDEPEND
    // tools run against (and are checked against) the host, exactly like
    // `--prefix`. Previously `base_roots().merge_root()` was (mis)used for
    // this and returned `R`, making an offset build check BDEPEND against
    // the (usually near-empty) offset VDB instead of the host's —
    // `roots().satisfaction_root(DepClass::BDepend)` is the dedicated
    // accessor now; `base_roots()` keeps its own, different "outer EROOT"
    // meaning (see both their doc comments).
    #[test]
    fn root_broot_is_host_not_offset() {
        let cli = emerge(["--root", "/srv/x", "-p", "sys-libs/zlib"]);
        assert_eq!(
            cli.roots().satisfaction_root(DepClass::Bdepend).as_str(),
            "/",
            "roots().satisfaction_root(BDepend) must be the host under --root"
        );
        assert_eq!(
            cli.base_roots().merge_root().as_str(),
            "/srv/x",
            "base_roots().merge_root() (outer EROOT) must stay the offset under --root"
        );
        assert_eq!(
            cli.roots().merge_root().as_str(),
            "/srv/x",
            "roots().merge_root() (install target) must stay the offset under --root"
        );
    }

    // `--local DIR` uses `DIR` directly as the standalone prefix root (not
    // `DIR/.gentoo` — that expansion only applies to the bare-flag default,
    // covered by `local_is_standalone_not_overlay`).
    #[test]
    fn local_with_path_uses_dir_directly() {
        let cli = emerge(["--local", "/tmp/x", "-p", "sys-libs/zlib"]);
        let r = cli.base_roots();
        assert_eq!(r.base().unwrap().as_str(), "/tmp/x");
        assert_eq!(r.target().unwrap().as_str(), "/tmp/x");
        assert_eq!(r.eprefix().unwrap().as_str(), "/tmp/x");
    }

    fn emerge_applet(cli: &Cli) -> &EmergeArgs {
        match &cli.applet {
            Some(Applet::Emerge(a)) => a,
            other => panic!("expected emerge, got {other:?}"),
        }
    }

    fn overlay_root_of(cli: &Cli) -> Option<&str> {
        match cli.applet_root_arg() {
            Some(a) => a.root.as_deref().or(cli.root.as_deref()),
            None => cli.root.as_deref(),
        }
    }

    #[test]
    fn bare_and_explicit_emerge_produce_identical_args() {
        let bare = parse_cli(&["em", "--root", "/srv/x", "-p", "sys-libs/zlib"]);
        let explicit = parse_cli(&["em", "emerge", "--root", "/srv/x", "-p", "sys-libs/zlib"]);
        assert_eq!(overlay_root_of(&bare), Some("/srv/x"));
        assert_eq!(overlay_root_of(&explicit), Some("/srv/x"));
        assert_eq!(emerge_applet(&bare).atoms, emerge_applet(&explicit).atoms);
        assert!(bare.pretend && explicit.pretend);
    }

    #[test]
    fn flags_before_emerge_word_reorder() {
        let cli = parse_cli(&["em", "--root", "/srv/x", "emerge", "-p", "sys-libs/zlib"]);
        assert_eq!(overlay_root_of(&cli), Some("/srv/x"));
        assert_eq!(emerge_applet(&cli).atoms, vec!["sys-libs/zlib".to_string()]);
        assert!(cli.pretend);
    }

    #[test]
    fn unrecognized_flag_inside_a_real_subcommand_does_not_retry_as_emerge() {
        let err = parse_err(&["em", "crossdev", "--bogus-flag"]);
        assert_eq!(err, "UnknownFlag --bogus-flag");
    }

    #[test]
    fn prefix_root_then_crossdev_is_try_into_reject() {
        let cli = parse_cli(&["em", "--root", "/tmp/r", "crossdev", "--setup"]);
        assert_eq!(cli.root.as_deref(), Some("/tmp/r"));
        assert!(matches!(cli.applet, Some(Applet::Crossdev(_))));
        let err = parse_cli_into(&["em", "--root", "/tmp/r", "crossdev", "--setup"]).unwrap_err();
        assert!(
            err.contains("InvalidValue --root"),
            "try_into must reject prefix --root with crossdev, got {err}"
        );
    }

    #[test]
    fn top_level_help_is_root_help() {
        use usage::test::{self as harness, Outcome};
        let words = harness::argv(["--help"]);
        let Outcome::Help(printed) = harness::outcome(Cli::spec(), &words.words(), Cli::parse_from)
        else {
            panic!("--help should be Help");
        };
        assert!(!printed.stderr);
        assert_eq!(printed.code, 0);
        assert!(
            printed.text.contains("query") && printed.text.contains("crossdev"),
            "{}",
            printed.text
        );
        assert!(
            !printed.text.contains("Usage: em emerge"),
            "{}",
            printed.text
        );
    }

    #[test]
    fn applet_help_is_toolchain_help() {
        use usage::test::{self as harness, Outcome};
        let words = harness::argv(["toolchain", "--help"]);
        let Outcome::Help(printed) = harness::outcome(Cli::spec(), &words.words(), Cli::parse_from)
        else {
            panic!("toolchain --help should be Help");
        };
        assert!(printed.text.contains("toolchain"), "{}", printed.text);
        assert!(
            !printed.text.contains("Usage: em emerge"),
            "{}",
            printed.text
        );
    }

    #[test]
    fn version_is_not_emerge() {
        use usage::test::{self as harness, Outcome};
        let words = harness::argv(["--version"]);
        let Outcome::Version(_) = harness::outcome(Cli::spec(), &words.words(), Cli::parse_from)
        else {
            panic!("--version should be Version");
        };
    }

    #[test]
    fn bare_em_is_root_help() {
        use usage::test::{self as harness, Outcome};
        let words = harness::argv([] as [&str; 0]);
        let Outcome::Help(printed) = harness::outcome(Cli::spec(), &words.words(), Cli::parse_from)
        else {
            panic!("bare em should be MissingArgsHelp");
        };
        assert!(printed.stderr);
        assert_eq!(printed.code, 2);
        assert!(printed.text.contains("query"), "{}", printed.text);
        assert!(printed.text.contains("crossdev"), "{}", printed.text);
        assert!(
            !printed.text.contains("Usage: em emerge"),
            "{}",
            printed.text
        );
        assert!(
            !printed.text.contains("__worker"),
            "hidden worker leaked: {}",
            printed.text
        );
    }

    #[test]
    fn dash_p_alone_parses_without_defaulting_to_emerge() {
        let cli = parse_cli(&["em", "-p"]);
        assert!(cli.pretend);
        assert!(cli.applet.is_none());
    }

    #[test]
    fn info_json_parses_without_emerge() {
        let cli = parse_cli(&["em", "--info", "--json"]);
        assert!(cli.info);
        assert!(cli.merge_flags.json);
        assert!(cli.applet.is_none());
    }

    #[test]
    fn info_use_selects_use() {
        let cli = parse_cli(&["em", "--info", "use"]);
        assert!(cli.info);
        assert!(matches!(cli.applet, Some(Applet::Use(_))));
    }

    #[test]
    fn info_firefox_is_emerge_not_info_only() {
        let cli = parse_cli(&["em", "--info", "firefox"]);
        assert!(cli.info);
        assert_eq!(emerge_applet(&cli).atoms, ["firefox"]);
    }

    #[test]
    fn emerge_info_is_unknown_flag() {
        assert_eq!(parse_err(&["em", "emerge", "--info"]), "UnknownFlag --info");
    }

    #[test]
    fn arch_and_repo_work_on_the_bare_path() {
        let cli = parse_cli(&["em", "--arch", "amd64", "-p", "sys-libs/zlib"]);
        assert_eq!(cli.arch, Arch::from_str("amd64").unwrap());
        assert_eq!(emerge_applet(&cli).atoms, vec!["sys-libs/zlib".to_string()]);

        let via_emerge = parse_cli(&["em", "--arch", "amd64", "emerge", "-p", "sys-libs/zlib"]);
        assert_eq!(via_emerge.arch, cli.arch);

        let repo = parse_cli(&["em", "--repo", "/tmp/r", "-p", "sys-libs/zlib"]);
        assert_eq!(repo.repo.as_deref(), Some("/tmp/r"));
    }

    #[test]
    fn json_before_emerge_word_is_merge_plan_json() {
        let before = parse_cli(&["em", "--json", "emerge", "-p", "sys-libs/zlib"]);
        assert!(before.merge_flags().json);
        let explicit = parse_cli(&["em", "emerge", "--json", "-p", "sys-libs/zlib"]);
        assert!(explicit.merge_flags().json);
        let bare = parse_cli(&["em", "--json", "-p", "sys-libs/zlib"]);
        assert!(bare.merge_flags().json);
    }

    #[test]
    fn exclude_value_matching_an_applet_name_is_not_the_search_applet() {
        let cli = parse_cli(&["em", "-X", "search", "-p", "sys-libs/zlib"]);
        assert_eq!(cli.merge_flags().exclude, vec!["search".to_string()]);
        assert_eq!(emerge_applet(&cli).atoms, vec!["sys-libs/zlib".to_string()]);
        assert!(matches!(cli.applet, Some(Applet::Emerge(_))));
    }

    #[test]
    fn root_value_named_emerge_is_kept() {
        let cli = parse_cli(&["em", "--root", "emerge", "-p", "sys-libs/zlib"]);
        assert_eq!(overlay_root_of(&cli), Some("emerge"));
        assert_eq!(emerge_applet(&cli).atoms, vec!["sys-libs/zlib".to_string()]);
    }

    #[test]
    fn crossdev_rejects_root_after_the_applet() {
        assert_eq!(
            parse_err(&["em", "crossdev", "--setup", "--root", "/tmp/a"]),
            "UnknownFlag --root"
        );
        assert_eq!(
            parse_err(&["em", "crossdev", "--root", "/tmp/a", "--setup"]),
            "UnknownFlag --root"
        );
    }

    #[test]
    fn prefix_before_named_applet_is_topology() {
        let cli = parse_cli(&["em", "--prefix", "P", "firefox"]);
        assert_eq!(cli.topology.prefix.as_deref(), Some("P"));
        assert_eq!(emerge_applet(&cli).atoms, ["firefox"]);

        let tc = parse_cli(&["em", "--prefix", "P", "toolchain", "--setup"]);
        assert_eq!(tc.topology.prefix.as_deref(), Some("P"));
        assert!(matches!(tc.applet, Some(Applet::Toolchain(_))));

        let canon = parse_cli(&["em", "toolchain", "--prefix", "P", "--setup"]);
        assert_eq!(canon.topology.prefix.as_deref(), Some("P"));
        assert!(matches!(canon.applet, Some(Applet::Toolchain(_))));
    }

    #[test]
    fn query_depgraph_nested_root_and_prefix() {
        let prefix = parse_cli(&["em", "query", "depgraph", "--prefix", "P", "zlib"]);
        assert_eq!(prefix.topology.prefix.as_deref(), Some("P"));
        assert!(matches!(prefix.applet, Some(Applet::Query(_))));

        let root = parse_cli(&["em", "query", "depgraph", "--root", "R", "zlib"]);
        match &root.applet {
            Some(Applet::Query(q)) => assert_eq!(q.root_arg.root.as_deref(), Some("R")),
            other => panic!("expected query, got {other:?}"),
        }
    }

    #[test]
    fn global_pretend_both_orders() {
        let before = parse_cli(&["em", "-p", "toolchain"]);
        assert!(before.pretend);
        assert!(matches!(before.applet, Some(Applet::Toolchain(_))));
        let after = parse_cli(&["em", "toolchain", "-p"]);
        assert!(after.pretend);
        assert!(matches!(after.applet, Some(Applet::Toolchain(_))));
    }

    #[test]
    fn use_dash_a_is_add_not_ask() {
        let cli = parse_cli(&["em", "use", "-a", "png"]);
        match &cli.applet {
            Some(Applet::Use(u)) => assert_eq!(u.add, ["png"]),
            other => panic!("expected use, got {other:?}"),
        }
        assert!(!cli.merge_flags().ask);
    }

    #[test]
    fn use_dash_e_is_add() {
        let cli = parse_cli(&["em", "use", "-E", "png"]);
        match &cli.applet {
            Some(Applet::Use(u)) => assert_eq!(u.add, ["png"]),
            other => panic!("expected use, got {other:?}"),
        }
    }

    #[test]
    fn search_dash_a_is_all() {
        let cli = parse_cli(&["em", "search", "-a"]);
        match &cli.applet {
            Some(Applet::Search(s)) => assert!(s.all),
            other => panic!("expected search, got {other:?}"),
        }
        assert!(!cli.merge_flags().ask);
    }

    #[test]
    fn prefix_ask_then_search_is_try_into_reject() {
        let cli = parse_cli(&["em", "-a", "search", "zlib"]);
        assert!(cli.merge_flags.ask);
        assert!(matches!(cli.applet, Some(Applet::Search(_))));
        let err = parse_cli_into(&["em", "-a", "search", "zlib"]).unwrap_err();
        assert!(
            err.contains("InvalidValue --ask"),
            "try_into must reject prefix --ask before a non-merge applet, got {err}"
        );
    }

    #[test]
    fn emerge_dash_a_is_ask() {
        let cli = parse_cli_into(&["em", "emerge", "-a", "pkg"]).expect("ask");
        assert!(cli.merge_flags().ask);
        assert_eq!(emerge_applet(&cli).atoms, ["pkg"]);
    }

    #[test]
    fn prefix_deep_then_query_is_try_into_reject() {
        let err = parse_cli_into(&["em", "--deep", "query", "depgraph", "zlib"]).unwrap_err();
        assert!(err.contains("InvalidValue emerge-mixin"), "got {err}");
        let ok = parse_cli(&["em", "query", "depgraph", "--deep", "zlib"]);
        match &ok.applet {
            Some(Applet::Query(q)) => match &q.command {
                QueryCommand::Depgraph { depgraph_flags, .. } => assert!(depgraph_flags.deep),
                other => panic!("expected depgraph, got {other:?}"),
            },
            other => panic!("expected query, got {other:?}"),
        }
    }

    #[test]
    fn bundled_update_deep_then_query_is_try_into_reject() {
        let err =
            parse_cli_into(&["em", "-uD", "query", "belongs", "/usr/bin/python"]).unwrap_err();
        assert!(err.contains("InvalidValue emerge-mixin"), "got {err}");
    }

    #[test]
    fn exclude_applet_wins_and_bools_or() {
        let cli = parse_cli(&["em", "-X", "foo", "emerge", "-X", "bar", "pkg"]);
        assert_eq!(cli.merge_flags().exclude, vec!["bar".to_string()]);

        let or_flags = parse_cli(&["em", "-u", "emerge", "-D", "pkg"]);
        assert!(or_flags.merge_flags().update);
        assert!(or_flags.depgraph_flags().deep);
    }

    #[test]
    fn privilege_prefix_none_beats_env_sudo() {
        let saved = std::env::var("EM_PRIVILEGE").ok();
        // SAFETY: this test is the only site that writes EM_PRIVILEGE.
        unsafe { std::env::set_var("EM_PRIVILEGE", "sudo") };
        let cli = parse_cli(&["em", "--privilege", "none", "emerge", "pkg"]);
        assert_eq!(cli.effective_privilege(), Privilege::None);
        unsafe {
            match saved {
                Some(v) => std::env::set_var("EM_PRIVILEGE", v),
                None => std::env::remove_var("EM_PRIVILEGE"),
            }
        }
    }

    #[test]
    fn local_default_missing_and_the_set_trap() {
        let ok = parse_cli(&["em", "active", "set", "--local="]);
        assert_eq!(ok.topology.local.as_deref(), Some(""));
        match &ok.applet {
            Some(Applet::Active(a)) => {
                assert!(matches!(a.command, Some(ActiveCommand::Set { .. })))
            }
            other => panic!("expected active set, got {other:?}"),
        }

        let stolen = parse_cli(&["em", "active", "--local", "set"]);
        assert_eq!(stolen.topology.local.as_deref(), Some("set"));
        match &stolen.applet {
            Some(Applet::Active(a)) => assert!(a.command.is_none()),
            other => panic!("expected active, got {other:?}"),
        }
    }

    fn worker_argv<'a>(extra: &'a [&'a str]) -> Vec<&'a str> {
        let mut argv = vec![
            "em",
            "__worker",
            "--ebuild",
            "/tmp/pkg.ebuild",
            "--cpv",
            "cat/pkg-1",
            "--use-flags",
            "",
            "--work-base",
            "/tmp/work",
            "--root",
            "/tmp/root",
        ];
        argv.extend_from_slice(extra);
        argv
    }

    #[test]
    fn worker_quiet_is_the_cli_global() {
        let argv = worker_argv(&["--quiet"]);
        let cli = parse_cli(&argv);
        assert!(cli.quiet);
        let Some(Applet::Worker(w)) = &cli.applet else {
            panic!("expected Applet::Worker");
        };
        assert_eq!(w.root, "/tmp/root");
        assert_eq!(w.worker_config_root, None);
    }

    #[test]
    fn worker_config_root_is_not_the_topology_flag() {
        let argv = worker_argv(&["--worker-config-root", "/tmp/cfg"]);
        let cli = parse_cli(&argv);
        let Some(Applet::Worker(w)) = &cli.applet else {
            panic!("expected Applet::Worker");
        };
        assert_eq!(w.worker_config_root.as_deref(), Some("/tmp/cfg"));
        let bad = worker_argv(&["--config-root", "/tmp/cfg"]);
        // Topology --config-root is a global, so it binds Cli.topology rather than failing.
        let global = parse_cli(&bad);
        assert_eq!(global.topology.config_root.as_deref(), Some("/tmp/cfg"));
        let Some(Applet::Worker(w)) = &global.applet else {
            panic!("expected Applet::Worker");
        };
        assert!(w.worker_config_root.is_none());
    }

    #[test]
    fn helper_hyphen_args_after_double_dash() {
        let cli = parse_cli(&["em", "__helper", "dodoc", "--", "-foo"]);
        match &cli.applet {
            Some(Applet::Helper(h)) => {
                assert_eq!(h.name, "dodoc");
                assert_eq!(h.args, ["-foo"]);
            }
            other => panic!("expected helper, got {other:?}"),
        }
    }

    #[test]
    fn firefox_defaults_to_emerge() {
        let cli = parse_cli(&["em", "firefox"]);
        assert_eq!(emerge_applet(&cli).atoms, ["firefox"]);
    }
}
/// Hidden `em __worker` install child — spawned per package by `build_and_merge`.
///
/// `--quiet` is the Cli global, not a field here. `--config-root` is Topology's;
/// this child takes `--worker-config-root` so the two never share a spelling.
#[derive(usage::Args, Debug)]
pub struct WorkerArgs {
    #[usage(long)]
    pub ebuild: String,
    /// The resolved plan entry's authoritative cpv — see
    /// `privilege::WorkerArgs::cpv`.
    #[usage(long)]
    pub cpv: String,
    #[usage(long)]
    pub use_flags: String,
    #[usage(long)]
    pub work_base: String,
    #[usage(long)]
    pub root: String,
    #[usage(long)]
    pub distdir: Option<String>,
    #[usage(long)]
    pub worker_config_root: Option<String>,
    #[usage(long)]
    pub sysroot: Option<String>,
    #[usage(long)]
    pub eprefix: Option<String>,
    /// Where BDEPEND-class build tools live (`Cli::host_roots()`'s merge root)
    #[usage(long)]
    pub broot: Option<String>,
    /// See `ebuild::RootContext::self_contained_bootstrap`
    #[usage(long)]
    pub self_contained_bootstrap: bool,
    /// See `ebuild::RootContext::extra_path`, `:`-joined
    #[usage(long)]
    pub extra_path: Option<String>,
    /// A pre-built GPKG to merge (`-k`/`-g`)
    #[usage(long)]
    pub binpkg: Option<String>,
    /// `binpkg`'s origin forces cryptographic GPG signature
    /// verification (a `binrepos.conf` entry with
    /// `verify-signature = yes`), independent of
    /// `FEATURES=binpkg-request-signature`.
    #[usage(long)]
    pub force_verify_signature: bool,
    #[usage(long)]
    pub buildpkg: bool,
    /// Parent activity session id — live FS phase updates only
    #[usage(long)]
    pub activity_job_id: Option<String>,
    #[usage(long)]
    pub activity_parent_job_id: Option<String>,
    /// Filesystem root of the parent's live activity sink
    #[usage(long)]
    pub activity_live_root: Option<String>,
    /// `host` or `target` package side for inflight paths
    #[usage(long)]
    pub activity_side: Option<String>,
    /// Unix socket path: stream phase JSONL back to the parent activity bus
    #[usage(long)]
    pub activity_reemit_path: Option<String>,
}

#[derive(usage::Subcommands, Debug)]
#[allow(clippy::large_enum_variant)] // __worker carries many CLI strings
pub enum Applet {
    /// Run one do*/new* install helper standalone against the exported build env
    ///
    /// Internal: backs the PATH shims dropped during a build so `find -exec doman` /
    /// `xargs do*` reach helpers that are in-shell builtins. Not for direct use.
    #[usage(name = "__helper", hide)]
    Helper(HelperArgs),

    /// Internal: the privilege-wrapped install worker (install+qmerge+binpkg
    /// for one package; spawned per package by `build_and_merge`).
    #[usage(name = "__worker", hide)]
    Worker(WorkerArgs),

    #[usage(help = "Execute ebuild phases")]
    Ebuild(EbuildArgs),

    #[usage(help = "System maintenance and health checks")]
    Maint(MaintArgs),

    #[usage(help = "Query Portage internal variables and data")]
    Portageq(PortageqArgs),

    /// Sync ebuild repositories from `repos.conf` (`git` and `rsync`)
    ///
    /// With no names, syncs every entry with `auto-sync = yes` (Portage
    /// default) and a usable `sync-type`/`sync-uri`. Named repos are synced
    /// regardless of `auto-sync`.
    ///
    /// Default backends shell out to `git` / `rsync` (Portage parity). Build
    /// with `--features sync-gix` for the experimental pure-gix git path.
    ///
    /// Identical implementation to `em maint sync` — this top-level form
    /// exists only because `sync` is common enough to deserve a short
    /// invocation, matching real Portage having both `emerge --sync` and
    /// `emaint sync`.
    // Both dispatch to `crate::maint::sync::run`.
    #[usage(help = "Sync repositories (git, rsync)")]
    Sync(SyncArgs),

    #[usage(help = "Remove orphaned/unused packages")]
    Depclean(DepcleanArgs),

    #[usage(help = "Regenerate metadata cache")]
    Regen(RegenArgs),

    #[usage(help = "Create binary packages from installed files")]
    Quickpkg(QuickpkgArgs),

    #[usage(
        name = "mirrordist",
        alias_hidden = "emirrordist",
        help = "Build/maintain a distfiles mirror (emirrordist workalike)",
        long_help = "Walks every ebuild in a repository, fetches every distfile its SRC_URI references (all versions, all USE branches), and verifies each against the repo Manifest — the server side of a Gentoo mirror.\n\nNot to be confused with `em select mirrors`, which chooses which mirrors *this* machine fetches from.\n\nRequires an up-to-date metadata cache: run `em regen <repo>` first for overlays."
    )]
    MirrorDist(MirrorDistArgs),

    #[usage(help = "Query package information")]
    Query(QueryArgs),

    #[usage(help = "Clean distfiles and/or binary packages")]
    Clean(CleanArgs),

    #[usage(help = "Enable/disable/query USE flags in make.conf")]
    Use(UseArgs),

    #[usage(help = "Edit per-package configuration (package.use, .keywords, .mask, .env)")]
    Pkg(PkgArgs),

    #[usage(help = "Rebuild packages with broken shared library deps")]
    Revdep(RevdepArgs),

    #[usage(help = "Display Portage elog files")]
    Read(ReadArgs),

    #[usage(help = "Analyze emerge.log")]
    Log(LogArgs),

    #[usage(help = "Search inside ebuilds and eclasses")]
    Grep(GrepArgs),

    #[usage(help = "Search package names and descriptions")]
    Search(SearchArgs),

    #[usage(help = "Parse/split atom strings")]
    Atom(AtomArgs),

    #[usage(help = "Native config selectors (profile, repos) — eselect-like")]
    Select(SelectArgs),

    /// Register a default `--prefix` / `--local` so bare `em <pkg>` picks it up (dogfooding)
    ///
    /// Explicit `--prefix`/`--local`/`--root` still win. State is stored under
    /// `$XDG_STATE_HOME/em/active`.
    #[usage(help = "Register a default --prefix/--local for bare em invocations")]
    Active(ActiveArgs),

    #[usage(help = "Bootstrap a prefix layout (use with --local or --prefix)")]
    Setup(SetupArgs),

    #[usage(help = "Set up a cross-compilation target (sysroot + overlay) — crossdev workalike")]
    Crossdev(CrossdevArgs),

    #[usage(help = "Bootstrap a self-hosting native toolchain into --root (the stages' compiler)")]
    Toolchain(ToolchainArgs),

    #[usage(help = "Assemble stage-build artifacts (stage1 packages.build) into --root")]
    Stages(StagesArgs),

    #[usage(
        help = "Reconcile pending config files (etc-update / dispatch-conf)",
        alias_hidden = "config",
        alias_hidden = "dispatch"
    )]
    Etc(EtcArgs),

    #[usage(help = "Regenerate /etc/profile.env and ld.so cache")]
    Env(EnvArgs),

    /// Resolve and merge/unmerge packages (emerge workalike).
    ///
    /// `em <atoms>` and `em emerge <atoms>` parse into the same arguments.
    #[usage(help = "Resolve and merge/unmerge packages (emerge workalike)")]
    Emerge(EmergeArgs),
}

/// Hidden `em __helper` shim — PATH helper dispatched by `find -exec`/`xargs`.
#[derive(usage::Args, Debug)]
pub struct HelperArgs {
    /// Helper name (e.g. `doman`, `dolib.a`)
    pub name: String,
    /// Arguments passed through to the helper
    #[usage(double_dash = "automatic")]
    pub args: Vec<String>,
}

/// `em ebuild` — execute ebuild phases
#[derive(usage::Args, Debug)]
pub struct EbuildArgs {
    /// Path to the `.ebuild` file to execute
    pub ebuild_path: String,
    /// Phase(s) to run in order (e.g. `compile`, `install`, `qmerge`)
    #[usage(required)]
    pub phase: Vec<String>,
    /// Override the build work directory (default: `/var/tmp/portage/<cat>/<pf>`)
    #[usage(short = 'w', long, value_name = "DIR")]
    pub work_dir: Option<camino::Utf8PathBuf>,
    #[usage(flatten)]
    pub root_arg: RootArg,
}

/// `em maint` — system maintenance and health checks
#[derive(usage::Args, Debug)]
pub struct MaintArgs {
    #[usage(subcommand)]
    pub command: MaintCommand,
    #[usage(flatten)]
    pub root_arg: RootArg,
}

/// `em portageq` — query Portage internal variables and data
#[derive(usage::Args, Debug)]
pub struct PortageqArgs {
    /// portageq sub-command to run (e.g. `envvar`, `get_repos`)
    pub command: String,
    /// Arguments passed through to the sub-command
    #[usage(double_dash = "automatic")]
    pub args: Vec<String>,
}

/// `em sync` — sync repositories (git, rsync)
#[derive(usage::Args, Debug)]
pub struct SyncArgs {
    /// Repo names from repos.conf (default: auto-sync enabled repos)
    pub repos: Vec<String>,
    #[usage(flatten)]
    pub root_arg: RootArg,
}

/// `em depclean` — remove orphaned/unused packages
#[derive(usage::Args, Debug)]
pub struct DepcleanArgs {
    /// Restrict cleaning to these atoms' dependency closure (every other
    /// installed package is protected). Default: the whole `@world` set
    #[usage(double_dash = "automatic")]
    pub atoms: Vec<String>,
    #[usage(flatten)]
    pub root_arg: RootArg,
    /// `--exclude`/`--with-bdeps` only — the rest of `MergeFlags` means
    /// nothing to depclean's own read-then-remove walk.
    #[usage(flatten)]
    pub merge_flags: MergeFlags,
}

/// `em regen` — regenerate metadata cache
#[derive(usage::Args, Debug)]
pub struct RegenArgs {
    /// Repo names or paths to regenerate (default: every repo except the
    /// main one, whose cache is normally maintained upstream)
    pub repos: Vec<String>,
    /// Write cache files to this directory instead of metadata/md5-cache
    #[usage(short = 'o', long, value_name = "DIR")]
    pub output: Option<std::path::PathBuf>,
    /// Directory containing master repositories
    #[usage(long, value_name = "DIR")]
    pub repos_dir: Option<String>,
    /// Number of parallel workers
    #[usage(short = 'j', long)]
    pub jobs: Option<usize>,
    /// Deduplicate top-level dep tokens before writing
    #[usage(long)]
    pub dedup: bool,
    /// Activity-output flags (`--activity-fd`/`--activity-jsonl`/
    /// `--emergelog`) — `em regen` drives its own activity bus.
    #[usage(flatten)]
    pub activity: ActivityArgs,
    #[usage(flatten)]
    pub root_arg: RootArg,
}

/// `em quickpkg` — create binary packages from installed files
#[derive(usage::Args, Debug)]
pub struct QuickpkgArgs {
    /// Atoms, package sets (`@system`), or VDB paths (`/var/db/pkg/cat/pf`)
    #[usage(required)]
    pub atoms: Vec<String>,
    /// Include CONFIG_PROTECT files
    #[usage(long)]
    pub include_config: bool,
    /// Include unmodified CONFIG_PROTECT files
    #[usage(long)]
    pub include_unmodified_config: bool,
    #[usage(flatten)]
    pub root_arg: RootArg,
}

/// `em mirrordist` — build/maintain a distfiles mirror
#[derive(usage::Args, Debug)]
pub struct MirrorDistArgs {
    /// repos.conf name or path
    ///
    /// Defaults to the main repo (opposite default from `em regen`, which excludes it).
    pub repo: Option<String>,
    /// Directory containing master repositories
    #[usage(long, value_name = "DIR")]
    pub repos_dir: Option<String>,
    /// Distfiles directory to populate
    #[usage(long, value_name = "DIR")]
    pub distfiles: camino::Utf8PathBuf,
    /// Concurrent downloads
    #[usage(short = 'j', long)]
    pub jobs: Option<usize>,
    /// Delete distfiles no longer referenced by any ebuild
    #[usage(long)]
    pub delete: bool,
    /// Grace period before an orphaned file is deleted (e.g. `7d`, `72h`)
    #[usage(long, value_name = "DURATION", default = "7d")]
    pub deletion_delay: String,
    /// Deletion-grace state file (default: `$XDG_STATE_HOME/em/mirrordist/<repo>-*.json`)
    #[usage(long, value_name = "FILE")]
    pub deletion_db: Option<camino::Utf8PathBuf>,
    /// Tab-delimited log of fetched files (appended)
    #[usage(long, value_name = "FILE")]
    pub success_log: Option<camino::Utf8PathBuf>,
    /// Tab-delimited log of fetch failures (appended)
    #[usage(long, value_name = "FILE")]
    pub failure_log: Option<camino::Utf8PathBuf>,
    /// Report of files scheduled for deletion, grouped by date (rewritten)
    #[usage(long, value_name = "FILE")]
    pub scheduled_deletion_log: Option<camino::Utf8PathBuf>,
    /// File(s) listing distfile names --delete must never remove (one
    /// name per line, `#`-comments ignored).
    #[usage(long, value_name = "FILE")]
    pub whitelist_from: Vec<camino::Utf8PathBuf>,
    /// Re-hash already-present files instead of trusting their size
    #[usage(long)]
    pub verify_existing_digest: bool,
    /// Also try GENTOO_MIRRORS after the ebuild's own URIs (real
    /// emirrordist never does this — off by default).
    #[usage(long)]
    pub gentoo_mirrors_fallback: bool,
    /// Allow --delete even when some ebuilds had no metadata cache entry
    #[usage(long)]
    pub delete_allow_incomplete: bool,
    #[usage(flatten)]
    pub root_arg: RootArg,
}

/// `em query` — query package information
#[derive(usage::Args, Debug)]
pub struct QueryArgs {
    #[usage(subcommand)]
    pub command: QueryCommand,
    #[usage(flatten)]
    pub root_arg: RootArg,
}

/// `em clean` — clean distfiles and/or binary packages
#[derive(usage::Args, Debug)]
pub struct CleanArgs {
    #[usage(subcommand)]
    pub target: CleanTarget,
    #[usage(flatten)]
    pub root_arg: RootArg,
}

/// `em use` — enable/disable/query USE flags in make.conf
#[derive(usage::Args, Debug)]
pub struct UseArgs {
    /// Add (enable) flags — euse calls this --enable/-E
    #[usage(short = 'E', short = 'a', long = "add", value_name = "FLAG")]
    pub add: Vec<String>,
    /// Subtract flags (written with leading '-', e.g. -themes) — euse
    /// calls this --disable/-D
    #[usage(short = 'D', short = 's', long = "subtract", value_name = "FLAG")]
    pub subtract: Vec<String>,
    /// Drop flags entirely (removes both flag and -flag forms) — euse
    /// calls this --remove/-R or --prune/-P
    #[usage(
        short = 'R',
        short = 'P',
        short = 'd',
        long = "drop",
        value_name = "FLAG"
    )]
    pub drop: Vec<String>,
    /// Preview the resulting value without writing make.conf
    #[usage(short = 'n', long = "dry-run")]
    pub dry_run: bool,
    /// Target a USE_EXPAND variable (e.g. VIDEO_CARDS) instead of USE —
    /// -a/-s/-d then edit that variable's value the same way
    #[usage(short = 'e', long = "expand", value_name = "VAR")]
    pub expand: Option<String>,
    /// List every USE_EXPAND variable known to the active profile, each
    /// with its current make.conf value
    #[usage(
        short = 'L',
        long = "list-expand",
        conflicts("add", "subtract", "drop", "expand")
    )]
    pub list_expand: bool,
    /// Show descriptions for the given USE flags (profiles/use.desc and
    /// use.local.desc, searching both unless -g/-l restricts it). With
    /// no flags given, lists every flag in scope
    #[usage(
        short = 'i',
        long = "info",
        value_name = "FLAG",
        conflicts("add", "subtract", "drop", "expand", "list_expand")
    )]
    pub info: Vec<String>,
    /// Restrict -i to global flags only (profiles/use.desc)
    #[usage(
        short = 'g',
        long = "global",
        conflicts("add", "subtract", "drop", "expand", "list_expand", "local_desc")
    )]
    pub global: bool,
    /// Restrict -i to per-package local flags only (profiles/use.local.desc,
    /// searched across every package — see `em query uses <atom>` for a
    /// single package's flags instead)
    #[usage(
        short = 'l',
        long = "local-desc",
        conflicts("add", "subtract", "drop", "expand", "list_expand", "global")
    )]
    pub local_desc: bool,
    /// Path to make.conf (default: resolved like other config commands,
    /// following --config-root/--local/--prefix)
    #[usage(long = "make-conf", value_name = "PATH")]
    pub make_conf: Option<camino::Utf8PathBuf>,
    #[usage(flatten)]
    pub root_arg: RootArg,
}

/// `em pkg` — edit per-package configuration
#[derive(usage::Args, Debug)]
pub struct PkgArgs {
    #[usage(subcommand)]
    pub command: PkgCommand,
    #[usage(flatten)]
    pub root_arg: RootArg,
}

/// `em revdep` — rebuild packages with broken shared library deps
#[derive(usage::Args, Debug)]
pub struct RevdepArgs {
    /// Only consider consumers of libraries whose soname contains NAME
    #[usage(short = 'L', long, value_name = "NAME")]
    pub library: Option<String>,
    #[usage(flatten)]
    pub root_arg: RootArg,
    #[usage(flatten)]
    pub merge_flags: MergeFlags,
}

/// `em read` — display Portage elog files
#[derive(usage::Args, Debug)]
pub struct ReadArgs {
    /// Only show packages whose `<category>/<pf>` contains this text
    pub package: Option<String>,
    /// List what is filed instead of printing the messages
    #[usage(short, long)]
    pub list: bool,
    /// Show only this many of the most recent packages; 0 for all
    #[usage(short = 'n', long, default = "10")]
    pub limit: usize,
    /// Remove each file once it has been shown
    #[usage(long)]
    pub delete: bool,
    #[usage(flatten)]
    pub root_arg: RootArg,
}

/// `em log` — analyze emerge.log
#[derive(usage::Args, Debug)]
pub struct LogArgs {
    #[usage(subcommand)]
    pub command: Option<LogCommand>,
    #[usage(flatten)]
    pub root_arg: RootArg,
}

/// `em grep` — search inside ebuilds and eclasses
#[derive(usage::Args, Debug)]
pub struct GrepArgs {
    /// Pattern to search for
    pub pattern: String,
    /// Restrict the search to these ebuild/eclass paths (default: the whole repo)
    #[usage(double_dash = "automatic")]
    pub paths: Vec<String>,
}

/// `em search` — search package names and descriptions
#[derive(usage::Args, Debug)]
pub struct SearchArgs {
    /// List all packages (no pattern required)
    #[usage(short = 'a', long)]
    pub all: bool,
    /// Search package descriptions instead of names
    #[usage(short = 'S', long = "desc")]
    pub desc: bool,
    /// Show only package name, no description
    #[usage(short = 'N', long = "name-only")]
    pub name_only: bool,
    /// Show homepage instead of description
    #[usage(short = 'H', long)]
    pub homepage: bool,
    /// Pattern to search (required unless --all)
    #[usage(required_unless = "all")]
    pub pattern: Option<String>,
    #[usage(flatten)]
    pub root_arg: RootArg,
}

/// `em atom` — parse/split atom strings
#[derive(usage::Args, Debug)]
pub struct AtomArgs {
    /// Atom strings to parse and print back in normalized form
    #[usage(required)]
    pub atoms: Vec<String>,
}

/// `em select` — native config selectors (profile, repos)
#[derive(usage::Args, Debug)]
pub struct SelectArgs {
    #[usage(subcommand)]
    pub command: SelectCommand,
    #[usage(flatten)]
    pub root_arg: RootArg,
}

/// `em active` — register a default `--prefix`/`--local` for bare invocations
#[derive(usage::Args, Debug)]
pub struct ActiveArgs {
    #[usage(subcommand)]
    pub command: Option<ActiveCommand>,
}

/// `em etc` — reconcile pending config files
#[derive(usage::Args, Debug)]
pub struct EtcArgs {
    #[usage(subcommand)]
    pub command: Option<EtcCommand>,
    #[usage(flatten)]
    pub opts: EtcOpts,
    #[usage(flatten)]
    pub root_arg: RootArg,
}

/// `em env` — regenerate `/etc/profile.env` and ld.so cache
#[derive(usage::Args, Debug)]
pub struct EnvArgs {
    #[usage(flatten)]
    pub root_arg: RootArg,
}

/// `em setup` — bootstrap a prefix layout
#[derive(usage::Args, Default, Debug)]
pub struct SetupArgs {
    /// Directory holding host tools this prefix should borrow while it has
    /// none of its own, put ahead of the sanitised build `PATH`. Repeatable.
    ///
    /// `--local` only, and only for the setup itself: builds sanitise `$HOME`
    /// and `/usr/local` off `PATH` so a local install cannot shadow the
    /// Gentoo toolchain, which also hides a hand-installed GNU sed/grep from
    /// the very first merges. `em setup --local` finds the usual locations by
    /// itself; this is for anywhere else you keep them.
    #[usage(long, value_name = "DIR")]
    pub extra_path: Vec<camino::Utf8PathBuf>,

    #[usage(flatten)]
    pub root_arg: RootArg,

    #[usage(flatten)]
    pub depgraph_flags: DepgraphFlags,

    #[usage(flatten)]
    pub merge_flags: MergeFlags,

    #[usage(flatten)]
    pub activity: ActivityArgs,

    /// Privilege backend for this setup run
    #[usage(long, value_enum, default = "auto")]
    pub privilege: Privilege,
}

/// `em crossdev` — cross-target setup, mirroring crossdev's option surface (the
/// no-build subset for now; building the toolchain is future work).
#[derive(usage::Args, Debug)]
pub struct CrossdevArgs {
    /// Deliberately no [`RootArg`]: none of `crossdev`'s three actions
    /// (`--init-target`/`--setup`/`--show-target-cfg`) read `--root` — it is a
    /// parse error after the applet, and a try_into reject in prefix position.

    /// Use the LLVM/Clang model (`cross_llvm-*`: host clang cross-targets, no per-target
    /// compiler)
    ///
    /// Rejects glibc — use musl or a bare-metal target.
    #[usage(short = 'L', long)]
    pub llvm: bool,

    /// Lay down the overlay + sysroot config without building anything
    #[usage(long)]
    pub init_target: bool,

    /// Bootstrap the cross toolchain into the prefix (`/usr/<tuple>`): the full
    /// intertwined sequence (binutils → headers → gcc-stage1 → libc →
    /// gcc-stage2). Implies `--init-target`.
    #[usage(long)]
    pub setup: bool,

    /// Print the derived target configuration and exit (no writes)
    #[usage(long)]
    pub show_target_cfg: bool,

    /// Build an extra package onto the established cross target (may be given multiple times)
    ///
    /// `CATEGORY/PN` — always runs on the host (like `binutils`/`gcc`), not the target sysroot,
    /// matching real crossdev's `--ex-pkg`.
    ///
    /// Applies to `--init-target`/`--setup` only; named per invocation,
    /// like real crossdev — not remembered across a later run that omits it.
    #[usage(long, value_name = "CATEGORY/PN")]
    pub ex_pkg: Vec<String>,

    /// Build a cross gdb (`dev-debug/gdb`) — shorthand for `--ex-pkg
    /// dev-debug/gdb`, crossdev's own `--ex-gdb`.
    #[usage(long)]
    pub ex_gdb: bool,

    #[usage(flatten)]
    pub depgraph_flags: DepgraphFlags,

    #[usage(flatten)]
    pub merge_flags: MergeFlags,

    #[usage(flatten)]
    pub activity: ActivityArgs,

    /// Privilege backend for this crossdev run
    #[usage(long, value_enum, default = "auto")]
    pub privilege: Privilege,
}

/// `em toolchain` — bootstrap a self-hosting native toolchain into `--root`
///
/// The native twin of `crossdev --setup` (`CHOST == CBUILD`): the staged
/// `baselayout → binutils → os-headers → glibc → gcc` bootstrap that produces a
/// working compiler + libc in a fresh ROOT. This is the *toolchain* primitive —
/// the compiler the `em stages` production (stage1 `packages.build`, stage3
/// `--emptytree @system`) then builds against. Kept separate from the stages on
/// purpose (catalyst/crossdev-stages do the same: toolchain, then the stages).
#[derive(usage::Args, Debug, Clone)]
pub struct ToolchainArgs {
    /// Build and install the toolchain into `--root` (the only action for now;
    /// required, mirroring `crossdev --setup`).
    #[usage(long)]
    pub setup: bool,

    #[usage(flatten)]
    pub root_arg: RootArg,

    #[usage(flatten)]
    pub depgraph_flags: DepgraphFlags,

    #[usage(flatten)]
    pub merge_flags: MergeFlags,

    #[usage(flatten)]
    pub activity: ActivityArgs,

    /// Privilege backend for this toolchain run
    #[usage(long, value_enum, default = "auto")]
    pub privilege: Privilege,
}

// `em stages` — assemble stage-build artifacts (stage1/stage3/stage4) *using*
// a toolchain already built by `em toolchain --setup`.
#[derive(usage::Args, Debug, Clone)]
pub struct StagesArgs {
    /// Emerge the profile's `packages.build` bootstrap set into `--root`:
    /// baselayout (USE=build, --nodeps) then the minimal stage1 package list
    /// (USE="-* build"), mirroring catalyst's `stage1/chroot.sh`. Requires a
    /// working toolchain already in the root (`em toolchain --setup`).
    #[usage(long)]
    pub stage1: bool,

    /// Emptytree rebuild of `@system` into `--root` (catalyst `stage3/chroot.sh`:
    /// `emerge -e --update --deep --with-bdeps=y @system`). Forces `-e -uD
    /// --with-bdeps` on top of other merge flags; seeds PKGDIR with `-b` like
    /// stage1. No stage2 (crossdev model). Requires a usable root (typically
    /// after `--stage1` or an unpacked seed).
    #[usage(long)]
    pub stage3: bool,

    #[usage(flatten)]
    pub root_arg: RootArg,

    #[usage(flatten)]
    pub depgraph_flags: DepgraphFlags,

    #[usage(flatten)]
    pub merge_flags: MergeFlags,

    #[usage(flatten)]
    pub activity: ActivityArgs,

    /// Privilege backend for this stages run
    #[usage(long, value_enum, default = "auto")]
    pub privilege: Privilege,
}

/// `em emerge` — resolve and merge/unmerge packages (real emerge workalike)
///
/// The explicit, self-contained form of the bare `em <atoms>` path — see
/// [`Applet::Emerge`]'s doc comment.
#[derive(usage::Args, Debug, Clone, Default)]
pub struct EmergeArgs {
    #[usage(flatten)]
    pub root_arg: RootArg,

    #[usage(flatten)]
    pub mode: EmergeModeArgs,

    #[usage(flatten)]
    pub merge_flags: MergeFlags,

    #[usage(flatten)]
    pub depgraph_flags: DepgraphFlags,

    #[usage(flatten)]
    pub activity: ActivityArgs,

    /// Privilege backend for this merge
    #[usage(long, value_enum, default = "auto")]
    pub privilege: Privilege,

    /// Atoms, package sets (`@world`), or ebuild paths to act on
    #[usage(value_name = "ATOM")]
    pub atoms: Vec<String>,
}

#[derive(usage::Subcommands, Debug)]
pub enum MaintCommand {
    #[usage(help = "Generate binary package metadata index")]
    Binhost,
    #[usage(help = "Inspect/verify/prune local binary packages (em-only, no emaint equivalent)")]
    Binpkg {
        #[usage(subcommand)]
        action: BinpkgAction,
    },
    #[usage(help = "No-op: em keeps no config-memory file to go stale")]
    Cleanconfmem,
    #[usage(help = "Discard saved resume lists")]
    Cleanresume {
        /// Actually delete the saved resume/resume-backup lists (default:
        /// just report what's there).
        #[usage(short, long)]
        fix: bool,
    },
    #[usage(help = "Prune the build.log files finished merges leave in the build tree")]
    Logs {
        /// Remove them; without this the logs are only listed
        #[usage(long)]
        fix: bool,
        /// Only consider logs at least this old (e.g. `30d`, `2weeks`)
        #[usage(short = 't', long, value_name = "AGE")]
        older_than: Option<String>,
    },
    #[usage(help = "Unavailable: em keeps no failed-merge registry")]
    Merges,
    #[usage(help = "Apply package moves to binary packages")]
    Movebin,
    #[usage(help = "Apply package moves to installed packages")]
    Moveinst,
    #[usage(help = "Regenerate profiles/use.local.desc from metadata.xml")]
    RegenUse {
        /// Write output here instead of profiles/use.local.desc ('-' for stdout)
        #[usage(short, long, value_name = "PATH")]
        output: Option<String>,
    },
    #[usage(help = "Purge repo revision history from repo_revisions")]
    Revisions {
        /// Purge only these repos (default: all)
        #[usage(value_name = "REPO")]
        repos: Vec<String>,
    },
    /// Same as `em sync` — shared implementation
    #[usage(help = "Sync repositories (git, rsync)")]
    Sync {
        /// Repo names from repos.conf (default: auto-sync enabled repos)
        repos: Vec<String>,
    },
    #[usage(help = "Check (and optionally fix) problems in the world file")]
    World {
        /// Remove orphaned entries from the world file
        #[usage(short, long)]
        fix: bool,
    },
}

/// Inspect, verify and prune the binary packages in the local `PKGDIR`
//
// the doc comment above is this subcommand's help, so the
// rationale stays a plain comment: there is no real-portage `emaint` module
// for this (only `emaint binhost`, which just regenerates the index) — it is
// an em-only extension, built on the `Packages` index/reader substrate.
#[derive(usage::Subcommands, Debug)]
pub enum BinpkgAction {
    #[usage(help = "Check each indexed binpkg's size/MD5/SHA1 against the file on disk")]
    Verify {
        /// Quarantine corrupt containers (rename to `.corrupt`) and drop
        /// missing/corrupt entries from the index by regenerating it.
        #[usage(long)]
        fix: bool,
        /// Reject a container with no OpenPGP signature at all (matches
        /// FEATURES=binpkg-request-signature); with a verify keyring
        /// present (`em maint binpkg gpg-import`), signatures are always
        /// cryptographically checked regardless of this flag.
        #[usage(long)]
        require_signature: bool,
    },
    #[usage(help = "List indexed binary packages (cpv, build-id, size, path)")]
    List,
    #[usage(help = "Keep only the newest BUILD_ID per package, deleting older ones")]
    Prune {
        /// Report what would be deleted without deleting or reindexing
        #[usage(long)]
        dry_run: bool,
    },
    #[usage(help = "Print the build-env key for the current roots' make.conf flags")]
    Fingerprint {
        /// Print the full key (space-joined sokgi hashes) instead of the
        /// short path-safe slug.
        #[usage(long)]
        full: bool,
        /// Fingerprint the host (BROOT) config instead of the target roots
        /// (only differs under --target).
        #[usage(long)]
        host: bool,
    },
    #[usage(help = "Import an armored OpenPGP public key into the GPG verify keyring")]
    GpgImport {
        /// Path to an armored public-key file (e.g. exported via
        /// `gpg --armor --export <key-id>`).
        keyfile: camino::Utf8PathBuf,
    },
}

/// `em select <module>` — native, eselect-like config selectors
#[derive(usage::Subcommands, Debug)]
pub enum SelectCommand {
    #[usage(help = "Select the system/sysroot profile (cross-aware)")]
    Profile {
        #[usage(subcommand)]
        action: ProfileAction,
    },
    #[usage(alias = "repos", help = "Manage local repositories (overlays)")]
    Repository {
        #[usage(subcommand)]
        action: RepositoryAction,
    },
    #[usage(
        alias = "gcc",
        help = "Select the active compiler profile (gcc-config/eselect gcc workalike)"
    )]
    Compiler {
        #[usage(subcommand)]
        action: CompilerAction,
    },
    #[usage(
        help = "Select the active binutils profile (binutils-config/eselect binutils workalike)"
    )]
    Binutils {
        #[usage(subcommand)]
        action: BinutilsAction,
    },
    #[usage(help = "Select the active linker profile")]
    Linker {
        #[usage(subcommand)]
        action: LinkerAction,
    },
    #[usage(help = "Select the active LLVM/clang slot")]
    Clang {
        #[usage(subcommand)]
        action: ClangAction,
    },
    #[usage(help = "Select the pkg-config backend and create the <CTARGET>-pkg-config wrapper")]
    Pkgconf {
        #[usage(subcommand)]
        action: PkgconfAction,
    },
    #[usage(
        alias = "mirror",
        help = "Manage Gentoo distfile mirrors (mirrorselect workalike)"
    )]
    Mirrors {
        #[usage(subcommand)]
        action: MirrorAction,
    },
    #[usage(help = "Read/manage GLEP 42 news items (eselect news workalike)")]
    News {
        #[usage(subcommand)]
        command: Option<NewsCommand>,
    },
    #[usage(help = "Check/fix Gentoo Linux Security Advisories (glsa-check workalike)")]
    Glsa {
        #[usage(subcommand)]
        command: Option<GlsaCommand>,
    },
}

/// `em select profile <action>`
#[derive(usage::Subcommands, Debug)]
pub enum ProfileAction {
    #[usage(help = "List available profiles (marks the current one)")]
    List,
    #[usage(help = "Show the current profile")]
    Show,
    #[usage(help = "Set the profile by list number or path (cross-aware: no arch check)")]
    Set {
        /// Profile list number (from `list`) or path (e.g. `default/linux/riscv/23.0/rv64/lp64d`)
        target: String,
    },
}

/// `em select repository <action>` — local repos only (remote sync is a TODO)
#[derive(usage::Subcommands, Debug)]
pub enum RepositoryAction {
    #[usage(help = "List configured repositories")]
    List,
    #[usage(help = "Register an existing local repository")]
    Add {
        /// Repository name
        name: String,
        /// Existing local path to the repository
        location: String,
    },
    #[usage(alias = "rm", help = "Remove a repository's repos.conf entry")]
    Remove {
        /// Repository name
        name: String,
    },
    #[usage(help = "Create a new local overlay (skeleton + repos.conf entry)")]
    Create {
        /// Repository name
        name: String,
        /// Location (default: `<config-root>/var/db/repos/<name>`)
        location: Option<String>,
    },
}

/// `em select compiler <action>` — gcc-config workalike
#[derive(usage::Subcommands, Debug)]
pub enum CompilerAction {
    #[usage(help = "List available compiler profiles")]
    List {
        /// Target tuple (CTARGET) to list profiles for
        #[usage(short, long)]
        target: Option<String>,
    },
    #[usage(help = "Show the current compiler profile")]
    Show {
        /// Target tuple (CTARGET) to show profile for
        #[usage(short, long)]
        target: Option<String>,
    },
    #[usage(help = "Set the active compiler profile")]
    Set {
        /// Compiler profile to activate (e.g., `riscv64-unknown-linux-gnu-16` or `1` for list number)
        profile: String,
        /// Target tuple (CTARGET) for cross-compiler selection
        #[usage(short, long)]
        target: Option<String>,
    },
}

/// `em select binutils <action>` — binutils-config workalike
#[derive(usage::Subcommands, Debug)]
pub enum BinutilsAction {
    #[usage(help = "List available binutils profiles")]
    List {
        /// Target tuple (CTARGET) to list profiles for
        #[usage(short, long)]
        target: Option<String>,
    },
    #[usage(help = "Show the current binutils profile")]
    Show {
        /// Target tuple (CTARGET) to show profile for
        #[usage(short, long)]
        target: Option<String>,
    },
    #[usage(help = "Set the active binutils profile")]
    Set {
        /// Binutils profile to activate (e.g., `riscv64-unknown-linux-gnu-2.46.0` or `1` for list number)
        profile: String,
        /// Target tuple (CTARGET) for cross-binutils selection
        #[usage(short, long)]
        target: Option<String>,
    },
}

/// `em select linker <action>` — linker profile selection
#[derive(usage::Subcommands, Debug)]
pub enum LinkerAction {
    #[usage(help = "List available linker profiles")]
    List {
        /// Target tuple (CTARGET) to list profiles for
        #[usage(short, long)]
        target: Option<String>,
    },
    #[usage(help = "Show the current linker profile")]
    Show {
        /// Target tuple (CTARGET) to show profile for
        #[usage(short, long)]
        target: Option<String>,
    },
    #[usage(help = "Set the active linker profile")]
    Set {
        /// Linker profile to activate (e.g., `riscv64-unknown-linux-gnu-lld-18` or `1` for list number)
        profile: String,
        /// Target tuple (CTARGET) for cross-linker selection
        #[usage(short, long)]
        target: Option<String>,
    },
}

/// `em select clang <action>` — LLVM/clang slot selection
#[derive(usage::Subcommands, Debug)]
pub enum ClangAction {
    #[usage(help = "List available LLVM/clang slots")]
    List,
    #[usage(help = "Show the current LLVM/clang slot")]
    Show,
    #[usage(help = "Set the active LLVM/clang slot")]
    Set {
        /// LLVM slot to activate: a slot (`22`), a slot qualified by where it
        /// lives (`22@host`, `22@prefix`), or a `list` number. A bare slot present
        /// in both resolves to the prefix's..
        slot: String,
    },
}

/// `em select pkgconf <action>` — picks the `pkg-config`/`pkgconf` backend
/// and creates the `<CTARGET>-pkg-config` wrapper real crossdev provides but
/// `em` otherwise never builds (`toolchain-funcs.eclass`'s `tc-getPKG_CONFIG`
/// searches `$PATH` for exactly this name).
#[derive(usage::Subcommands, Debug)]
pub enum PkgconfAction {
    #[usage(help = "List available pkg-config backends (pkgconf, pkg-config)")]
    List {
        /// Target tuple (CTARGET) to show the wrapper for
        #[usage(short, long)]
        target: Option<String>,
    },
    #[usage(help = "Show the backend the <target>-pkg-config wrapper currently points at")]
    Show {
        /// Target tuple (CTARGET) to show the wrapper for
        #[usage(short, long)]
        target: Option<String>,
    },
    #[usage(help = "Create/update the <target>-pkg-config wrapper")]
    Set {
        /// Backend to wrap (`pkgconf`, `pkg-config`, or a list number from `list`)
        backend: String,
        /// Target tuple (CTARGET) to create the wrapper for
        #[usage(short, long)]
        target: Option<String>,
    },
}

/// `em select mirrors <action>` — mirrorselect workalike for `GENTOO_MIRRORS`
#[derive(usage::Subcommands, Debug)]
pub enum MirrorAction {
    /// List available Gentoo distfile mirrors (marks those already selected)
    List {
        /// Keep only mirrors in this ISO country code (e.g. `US`, `DE`)
        #[usage(short, long)]
        country: Option<String>,
        /// Keep only mirrors in this region (e.g. `Europe`, `North America`)
        #[usage(short, long)]
        region: Option<String>,
    },
    /// Show the currently configured `GENTOO_MIRRORS` value
    Show,
    /// Set `GENTOO_MIRRORS`
    Set {
        /// Explicit mirror URLs to use
        ///
        /// If omitted, mirrors are picked from `--country`/`--region` instead.
        #[usage(value_name = "URL")]
        urls: Vec<String>,
        /// Use every mirror in this ISO country code
        #[usage(short, long)]
        country: Option<String>,
        /// Use every mirror in this region
        #[usage(short, long)]
        region: Option<String>,
    },
}

#[derive(usage::Subcommands, Debug)]
pub enum PkgCommand {
    #[usage(help = "Edit per-package USE flags in package.use")]
    Use {
        /// Package atom (e.g. sys-boot/grub or >=dev-libs/foo-1.0)
        atom: String,
        /// Add flags (written verbatim, e.g. truetype) — euse calls this
        /// --enable/-E
        #[usage(short = 'E', short = 'a', long = "add", value_name = "FLAG")]
        add: Vec<String>,
        /// Subtract flags (written with leading '-', e.g. -themes) — euse
        /// calls this --disable/-D
        #[usage(short = 'D', short = 's', long = "subtract", value_name = "FLAG")]
        subtract: Vec<String>,
        /// Drop flags entirely (removes both flag and -flag forms) — euse
        /// calls this --remove/-R or --prune/-P
        #[usage(
            short = 'R',
            short = 'P',
            short = 'd',
            long = "drop",
            value_name = "FLAG"
        )]
        drop: Vec<String>,
        /// Preview the resulting entry without writing package.use
        #[usage(short = 'n', long = "dry-run")]
        dry_run: bool,
        /// Show descriptions for the given USE flags on this package
        /// (metadata.xml/use.local.desc first, falling back to the global
        /// profiles/use.desc)
        #[usage(
            short = 'i',
            long = "info",
            value_name = "FLAG",
            conflicts("add", "subtract", "drop")
        )]
        info: Vec<String>,
        /// Target file inside package.use/ (default: `<cat>-<pkg>`)
        #[usage(long, value_name = "FILE")]
        path: Option<camino::Utf8PathBuf>,
    },
    #[usage(help = "Edit per-package keywords in package.accept_keywords")]
    Keyword {
        /// Package atom (e.g. sys-boot/grub or >=dev-libs/foo-1.0)
        atom: String,
        /// Add keyword tokens (e.g. `~amd64`, `-*`)
        #[usage(short = 'a', long = "add", value_name = "KW")]
        add: Vec<String>,
        /// Subtract keyword tokens (written with leading '-', e.g. `-~amd64`)
        #[usage(short = 's', long = "subtract", value_name = "KW")]
        subtract: Vec<String>,
        /// Drop keyword tokens entirely (removes both the token and its negated form)
        #[usage(short = 'd', long = "drop", value_name = "KW")]
        drop: Vec<String>,
        /// Target file inside package.accept_keywords/ (default: `<cat>-<pkg>`)
        #[usage(long, value_name = "FILE")]
        path: Option<camino::Utf8PathBuf>,
    },
    #[usage(help = "Add/remove a package from package.mask")]
    Mask {
        /// Package atom (e.g. sys-boot/grub or >=dev-libs/foo-1.0)
        atom: String,
        /// Add the atom to package.mask
        #[usage(short = 'a', long = "add")]
        add: bool,
        /// Remove the atom from package.mask
        #[usage(short = 'd', long = "drop")]
        drop: bool,
        /// Target file inside package.mask/ (default: `<cat>-<pkg>`)
        #[usage(long, value_name = "FILE")]
        path: Option<camino::Utf8PathBuf>,
    },
    #[usage(help = "Edit per-package env files in package.env")]
    Env {
        /// Package atom (e.g. sys-boot/grub or >=dev-libs/foo-1.0)
        atom: String,
        /// Add env file name(s) (from `/etc/portage/env/`) to apply to this package
        #[usage(short = 'a', long = "add", value_name = "ENVFILE")]
        add: Vec<String>,
        /// Drop env file name(s) from this package's entry
        #[usage(short = 'd', long = "drop", value_name = "ENVFILE")]
        drop: Vec<String>,
        /// Target file inside package.env/ (default: `<cat>-<pkg>`)
        #[usage(long, value_name = "FILE")]
        path: Option<camino::Utf8PathBuf>,
    },
}

#[derive(usage::Subcommands, Debug)]
pub enum QueryCommand {
    #[usage(help = "Find which package owns a file", alias_hidden = "b")]
    Belongs {
        /// File path(s) to look up in the VDB contents records
        #[usage(required)]
        file: Vec<String>,
    },
    #[usage(help = "Verify checksums of installed package", alias_hidden = "k")]
    Check {
        /// Installed package atom(s) to verify
        #[usage(required)]
        atom: Vec<String>,
    },
    #[usage(help = "List packages depending on an atom", alias_hidden = "d")]
    Depends {
        /// Atom(s) whose dependents to list
        #[usage(required)]
        atom: Vec<String>,
    },
    #[usage(help = "Display full dependency tree", alias_hidden = "g")]
    Depgraph {
        /// Atom(s) to resolve and display the dependency tree for
        #[usage(required)]
        atom: Vec<String>,
        /// Output format
        #[usage(long, short, value_enum, default = "pretty")]
        format: DepgraphFormat,
        /// Let the solver choose USE flags to satisfy REQUIRED_USE (Level C)
        #[usage(long)]
        autosolve_use: bool,
        #[usage(flatten)]
        depgraph_flags: DepgraphFlags,
        /// Treat every atom as not-yet-installed (emerge's `-e`/`--emptytree`)
        #[usage(short = 'e', long)]
        emptytree: bool,
        /// Only show dependencies, excluding the given atoms themselves from the tree
        #[usage(short = 'o', long)]
        onlydeps: bool,
        /// Include build-time dependencies (BDEPEND) in the resolution
        #[usage(long)]
        with_bdeps: bool,
        /// emerge's `--root-deps[=rdeps]`: only require RDEPEND (not DEPEND)
        /// to be satisfiable in the merge target.
        #[usage(long = "root-deps")]
        root_deps: bool,
    },
    #[usage(help = "List files installed by a package", alias_hidden = "f")]
    Files {
        /// Atom(s) whose installed file list to show
        #[usage(required)]
        atom: Vec<String>,
    },
    #[usage(
        help = "List installed packages by a VDB field value",
        alias_hidden = "a"
    )]
    Has {
        /// VDB field to match, e.g. `SLOT`, `USE`, `repository`
        field: String,
        /// Value the field must contain; omit to list every package whose
        /// field is set at all
        value: Option<String>,
    },
    #[usage(
        help = "List packages with a given USE flag in IUSE",
        alias_hidden = "h"
    )]
    Hasuse {
        /// USE flag name(s) to search for in IUSE
        #[usage(required)]
        flag: Vec<String>,
    },
    #[usage(
        help = "Display keyword status across architectures",
        alias_hidden = "y"
    )]
    Keywords {
        /// Atom(s) to show keyword status for
        #[usage(required)]
        atom: Vec<String>,
    },
    #[usage(help = "List installed/available packages matching a pattern")]
    List {
        /// List only installed packages (from VDB), not available ones
        #[usage(short = 'I', long = "installed")]
        installed: bool,
        /// Glob or substring pattern(s); omit to list all packages
        pattern: Vec<String>,
    },
    #[usage(
        help = "Display package metadata (maintainer, homepage, etc.)",
        alias_hidden = "m"
    )]
    Meta {
        /// Atom(s) whose metadata to display
        #[usage(required)]
        atom: Vec<String>,
    },
    #[usage(help = "Display total file size of a package", alias_hidden = "s")]
    Size {
        /// Atom(s) whose installed file size to sum
        #[usage(required)]
        atom: Vec<String>,
    },
    #[usage(help = "Display USE flags for a package", alias_hidden = "u")]
    Uses {
        /// Atom(s) whose USE flags to display
        #[usage(required)]
        atom: Vec<String>,
    },
    #[usage(
        help = "Print full path to the ebuild for a package",
        alias_hidden = "w"
    )]
    Which {
        /// Atom(s) to resolve to an ebuild path
        #[usage(required)]
        atom: Vec<String>,
    },
}

/// `em etc <command>`
#[derive(usage::Subcommands, Debug)]
pub enum EtcCommand {
    #[usage(help = "Show what each pending file would change")]
    Diff {
        /// Only files whose path contains this text
        path: Option<String>,
    },
    #[usage(help = "Resolve each pending file interactively")]
    Merge,
}

/// Batch resolutions for `em etc`
///
/// Mutually exclusive with each other; without any of them `em etc` lists.
#[derive(usage::Args, Clone, Debug, Default)]
pub struct EtcOpts {
    /// Install every pending file over its target
    #[usage(long, conflicts("use_old", "auto"))]
    pub use_new: bool,
    /// Discard every pending file, keeping what is installed
    #[usage(long, conflicts("use_new", "auto"))]
    pub use_old: bool,
    /// Resolve only what needs no decision: identical files, and those
    /// differing from the installed one in comments or whitespace alone
    #[usage(long, conflicts("use_new", "use_old"))]
    pub auto: bool,
}

#[derive(usage::Subcommands, Debug)]
pub enum CleanTarget {
    #[usage(
        help = "Remove distfiles no ebuild references",
        alias_hidden = "distfiles",
        alias_hidden = "d"
    )]
    Dist {
        #[usage(flatten)]
        opts: CleanOpts,
    },
    #[usage(
        help = "Remove binary packages no ebuild references",
        alias_hidden = "packages",
        alias_hidden = "p"
    )]
    Pkg {
        #[usage(flatten)]
        opts: CleanOpts,
    },
    #[usage(help = "Everything above, plus the build logs finished merges leave behind")]
    All {
        #[usage(flatten)]
        opts: CleanOpts,
    },
}

/// Filters shared by both clean targets
///
/// Deliberately narrower than `eclean`'s: the destructive/interactive modes it
/// grew are covered here by the global `-p` plus `--deep`, and everything else
/// it offers is a filter on the same candidate set.
#[derive(usage::Args, Clone, Debug, Default)]
pub struct CleanOpts {
    /// Keep only what installed packages still reference, rather than
    /// everything any ebuild in the tree references
    #[usage(short = 'd', long)]
    pub deep: bool,
    /// Skip files smaller than this (e.g. `10M`, `1G`) — clears the big wins
    /// without touching a long tail of small files
    #[usage(short = 's', long, value_name = "SIZE")]
    pub size_limit: Option<String>,
    /// Keep files modified more recently than this (e.g. `2weeks`, `30d`)
    #[usage(short = 't', long, value_name = "AGE")]
    pub time_limit: Option<String>,
}

#[derive(usage::Subcommands, Debug)]
pub enum NewsCommand {
    #[usage(help = "Count unread news items")]
    Count,
    #[usage(help = "List news items")]
    List,
    #[usage(
        help = "Read news items (numbers/names from `list`; \"new\"/\"all\", or none for all unread)"
    )]
    Read {
        /// Item numbers/names from `list`, the single keyword "new" (every
        /// unread item) or "all" (every item), or omit for "new".
        ids: Vec<String>,
    },
    #[usage(help = "Purge read news items")]
    Purge,
}

#[derive(usage::Subcommands, Debug)]
pub enum GlsaCommand {
    #[usage(help = "List all GLSAs")]
    List,
    #[usage(help = "Check for affected GLSAs")]
    Check {
        /// GLSA id(s) to check (default: every GLSA in the repo)
        ids: Vec<String>,
    },
    #[usage(help = "Apply a GLSA fix")]
    Fix {
        /// GLSA id(s) to fix (default: every affected GLSA)
        ids: Vec<String>,
    },
}

/// `em active <subcommand>` — persistent default `--prefix` / `--local`
///
/// `set` reads the global `--prefix` / `--local` flags (same shape as
/// `em --prefix DIR setup`), so there is no second set of flag names to
/// collide with the globals.
///
/// Entries can be referenced by name, index (0-based), or exact path.
#[derive(usage::Subcommands, Debug)]
pub enum ActiveCommand {
    /// Show the registered active context (default when no subcommand)
    #[usage(help = "Show the registered active prefix/local")]
    Show,
    /// Register the invocation's `--prefix` or `--local` as the active context
    ///
    /// Without arguments, reads from `--prefix`/`--local` flags:
    ///   `em --prefix /home/me/prefix active set`
    ///   `em --local= active set`           (default `~/.gentoo`)
    ///   `em --local /other active set`
    ///
    /// With a reference argument, activates an existing entry:
    ///   `em active set my-name`     # by name
    ///   `em active set 0`           # by index
    ///   `em active set /path/to/dir` # by exact path
    ///
    /// Note: `em --local active set` is wrong — `--local` takes `active` as the
    /// `--local` path. Use `em --local=` or pass an explicit directory.
    #[usage(help = "Register --prefix/--local as active or activate an existing entry")]
    Set {
        /// Reference to an existing entry (name, index, or path) to activate
        ///
        /// If not provided, creates a new entry from --prefix/--local flags.
        #[usage(value_name = "REF")]
        reference: Option<String>,
    },
    /// Clear the registered active context
    ///
    /// Use `--all` to remove all entries, not just the active pointer.
    #[usage(help = "Clear the active context (or all entries with --all)")]
    Clear {
        /// Clear all entries, not just the active pointer
        #[usage(long)]
        all: bool,
    },
    /// Print shell exports for `eval "$(em active env)"` (PATH + markers)
    #[usage(help = "Print shell exports for the active context")]
    Env,
    /// List all registered entries
    #[usage(help = "List all registered prefix/local entries")]
    List,
    /// Add a new entry without activating it
    ///
    /// Examples:
    ///   `em --prefix /home/me/prefix active add my-prefix`
    ///   `em --local /home/me/.gentoo active add my-gentoo`
    ///   `em --local= active add`  # adds ~/.gentoo with auto-generated name
    #[usage(help = "Add a new prefix/local entry")]
    Add {
        /// Optional name for the entry. If not provided, uses path basename
        #[usage(value_name = "NAME")]
        name: Option<String>,
    },
    /// Remove an entry by name, index, or path
    ///
    /// Examples:
    ///   `em active remove my-name`
    ///   `em active remove 0`           # by index
    ///   `em active remove /path/to/dir` # by exact path
    #[usage(help = "Remove a registered entry")]
    Remove {
        /// Reference to the entry to remove (name, index, or path)
        #[usage(value_name = "REF")]
        reference: String,
    },
}

#[derive(usage::Subcommands, Debug)]
pub enum LogCommand {
    #[usage(help = "Show currently running merges")]
    Current,
    #[usage(help = "Show recent merge history from activity JSONL")]
    List {
        /// Max rows (default 20)
        limit: Option<u32>,
    },
    #[usage(help = "Show merge times for a package (or global median)")]
    Time {
        /// Package name/atom substring to filter by; omit for the global median
        atom: Option<String>,
    },
    #[usage(help = "ETA for remainder of a live activity session")]
    Predict,
}

/// How an unprivileged build gets root for `chown`/setuid (see `--privilege`)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, usage::ValueEnum)]
pub enum Privilege {
    /// Best compiled-in fake root (pseudoroot, else fakeroost, else none) when
    /// unprivileged, real chowns when already root (default).
    #[default]
    Auto,
    /// Pure-Rust ptrace+seccomp fake root; ownership faked in-session
    #[cfg(all(feature = "fakeroost", target_os = "linux"))]
    Fakeroost,
    /// LD_PRELOAD fake root (`pseudoroot`); ownership faked in-session, no ptrace tax
    #[cfg(all(feature = "pseudoroot", any(target_os = "linux", target_os = "macos")))]
    Pseudoroot,
    /// User-namespace sandbox with build-user→0 map; real chowns in-box
    #[cfg(all(feature = "hakoniwa", target_os = "linux"))]
    Hakoniwa,
    /// Re-exec under `sudo` for real root (root-owned tree, real setuid)
    Sudo,
    /// No wrapping; run unprivileged (chowns best-effort, may not stick)
    None,
}

/// Output format for `em query depgraph`
#[derive(Clone, Copy, Debug, usage::ValueEnum)]
pub enum DepgraphFormat {
    /// emerge -p style pretend output
    Pretty,
    /// Machine-parsable JSON
    Json,
    /// cargo tree style dependency tree
    Tree,
}
