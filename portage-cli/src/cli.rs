#[cfg(test)]
use std::ffi::OsStr;
use std::ffi::OsString;

use gentoo_core::Arch;
#[cfg(test)]
use portage_atom_pubgrub::DepClass;
use portage_resolve::Roots;
use winnow_args::{Args as _, FromArg};

mod activity;
mod context;
mod depgraph_flags;
mod emerge_mode;
mod merge_flags;
mod topology;
pub use activity::ActivityArgs;
pub use context::{
    ArchArg, FormatArg, OutputFormat, PretendArg, QuietArg, RepoArg, VdbArg, VerboseArg,
};
pub use depgraph_flags::DepgraphFlags;
pub use emerge_mode::EmergeModeArgs;
pub use merge_flags::{MergeFlags, YesNo};
pub use topology::{RootArg, RootTopology, RootedTopology, Topology};

fn default_arch() -> Arch {
    Arch::current()
}

/// When to colour terminal output (`--color`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, winnow_args::ValueEnum)]
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

/// Parsed `em` invocation after validation.
pub struct Validated(pub Cli);

/// What validation refuses, reported as a parse failure is.
pub type ValidationError = winnow_args::Error;

/// `value`, given for `field` (a flag or an environment variable), is refused for `reason`.
fn invalid(field: &str, value: &str, reason: impl Into<String>) -> ValidationError {
    winnow_args::Error::invalid_value(0, field, value.as_bytes(), reason.into())
}

impl TryFrom<Cli> for Validated {
    type Error = ValidationError;

    fn try_from(cli: Cli) -> Result<Self, Self::Error> {
        validate(&cli)?;
        Ok(Validated(cli))
    }
}

fn validate(cli: &Cli) -> Result<(), ValidationError> {
    if let Some(root) = cli.root_topology.root.as_deref() {
        let name = match &cli.applet {
            Some(Applet::Crossdev(_)) => Some("crossdev"),
            Some(Applet::Active(_)) => Some("active"),
            Some(Applet::Worker(_)) => Some("__worker"),
            _ => None,
        };
        if let Some(name) = name {
            return Err(invalid(
                "--root",
                root,
                format!("not valid with the '{name}' applet"),
            ));
        }
    }
    if applet_privilege(cli) == Privilege::Auto {
        privilege_from_env()?;
    }
    if !applet_activity(cli).emergelog {
        env_bool("EM_EMERGELOG")?;
    }
    Ok(())
}

fn privilege_from_env() -> Result<Option<Privilege>, ValidationError> {
    let Ok(raw) = std::env::var("EM_PRIVILEGE") else {
        return Ok(None);
    };
    match Privilege::from_arg(raw.to_ascii_lowercase().as_str().into()) {
        Ok(privilege) => Ok(Some(privilege)),
        Err(_) => Err(invalid(
            "EM_PRIVILEGE",
            &raw,
            format!("expected one of {}", Privilege::CHOICES.join(", ")),
        )),
    }
}

fn env_bool(name: &'static str) -> Result<Option<bool>, ValidationError> {
    let Ok(raw) = std::env::var(name) else {
        return Ok(None);
    };
    match raw.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" | "t" | "y" => Ok(Some(true)),
        "0" | "false" | "no" | "off" | "f" | "n" => Ok(Some(false)),
        _ => Err(invalid(
            name,
            &raw,
            "expected a boolean (1/true/yes or 0/false/no)",
        )),
    }
}

fn applet_privilege(cli: &Cli) -> Privilege {
    match &cli.applet {
        Some(Applet::Emerge(a)) => a.privilege,
        Some(Applet::Crossdev(a)) => a.privilege,
        Some(Applet::Toolchain(a)) => a.privilege,
        Some(Applet::Stages(a)) => a.privilege,
        Some(Applet::Setup(a)) => a.privilege,
        _ => Privilege::Auto,
    }
}

fn applet_activity(cli: &Cli) -> ActivityArgs {
    match &cli.applet {
        Some(Applet::Emerge(a)) => a.activity.clone(),
        Some(Applet::Regen(a)) => a.activity.clone(),
        Some(Applet::Crossdev(a)) => a.activity.clone(),
        Some(Applet::Toolchain(a)) => a.activity.clone(),
        Some(Applet::Stages(a)) => a.activity.clone(),
        Some(Applet::Setup(a)) => a.activity.clone(),
        _ => ActivityArgs::default(),
    }
}

/// Parse process argv into `Validated`. Exits on `--help`/`--version`/failure,
/// and answers a completion script's callback.
pub fn parse_cli_or_exit() -> Validated {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if let Some(answer) = Cli::completion_request(&args) {
        print!("{answer}");
        std::process::exit(0);
    }
    match Cli::parse_from(&args).and_then(Validated::try_from) {
        Ok(validated) => validated,
        Err(error) => std::process::exit(winnow_args::report(&error, "em")),
    }
}

/// A leading flag before any subcommand name routes into `emerge` (the
/// default command) — e.g. `em -ua @world` — and every later word is then an
/// emerge argument, even the literal word `emerge` (`em --json emerge -p pkg`
/// emerges a package named "emerge"). Write `em emerge --json -p pkg` rather
/// than mixing a leading flag with a later `emerge` word.
///
/// This also means `-p`/`--pretend` only reaches a *named* applet in prefix
/// position by coincidentally also being emerge's own flag: `em -p pkg`
/// previews an emerge, but `em -p toolchain` no longer selects `toolchain` —
/// write `em toolchain -p` instead.
#[derive(winnow_args::Args, Debug)]
#[arg(
    name = "em",
    version,
    about = "Gentoo Portage package manager workalike",
    long_about = "Gentoo Portage package manager workalike",
    arg_required_else_help,
    unknown_flags = "error",
    default_subcommand = "emerge",
    default_subcommand_flags,
    default_subcommand_help,
    example("em -ua @world", header = "Update @world"),
    example("em query belongs /usr/bin/python", header = "Find a file's owner"),
    example("em select profile list", header = "List profiles")
)]
pub struct Cli {
    #[arg(long, global, default = "auto", value_name = "WHEN")]
    pub color: ColorChoice,

    #[arg(flatten)]
    pub root_topology: RootTopology,

    #[arg(subcommand)]
    pub applet: Option<Applet>,
}

/// `applet.field.clone().or_else(|| cli.field.clone())`, for every field.
fn overlay_topology(applet: &Topology, cli: &RootTopology) -> Topology {
    Topology {
        prefix: applet.prefix.clone().or_else(|| cli.prefix.clone()),
        local: applet.local.clone().or_else(|| cli.local.clone()),
        config_root: applet
            .config_root
            .clone()
            .or_else(|| cli.config_root.clone()),
        target: applet.target.clone().or_else(|| cli.target.clone()),
    }
}

impl Cli {
    /// The dispatched applet's own topology group (`prefix`/`local`/
    /// `config_root`/`target`), regardless of whether it carries `--root`
    /// alongside it ([`RootedTopology`]) or not ([`Topology`], bare, on
    /// `Crossdev`/`Active`).
    fn applet_topology(&self) -> Option<Topology> {
        match &self.applet {
            Some(Applet::Emerge(a)) => Some(a.topology.split().0),
            Some(Applet::Toolchain(a)) => Some(a.topology.split().0),
            Some(Applet::Stages(a)) => Some(a.topology.split().0),
            Some(Applet::Setup(a)) => Some(a.topology.split().0),
            Some(Applet::Ebuild(a)) => Some(a.topology.split().0),
            Some(Applet::Maint(a)) => Some(a.topology.split().0),
            Some(Applet::Sync(a)) => Some(a.topology.split().0),
            Some(Applet::Depclean(a)) => Some(a.topology.split().0),
            Some(Applet::Regen(a)) => Some(a.topology.split().0),
            Some(Applet::Quickpkg(a)) => Some(a.topology.split().0),
            Some(Applet::MirrorDist(a)) => Some(a.topology.split().0),
            Some(Applet::Clean(a)) => Some(a.topology.split().0),
            Some(Applet::Etc(a)) => Some(a.topology.split().0),
            Some(Applet::Query(a)) => Some(a.topology.split().0),
            Some(Applet::Use(a)) => Some(a.topology.split().0),
            Some(Applet::Pkg(a)) => Some(a.topology.split().0),
            Some(Applet::Revdep(a)) => Some(a.topology.split().0),
            Some(Applet::Read(a)) => Some(a.topology.split().0),
            Some(Applet::Log(a)) => Some(a.topology.split().0),
            Some(Applet::Search(a)) => Some(a.topology.split().0),
            Some(Applet::Select(a)) => Some(a.topology.split().0),
            Some(Applet::Env(a)) => Some(a.topology.split().0),
            Some(Applet::Grep(a)) => Some(a.topology.split().0),
            Some(Applet::Crossdev(a)) => Some(a.topology.clone()),
            Some(Applet::Active(a)) => Some(a.topology.clone()),
            _ => None,
        }
    }

    /// The dispatched applet's own `--root`, if it carries one at all
    /// ([`RootedTopology`] applets only — `Crossdev`/`Active` have no root
    /// field to bind to, which is what keeps `--root` a hard parse error for
    /// them, not just a `validate()` rejection).
    fn applet_root(&self) -> Option<&str> {
        match &self.applet {
            Some(Applet::Emerge(a)) => a.topology.root.as_deref(),
            Some(Applet::Toolchain(a)) => a.topology.root.as_deref(),
            Some(Applet::Stages(a)) => a.topology.root.as_deref(),
            Some(Applet::Setup(a)) => a.topology.root.as_deref(),
            Some(Applet::Ebuild(a)) => a.topology.root.as_deref(),
            Some(Applet::Maint(a)) => a.topology.root.as_deref(),
            Some(Applet::Sync(a)) => a.topology.root.as_deref(),
            Some(Applet::Depclean(a)) => a.topology.root.as_deref(),
            Some(Applet::Regen(a)) => a.topology.root.as_deref(),
            Some(Applet::Quickpkg(a)) => a.topology.root.as_deref(),
            Some(Applet::MirrorDist(a)) => a.topology.root.as_deref(),
            Some(Applet::Clean(a)) => a.topology.root.as_deref(),
            Some(Applet::Etc(a)) => a.topology.root.as_deref(),
            Some(Applet::Query(a)) => a.topology.root.as_deref(),
            Some(Applet::Use(a)) => a.topology.root.as_deref(),
            Some(Applet::Pkg(a)) => a.topology.root.as_deref(),
            Some(Applet::Revdep(a)) => a.topology.root.as_deref(),
            Some(Applet::Read(a)) => a.topology.root.as_deref(),
            Some(Applet::Log(a)) => a.topology.root.as_deref(),
            Some(Applet::Search(a)) => a.topology.root.as_deref(),
            Some(Applet::Select(a)) => a.topology.root.as_deref(),
            Some(Applet::Env(a)) => a.topology.root.as_deref(),
            Some(Applet::Grep(a)) => a.topology.root.as_deref(),
            _ => None,
        }
    }

    pub(crate) fn topology_and_root(&self) -> (Topology, RootArg) {
        let topology = match self.applet_topology() {
            Some(t) => overlay_topology(&t, &self.root_topology),
            None => Topology {
                prefix: self.root_topology.prefix.clone(),
                local: self.root_topology.local.clone(),
                config_root: self.root_topology.config_root.clone(),
                target: self.root_topology.target.clone(),
            },
        };
        let root = RootArg {
            root: self
                .applet_root()
                .map(str::to_string)
                .or_else(|| self.root_topology.root.clone()),
        };
        (topology, root)
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
        self.topology_and_root()
            .0
            .target
            .map(|t| t.tuple().to_owned())
    }

    /// The spec file `--target` named, if it named one
    pub(crate) fn target_spec(&self) -> Option<crate::crossdev::spec::SysrootSpec> {
        self.topology_and_root().0.target?.spec().cloned()
    }

    /// The dispatched applet's own `--vdb` override, if any — a read-only
    /// query override, not part of topology resolution (see [`VdbArg`]).
    pub fn vdb(&self) -> Option<String> {
        match &self.applet {
            Some(Applet::Emerge(a)) => a.vdb_arg.vdb.clone(),
            Some(Applet::Maint(a)) => a.vdb_arg.vdb.clone(),
            Some(Applet::Depclean(a)) => a.vdb_arg.vdb.clone(),
            Some(Applet::Quickpkg(a)) => a.vdb_arg.vdb.clone(),
            Some(Applet::Query(a)) => a.vdb_arg.vdb.clone(),
            Some(Applet::Revdep(a)) => a.vdb_arg.vdb.clone(),
            Some(Applet::Select(a)) => a.vdb_arg.vdb.clone(),
            _ => None,
        }
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
        if let Some(p) = self.repo_flag() {
            return p;
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
        if let Some(p) = self.repo_flag() {
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

    /// The dispatched applet's own `-v`/`--verbose` count, or `0` for an
    /// applet that doesn't carry [`VerboseArg`].
    pub fn verbose(&self) -> u8 {
        match &self.applet {
            Some(Applet::Emerge(a)) => a.verbose_arg.verbose,
            Some(Applet::Crossdev(a)) => a.verbose_arg.verbose,
            Some(Applet::Toolchain(a)) => a.verbose_arg.verbose,
            Some(Applet::Stages(a)) => a.verbose_arg.verbose,
            Some(Applet::Setup(a)) => a.verbose_arg.verbose,
            Some(Applet::Maint(a)) => a.verbose_arg.verbose,
            Some(Applet::Sync(a)) => a.verbose_arg.verbose,
            Some(Applet::Regen(a)) => a.verbose_arg.verbose,
            Some(Applet::Query(a)) => a.verbose_arg.verbose,
            Some(Applet::Grep(a)) => a.verbose_arg.verbose,
            _ => 0,
        }
    }

    /// The dispatched applet's own `-q`/`--quiet`, or `false` for an applet
    /// that doesn't carry [`QuietArg`] (or its own field, for `__worker`).
    pub fn quiet(&self) -> bool {
        match &self.applet {
            Some(Applet::Worker(w)) => w.quiet,
            Some(Applet::Emerge(a)) => a.quiet_arg.quiet,
            Some(Applet::Crossdev(a)) => a.quiet_arg.quiet,
            Some(Applet::Toolchain(a)) => a.quiet_arg.quiet,
            Some(Applet::Stages(a)) => a.quiet_arg.quiet,
            Some(Applet::Setup(a)) => a.quiet_arg.quiet,
            Some(Applet::Maint(a)) => a.quiet_arg.quiet,
            Some(Applet::Sync(a)) => a.quiet_arg.quiet,
            Some(Applet::Etc(a)) => a.quiet_arg.quiet,
            Some(Applet::Regen(a)) => a.quiet_arg.quiet,
            _ => false,
        }
    }

    /// The dispatched applet's own `-p`/`--pretend`, or `false` for an
    /// applet that doesn't carry [`PretendArg`].
    pub fn pretend(&self) -> bool {
        match &self.applet {
            Some(Applet::Emerge(a)) => a.pretend_arg.pretend,
            Some(Applet::Crossdev(a)) => a.pretend_arg.pretend,
            Some(Applet::Toolchain(a)) => a.pretend_arg.pretend,
            Some(Applet::Stages(a)) => a.pretend_arg.pretend,
            Some(Applet::Setup(a)) => a.pretend_arg.pretend,
            Some(Applet::Depclean(a)) => a.pretend_arg.pretend,
            Some(Applet::Revdep(a)) => a.pretend_arg.pretend,
            Some(Applet::Ebuild(a)) => a.pretend_arg.pretend,
            Some(Applet::MirrorDist(a)) => a.pretend_arg.pretend,
            Some(Applet::Clean(a)) => a.pretend_arg.pretend,
            Some(Applet::Select(a)) => a.pretend_arg.pretend,
            Some(Applet::Sync(a)) => a.pretend_arg.pretend,
            Some(Applet::Maint(a)) => a.pretend_arg.pretend,
            Some(Applet::Etc(a)) => a.pretend_arg.pretend,
            _ => false,
        }
    }

    /// The dispatched applet's own `--arch`, or `default_arch` for an
    /// applet that doesn't carry [`ArchArg`].
    pub fn arch(&self) -> Arch {
        match &self.applet {
            Some(Applet::Emerge(a)) => a.arch_arg.arch,
            Some(Applet::Crossdev(a)) => a.arch_arg.arch,
            Some(Applet::Toolchain(a)) => a.arch_arg.arch,
            Some(Applet::Stages(a)) => a.arch_arg.arch,
            Some(Applet::Setup(a)) => a.arch_arg.arch,
            Some(Applet::Select(a)) => a.arch_arg.arch,
            Some(Applet::Query(a)) => a.arch_arg.arch,
            Some(Applet::Maint(a)) => a.arch_arg.arch,
            Some(Applet::Pkg(a)) => a.arch_arg.arch,
            _ => default_arch(),
        }
    }

    /// The dispatched applet's own `--repo`, or `None` for an applet that
    /// doesn't carry [`RepoArg`].
    pub(crate) fn repo_flag(&self) -> Option<String> {
        match &self.applet {
            Some(Applet::Emerge(a)) => a.repo_arg.repo.clone(),
            Some(Applet::Crossdev(a)) => a.repo_arg.repo.clone(),
            Some(Applet::Toolchain(a)) => a.repo_arg.repo.clone(),
            Some(Applet::Stages(a)) => a.repo_arg.repo.clone(),
            Some(Applet::Setup(a)) => a.repo_arg.repo.clone(),
            Some(Applet::Select(a)) => a.repo_arg.repo.clone(),
            Some(Applet::Query(a)) => a.repo_arg.repo.clone(),
            Some(Applet::Maint(a)) => a.repo_arg.repo.clone(),
            Some(Applet::Search(a)) => a.repo_arg.repo.clone(),
            Some(Applet::Ebuild(a)) => a.repo_arg.repo.clone(),
            Some(Applet::Grep(a)) => a.repo_arg.repo.clone(),
            _ => None,
        }
    }

    /// The dispatched applet's own [`MergeFlags`], or the all-default value
    /// for an applet that doesn't carry one.
    pub fn merge_flags(&self) -> MergeFlags {
        match &self.applet {
            Some(Applet::Emerge(a)) => a.merge_flags.clone(),
            Some(Applet::Crossdev(a)) => a.merge_flags.clone(),
            Some(Applet::Toolchain(a)) => a.merge_flags.clone(),
            Some(Applet::Stages(a)) => a.merge_flags.clone(),
            Some(Applet::Setup(a)) => a.merge_flags.clone(),
            Some(Applet::Revdep(a)) => a.merge_flags.clone(),
            Some(Applet::Depclean(a)) => a.merge_flags.clone(),
            _ => MergeFlags::default(),
        }
    }

    /// The dispatched applet's own [`DepgraphFlags`], for merge applets only — not query.
    pub fn depgraph_flags(&self) -> DepgraphFlags {
        match &self.applet {
            Some(Applet::Emerge(a)) => a.depgraph_flags.clone(),
            Some(Applet::Crossdev(a)) => a.depgraph_flags.clone(),
            Some(Applet::Toolchain(a)) => a.depgraph_flags.clone(),
            Some(Applet::Stages(a)) => a.depgraph_flags.clone(),
            Some(Applet::Setup(a)) => a.depgraph_flags.clone(),
            _ => DepgraphFlags::default(),
        }
    }

    /// The dispatched applet's own activity-output flags, then `EM_EMERGELOG`
    /// once if still off.
    pub fn effective_activity(&self) -> ActivityArgs {
        let mut activity = applet_activity(self);
        if !activity.emergelog && env_bool("EM_EMERGELOG").ok().flatten() == Some(true) {
            activity.emergelog = true;
        }
        activity
    }

    /// The dispatched applet's own `--privilege`, then `EM_PRIVILEGE` once if still `Auto`.
    pub fn effective_privilege(&self) -> Privilege {
        let applet = applet_privilege(self);
        if applet != Privilege::Auto {
            applet
        } else {
            privilege_from_env()
                .ok()
                .flatten()
                .unwrap_or(Privilege::Auto)
        }
    }

    /// `Applet::Emerge`'s own mode switches, or the all-`false` default when
    /// it isn't the active applet.
    pub fn mode(&self) -> EmergeModeArgs {
        match &self.applet {
            Some(Applet::Emerge(a)) => a.mode.clone(),
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
pub(crate) fn os_argv<'a>(argv: &'a [&'a str]) -> Vec<&'a OsStr> {
    argv.iter().copied().map(OsStr::new).collect()
}

/// Parse argv including argv0, through `default_subcommand_flags`' own routing.
#[cfg(test)]
pub(crate) fn parse_cli(argv: &[&str]) -> Cli {
    Cli::parse_from_argv(argv).unwrap_or_else(|e| panic!("expected parse of {argv:?}: {e}"))
}

#[cfg(test)]
pub(crate) fn parse_cli_into(argv: &[&str]) -> Result<Cli, String> {
    Cli::parse_from_argv(argv)
        .and_then(Validated::try_from)
        .map(|Validated(cli)| cli)
        .map_err(|e| err_name(&e))
}

#[cfg(test)]
fn err_name(e: &winnow_args::Error) -> String {
    use winnow_args::ErrorKind;
    let token = e.token().unwrap_or_default();
    match e.kind() {
        ErrorKind::UnknownFlag => format!("UnknownFlag {token}"),
        ErrorKind::HelpRequested if e.is_bare_help() => "MissingArgsHelp".into(),
        ErrorKind::HelpRequested => "Help".into(),
        ErrorKind::VersionRequested => "Version".into(),
        ErrorKind::InvalidValue | ErrorKind::InvalidChoice => format!("InvalidValue {token} {e}"),
        ErrorKind::MissingRequired | ErrorKind::MissingArgument => {
            format!("MissingRequired {token}")
        }
        other => format!("{other:?} {e}"),
    }
}

#[cfg(test)]
fn parse_err(argv: &[&str]) -> String {
    parse_cli_into(argv).expect_err(&format!("expected error for {argv:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn emerge<'a>(argv: impl IntoIterator<Item = &'a str>) -> Cli {
        let v: Vec<&str> = ["em", "emerge"].into_iter().chain(argv).collect();
        parse_cli(&v)
    }

    /// What a shell is offered after `words`, the last one being typed.
    fn candidates(words: &[&str]) -> Vec<String> {
        let offered = winnow_args::complete::complete(Cli::HELP, words);
        offered.candidates.into_iter().map(|c| c.value).collect()
    }

    #[test]
    fn completions_omit_hidden_applets_and_offer_emerge() {
        let offered = candidates(&[""]);
        assert!(
            offered.iter().any(|w| w == "emerge"),
            "emerge missing: {offered:?}"
        );
        assert!(
            offered.iter().any(|w| w == "completion"),
            "completion missing: {offered:?}"
        );
        assert!(
            !offered
                .iter()
                .any(|w| w.contains("__helper") || w.contains("__worker")),
            "hidden applet leaked: {offered:?}"
        );
    }

    #[test]
    fn completions_offer_prefix_after_toolchain() {
        let offered = candidates(&["toolchain", "--"]);
        assert!(
            offered.iter().any(|w| w == "--prefix"),
            "--prefix missing after toolchain: {offered:?}"
        );
    }

    #[test]
    fn completion_applet_parses_shell_name() {
        let cli = parse_cli(&["em", "completion", "bash"]);
        match &cli.applet {
            Some(Applet::Completion(a)) => assert_eq!(a.shell, "bash"),
            other => panic!("expected completion, got {other:?}"),
        }
    }

    #[test]
    fn completion_request_is_not_an_emerge_atom() {
        let argv = vec![
            std::ffi::OsString::from("__complete_word__"),
            std::ffi::OsString::from("--shell"),
            std::ffi::OsString::from("bash"),
            std::ffi::OsString::from("--line"),
            std::ffi::OsString::from("em "),
        ];
        let answer = Cli::completion_request(&argv).expect("recognized");
        assert!(answer.contains("emerge"), "{answer}");
        assert!(!answer.contains("__helper"), "{answer}");
        assert!(!answer.contains("__worker"), "{answer}");
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

    #[test]
    fn target_takes_a_spec_file_and_reads_its_tuple() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("board.toml");
        let target =
            crate::crossdev::target::CrossTarget::parse("riscv64-unknown-linux-gnu", false)
                .unwrap();
        let mut spec = crate::crossdev::spec::SysrootSpec::builtin(&target, &[]);
        spec.cflags = "-O3 -march=rva23u64 -pipe".into();
        std::fs::write(&path, spec.to_toml().unwrap()).unwrap();

        let cli = emerge(["--target", path.to_str().unwrap(), "-p", "sys-libs/zlib"]);
        assert_eq!(cli.target().as_deref(), Some("riscv64-unknown-linux-gnu"));
        assert_eq!(cli.target_spec(), Some(spec));
        assert_eq!(
            cli.roots().merge_root().as_str(),
            "/usr/riscv64-unknown-linux-gnu"
        );

        let tuple = emerge([
            "--target",
            "riscv64-unknown-linux-gnu",
            "-p",
            "sys-libs/zlib",
        ]);
        assert_eq!(tuple.target_spec(), None);
        assert!(
            parse_cli_into(&["em", "--target", "./missing.toml", "-p", "sys-libs/zlib"]).is_err()
        );
    }

    #[test]
    fn a_board_root_does_not_count_the_sysroots_packages_as_installed() {
        let (_tmp, _g) = crate::test_support::isolate_active_state();
        let prefix = tempfile::tempdir().unwrap();
        let board = tempfile::tempdir().unwrap();
        let tuple = "riscv64-unknown-linux-gnu";
        let pkg = prefix
            .path()
            .join("usr")
            .join(tuple)
            .join("var/db/pkg/sys-apps/baselayout-2.18-r1");
        std::fs::create_dir_all(&pkg).unwrap();
        for (file, value) in [
            ("EAPI", "8"),
            ("SLOT", "0"),
            ("CONTENTS", ""),
            ("USE", "build"),
        ] {
            std::fs::write(pkg.join(file), value).unwrap();
        }
        let prefix_arg = prefix.path().to_str().unwrap();
        let board_arg = board.path().to_str().unwrap();
        let installed = |args: &[&str]| {
            let roots = emerge(args.iter().copied()).roots();
            portage_resolve::installed::load_target_installed_with_cache(
                &roots,
                &Default::default(),
            )
        };

        let board_view = installed(&[
            "--prefix",
            prefix_arg,
            "--root",
            board_arg,
            "--target",
            tuple,
            "-p",
            "sys-apps/baselayout",
        ]);
        assert_eq!(board_view.len(), 0);
        let sysroot_view = installed(&[
            "--prefix",
            prefix_arg,
            "--target",
            tuple,
            "-p",
            "sys-apps/baselayout",
        ]);
        assert_eq!(sysroot_view.len(), 1);
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
    // symlinks host python there (setup/mod.rs) to satisfy them without building
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

    fn overlay_root_of(cli: &Cli) -> Option<String> {
        cli.topology_and_root().1.root
    }

    fn merged_topology(cli: &Cli) -> Topology {
        cli.topology_and_root().0
    }

    // emerge's `--selective [y|n]`: bare is `y`, the value attaches with `=`, and `n` overrides `-n`
    #[test]
    fn selective_parses_like_emerge() {
        let selective = |args: &[&str]| parse_cli(args).merge_flags().selective();
        assert_eq!(selective(&["em", "-p", "sys-libs/zlib"]), None);
        assert_eq!(selective(&["em", "-pn", "sys-libs/zlib"]), Some(true));
        assert_eq!(
            selective(&["em", "-p", "--selective", "sys-libs/zlib"]),
            Some(true)
        );
        assert_eq!(
            selective(&["em", "-pn", "--selective=n", "sys-libs/zlib"]),
            Some(false)
        );
        let cli = parse_cli(&["em", "-p", "--selective", "sys-libs/zlib"]);
        assert_eq!(emerge_applet(&cli).atoms, vec!["sys-libs/zlib".to_string()]);
    }

    #[test]
    fn bare_and_explicit_emerge_produce_identical_args() {
        let bare = parse_cli(&["em", "--root", "/srv/x", "-p", "sys-libs/zlib"]);
        let explicit = parse_cli(&["em", "emerge", "--root", "/srv/x", "-p", "sys-libs/zlib"]);
        assert_eq!(overlay_root_of(&bare).as_deref(), Some("/srv/x"));
        assert_eq!(overlay_root_of(&explicit).as_deref(), Some("/srv/x"));
        assert_eq!(emerge_applet(&bare).atoms, emerge_applet(&explicit).atoms);
        assert!(bare.pretend() && explicit.pretend());
    }

    #[test]
    fn flags_before_emerge_word_reorder() {
        let cli = parse_cli(&["em", "--root", "/srv/x", "emerge", "-p", "sys-libs/zlib"]);
        assert_eq!(overlay_root_of(&cli).as_deref(), Some("/srv/x"));
        assert_eq!(emerge_applet(&cli).atoms, vec!["sys-libs/zlib".to_string()]);
        assert!(cli.pretend());
    }

    #[test]
    fn unrecognized_flag_inside_a_real_subcommand_does_not_retry_as_emerge() {
        let err = parse_err(&["em", "crossdev", "--bogus-flag"]);
        assert_eq!(err, "UnknownFlag --bogus-flag");
    }

    #[test]
    fn prefix_root_then_crossdev_is_refused_by_validation() {
        let cli = parse_cli(&["em", "--root", "/tmp/r", "crossdev", "--setup"]);
        assert_eq!(cli.root_topology.root.as_deref(), Some("/tmp/r"));
        assert!(matches!(cli.applet, Some(Applet::Crossdev(_))));
        let err = parse_cli_into(&["em", "--root", "/tmp/r", "crossdev", "--setup"]).unwrap_err();
        assert!(
            err.contains("InvalidValue --root"),
            "validation must reject prefix --root with crossdev, got {err}"
        );
    }

    #[test]
    fn prefix_root_then_worker_is_refused_by_validation() {
        let mut argv = worker_argv(&[]);
        argv.insert(1, "--root");
        argv.insert(2, "/srv");
        let err = parse_cli_into(&argv).unwrap_err();
        assert!(
            err.contains("InvalidValue --root"),
            "validation must reject prefix --root with __worker, got {err}"
        );
    }

    #[test]
    fn validation_messages_name_the_actual_conflict() {
        let words = ["em", "--root", "/tmp/r", "crossdev", "--setup"];
        let err = match Cli::parse_from_argv(words).and_then(Validated::try_from) {
            Ok(_) => panic!("expected validation to refuse it"),
            Err(e) => e,
        };
        let rendered = err.render(winnow_args::help::Style::PLAIN);
        assert!(
            rendered.contains("invalid value `/tmp/r` for `--root`")
                && rendered.contains("'crossdev' applet"),
            "got {rendered}"
        );
    }

    #[test]
    fn version_is_not_emerge() {
        let err = Cli::parse_from(["--version"]).unwrap_err();
        assert_eq!(
            err.kind(),
            winnow_args::ErrorKind::VersionRequested,
            "--version should be Version"
        );
    }

    #[test]
    fn dash_p_alone_defaults_to_an_empty_emerge_preview() {
        // -p is now a per-applet flag (PretendArg on EmergeArgs), so it
        // reaches the default subcommand the same way -u/-a do: `em -p`
        // commits to `emerge` with no atoms, rather than leaving `applet`
        // unset.
        let cli = parse_cli(&["em", "-p"]);
        assert!(cli.pretend());
        assert!(emerge_applet(&cli).atoms.is_empty());
    }

    #[test]
    fn info_json_parses_as_emerge_with_no_atoms() {
        let cli = parse_cli(&["em", "--info", "--json"]);
        assert!(cli.mode().info);
        assert!(cli.merge_flags().json);
        assert!(emerge_applet(&cli).atoms.is_empty());
    }

    #[test]
    fn info_before_a_word_naming_an_applet_is_swallowed_as_an_atom() {
        // `--info` now lives on EmergeModeArgs (default-child-only), so it
        // commits the line to emerge before `use` is considered as a
        // possible applet word — same reasoning as -a/-X in
        // merge_flags_need_the_explicit_applet_word.
        let cli = parse_cli(&["em", "--info", "use"]);
        assert!(cli.mode().info);
        assert_eq!(emerge_applet(&cli).atoms, ["use"]);
    }

    #[test]
    fn info_firefox_is_emerge_not_info_only() {
        let cli = parse_cli(&["em", "--info", "firefox"]);
        assert!(cli.mode().info);
        assert_eq!(emerge_applet(&cli).atoms, ["firefox"]);
    }

    #[test]
    fn emerge_info_parses() {
        let cli = parse_cli(&["em", "emerge", "--info"]);
        assert!(cli.mode().info);
        assert!(emerge_applet(&cli).atoms.is_empty());
    }

    #[test]
    fn arch_and_repo_work_on_the_bare_path() {
        let cli = parse_cli(&["em", "--arch", "amd64", "-p", "sys-libs/zlib"]);
        assert_eq!(cli.arch(), Arch::from_str("amd64").unwrap());
        assert_eq!(emerge_applet(&cli).atoms, vec!["sys-libs/zlib".to_string()]);

        // Leading with the explicit "emerge" word works too, as long as no
        // flag precedes it (see the Cli doc comment for the flag-then-"emerge"
        // caveat).
        let via_emerge = parse_cli(&["em", "emerge", "--arch", "amd64", "-p", "sys-libs/zlib"]);
        assert_eq!(via_emerge.arch(), cli.arch());

        let repo = parse_cli(&["em", "--repo", "/tmp/r", "-p", "sys-libs/zlib"]);
        assert_eq!(repo.repo_flag().as_deref(), Some("/tmp/r"));
    }

    #[test]
    fn json_before_emerge_word_is_merge_plan_json() {
        let explicit = parse_cli(&["em", "emerge", "--json", "-p", "sys-libs/zlib"]);
        assert!(explicit.merge_flags().json);
        let bare = parse_cli(&["em", "--json", "-p", "sys-libs/zlib"]);
        assert!(bare.merge_flags().json);
    }

    #[test]
    fn a_prefix_flag_before_an_explicit_emerge_word_swallows_it_as_an_atom() {
        // Known, accepted limitation (see the Cli doc comment): once --json
        // commits the line to the default command, the later literal word
        // "emerge" is just another atom, not a no-op restating the applet.
        let cli = parse_cli(&["em", "--json", "emerge", "-p", "sys-libs/zlib"]);
        assert!(cli.merge_flags().json);
        assert_eq!(emerge_applet(&cli).atoms, ["emerge", "sys-libs/zlib"]);
    }

    #[test]
    fn exclude_value_matching_an_applet_name_is_not_the_search_applet() {
        let cli = parse_cli(&["em", "emerge", "-X", "search", "-p", "sys-libs/zlib"]);
        assert_eq!(cli.merge_flags().exclude, vec!["search".to_string()]);
        assert_eq!(emerge_applet(&cli).atoms, vec!["sys-libs/zlib".to_string()]);
        assert!(matches!(cli.applet, Some(Applet::Emerge(_))));
    }

    #[test]
    fn root_value_named_emerge_is_kept() {
        let cli = parse_cli(&["em", "--root", "emerge", "-p", "sys-libs/zlib"]);
        assert_eq!(overlay_root_of(&cli).as_deref(), Some("emerge"));
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
        assert_eq!(merged_topology(&cli).prefix.as_deref(), Some("P"));
        assert_eq!(emerge_applet(&cli).atoms, ["firefox"]);

        let tc = parse_cli(&["em", "--prefix", "P", "toolchain", "--setup"]);
        assert_eq!(merged_topology(&tc).prefix.as_deref(), Some("P"));
        assert!(matches!(tc.applet, Some(Applet::Toolchain(_))));

        let canon = parse_cli(&["em", "toolchain", "--prefix", "P", "--setup"]);
        assert_eq!(merged_topology(&canon).prefix.as_deref(), Some("P"));
        assert!(matches!(canon.applet, Some(Applet::Toolchain(_))));
    }

    #[test]
    fn query_depgraph_nested_root_and_prefix() {
        let prefix = parse_cli(&["em", "query", "depgraph", "--prefix", "P", "zlib"]);
        assert_eq!(merged_topology(&prefix).prefix.as_deref(), Some("P"));
        assert!(matches!(prefix.applet, Some(Applet::Query(_))));

        let root = parse_cli(&["em", "query", "depgraph", "--root", "R", "zlib"]);
        match &root.applet {
            Some(Applet::Query(q)) => assert_eq!(q.topology.root.as_deref(), Some("R")),
            other => panic!("expected query, got {other:?}"),
        }
    }

    #[test]
    fn pretend_only_selects_toolchain_in_postfix_position() {
        // Known, accepted limitation (see the Cli doc comment): `-p` is now
        // per-applet, so a leading `-p` commits to the default (emerge)
        // subcommand before "toolchain" is considered — it's swallowed as an
        // atom, not routed to Toolchain. Postfix position still works.
        let before = parse_cli(&["em", "-p", "toolchain"]);
        assert!(before.pretend());
        assert_eq!(emerge_applet(&before).atoms, ["toolchain"]);

        let after = parse_cli(&["em", "toolchain", "-p"]);
        assert!(after.pretend());
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
    fn prefix_ask_before_a_word_naming_an_applet_is_swallowed_as_an_atom() {
        // Once `-a` (emerge-only) commits the line to the default subcommand,
        // `default_subcommand_flags` does not look ahead for a
        // real applet name — later words become emerge's own atoms instead.
        let cli = parse_cli(&["em", "-a", "search", "zlib"]);
        assert!(cli.merge_flags().ask);
        assert_eq!(emerge_applet(&cli).atoms, ["search", "zlib"]);
    }

    #[test]
    fn emerge_dash_a_is_ask() {
        let cli = parse_cli_into(&["em", "emerge", "-a", "pkg"]).expect("ask");
        assert!(cli.merge_flags().ask);
        assert_eq!(emerge_applet(&cli).atoms, ["pkg"]);
    }

    #[test]
    fn prefix_deep_before_a_word_naming_an_applet_is_swallowed_as_an_atom() {
        // Same reasoning as prefix_ask above: `--deep` commits to emerge first.
        let cli = parse_cli(&["em", "--deep", "query", "depgraph", "zlib"]);
        assert!(cli.depgraph_flags().deep);
        assert_eq!(emerge_applet(&cli).atoms, ["query", "depgraph", "zlib"]);

        // No prefix flag precedes the applet word here, so `query depgraph`
        // still routes normally.
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
    fn bundled_update_deep_before_a_word_naming_an_applet_is_swallowed_as_an_atom() {
        let cli = parse_cli(&["em", "-uD", "query", "belongs", "/usr/bin/python"]);
        assert!(cli.merge_flags().update);
        assert!(cli.depgraph_flags().deep);
        assert_eq!(
            emerge_applet(&cli).atoms,
            ["query", "belongs", "/usr/bin/python"]
        );
    }

    #[test]
    fn merge_flags_before_an_explicit_emerge_word_accumulate() {
        let cli = parse_cli(&["em", "-X", "foo", "emerge", "-X", "bar", "pkg"]);
        assert_eq!(
            cli.merge_flags().exclude,
            vec!["foo".to_string(), "bar".to_string()]
        );

        let or_flags = parse_cli(&["em", "-u", "emerge", "-D", "pkg"]);
        assert!(or_flags.merge_flags().update);
        assert!(or_flags.depgraph_flags().deep);
    }

    #[test]
    fn privilege_prefix_none_beats_env_sudo() {
        // Same lock local_is_standalone_not_overlay takes: the hazard isn't
        // another writer of *this* var, it's any concurrent env read on
        // another thread while this one mutates the process environ.
        let _env_lock = crate::test_support::home_lock();
        let saved = std::env::var("EM_PRIVILEGE").ok();
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
    fn invalid_em_privilege_is_refused_by_validation() {
        let _env_lock = crate::test_support::home_lock();
        let saved = std::env::var("EM_PRIVILEGE").ok();
        unsafe { std::env::set_var("EM_PRIVILEGE", "typo") };
        let err = parse_cli_into(&["em", "emerge", "-p", "pkg"]);
        unsafe {
            match saved {
                Some(v) => std::env::set_var("EM_PRIVILEGE", v),
                None => std::env::remove_var("EM_PRIVILEGE"),
            }
        }
        let err = err.unwrap_err();
        assert!(
            err.contains("InvalidValue EM_PRIVILEGE"),
            "bad EM_PRIVILEGE must fail validation, got {err}"
        );
    }

    #[test]
    fn invalid_em_privilege_is_ignored_when_the_flag_is_set() {
        let _env_lock = crate::test_support::home_lock();
        let saved = std::env::var("EM_PRIVILEGE").ok();
        unsafe { std::env::set_var("EM_PRIVILEGE", "typo") };
        let cli = parse_cli_into(&["em", "--privilege", "none", "emerge", "pkg"]);
        unsafe {
            match saved {
                Some(v) => std::env::set_var("EM_PRIVILEGE", v),
                None => std::env::remove_var("EM_PRIVILEGE"),
            }
        }
        assert_eq!(cli.unwrap().effective_privilege(), Privilege::None);
    }

    #[test]
    fn invalid_em_emergelog_is_refused_by_validation() {
        let _env_lock = crate::test_support::home_lock();
        let saved = std::env::var("EM_EMERGELOG").ok();
        unsafe { std::env::set_var("EM_EMERGELOG", "nope") };
        let err = parse_cli_into(&["em", "emerge", "-p", "pkg"]);
        unsafe {
            match saved {
                Some(v) => std::env::set_var("EM_EMERGELOG", v),
                None => std::env::remove_var("EM_EMERGELOG"),
            }
        }
        let err = err.unwrap_err();
        assert!(
            err.contains("InvalidValue EM_EMERGELOG"),
            "bad EM_EMERGELOG must fail validation, got {err}"
        );
    }

    #[test]
    fn local_default_missing_and_the_set_trap() {
        let ok = parse_cli(&["em", "active", "set", "--local="]);
        assert_eq!(merged_topology(&ok).local.as_deref(), Some(""));
        match &ok.applet {
            Some(Applet::Active(a)) => {
                assert!(matches!(a.command, Some(ActiveCommand::Set { .. })))
            }
            other => panic!("expected active set, got {other:?}"),
        }

        // A detached `--local` still takes the next word as DIR. `--local=`
        // (or `--local` at end of argv) does not, so `set` can follow.
        let stolen = parse_cli(&["em", "active", "--local", "set"]);
        assert_eq!(merged_topology(&stolen).local.as_deref(), Some("set"));
        match &stolen.applet {
            Some(Applet::Active(a)) => assert!(a.command.is_none()),
            other => panic!("expected active, got {other:?}"),
        }
    }

    #[test]
    fn active_set_does_not_require_subcommand_first() {
        for argv in [
            ["em", "active", "set", "--local="].as_slice(),
            &["em", "active", "--local=", "set"],
            &["em", "--local=", "active", "set"],
            &["em", "active", "set", "--local"],
        ] {
            let cli = parse_cli(argv);
            assert_eq!(merged_topology(&cli).local.as_deref(), Some(""), "{argv:?}");
            match &cli.applet {
                Some(Applet::Active(a)) => {
                    assert!(
                        matches!(a.command, Some(ActiveCommand::Set { .. })),
                        "{argv:?} -> {a:?}"
                    );
                }
                other => panic!("expected active set for {argv:?}, got {other:?}"),
            }
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
    fn worker_quiet_is_its_own_field() {
        let argv = worker_argv(&["--quiet"]);
        let cli = parse_cli(&argv);
        assert!(cli.quiet());
        let Some(Applet::Worker(w)) = &cli.applet else {
            panic!("expected Applet::Worker");
        };
        assert!(w.quiet);
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

        // `--config-root` (RootTopology's parent-flag copy, for `em --config-root X
        // toolchain --setup` routing) is not global, so it doesn't reach Worker's
        // parsing scope: Worker flattens neither `Topology` nor `RootedTopology`,
        // so it is an `UnknownFlag`, not a silently ignored bind.
        let bad = worker_argv(&["--config-root", "/tmp/cfg"]);
        assert_eq!(parse_err(&bad), "UnknownFlag --config-root");
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
    fn helper_hyphen_args_without_separator() {
        let cli = parse_cli(&["em", "__helper", "dodoc", "-foo"]);
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

    #[test]
    fn mode_flags_without_a_positional_retry_into_emerge() {
        let resume = parse_cli(&["em", "-r"]);
        assert!(matches!(resume.applet, Some(Applet::Emerge(_))));
        assert!(resume.mode().resume);

        let depclean = parse_cli(&["em", "-c"]);
        assert!(matches!(depclean.applet, Some(Applet::Emerge(_))));
        assert!(depclean.mode().depclean);

        let search = parse_cli(&["em", "-s"]);
        assert!(matches!(search.applet, Some(Applet::Emerge(_))));
        assert!(search.mode().search);

        // `-p` is a per-applet flag like the others above now, so it retries
        // into emerge the same way.
        let pretend = parse_cli(&["em", "-p"]);
        assert!(matches!(pretend.applet, Some(Applet::Emerge(_))));
        assert!(pretend.pretend());
        assert_eq!(pretend.mode(), EmergeModeArgs::default());
    }

    #[test]
    fn mode_flags_before_or_after_emerge_word() {
        let prefix_s = parse_cli(&["em", "-s", "firefox"]);
        assert!(prefix_s.mode().search);
        assert_eq!(prefix_s.atoms(), ["firefox"]);

        let applet_s = parse_cli(&["em", "emerge", "-s", "firefox"]);
        assert!(applet_s.mode().search);
        assert_eq!(applet_s.atoms(), ["firefox"]);

        let unmerge = parse_cli(&["em", "-C", "firefox"]);
        assert!(unmerge.mode().unmerge);
        assert_eq!(unmerge.atoms(), ["firefox"]);

        let nodeps = parse_cli(&["em", "-O", "firefox"]);
        assert!(nodeps.mode().nodeps);
        assert_eq!(nodeps.atoms(), ["firefox"]);
    }

    #[test]
    fn merge_flags_need_the_explicit_applet_word() {
        // `-a` (emerge-only) commits the line to emerge before `crossdev` is
        // ever considered as a possible applet word, so `--setup` — not a real
        // Emerge flag — is what the real parse then rejects.
        assert_eq!(
            parse_cli_into(&[
                "em",
                "-a",
                "crossdev",
                "--target",
                "riscv64-unknown-linux-gnu",
                "--setup"
            ])
            .unwrap_err(),
            "UnknownFlag --setup"
        );
        // `-X` takes a value, so `depclean` is the exclude atom, not the applet.
        let just_x = parse_cli(&["em", "-X", "depclean"]);
        assert!(matches!(just_x.applet, Some(Applet::Emerge(_))));
        assert_eq!(just_x.merge_flags().exclude, ["depclean"]);
        assert!(just_x.atoms().is_empty());

        // After the value, `depclean` is just another word within the already
        // emerge-committed line — swallowed as an atom, not routed to the
        // `depclean` applet. Same reasoning as `-a search` above.
        let excluded_word = parse_cli(&["em", "-X", "foo", "depclean"]);
        assert!(matches!(excluded_word.applet, Some(Applet::Emerge(_))));
        assert_eq!(excluded_word.merge_flags().exclude, ["foo"]);
        assert_eq!(excluded_word.atoms(), ["depclean"]);

        // A value that spells an applet name is still the flag's value, so this
        // retries into emerge rather than selecting `search`.
        let excluded = parse_cli(&["em", "-X", "search", "zlib"]);
        assert!(matches!(excluded.applet, Some(Applet::Emerge(_))));
        assert_eq!(excluded.merge_flags().exclude, ["search"]);
        assert_eq!(excluded.atoms(), ["zlib"]);

        let crossdev = parse_cli(&[
            "em",
            "crossdev",
            "-a",
            "--target",
            "riscv64-unknown-linux-gnu",
            "--setup",
        ]);
        assert!(crossdev.merge_flags().ask);
        assert!(matches!(crossdev.applet, Some(Applet::Crossdev(_))));

        let depclean = parse_cli(&["em", "depclean", "-X", "foo"]);
        assert_eq!(depclean.merge_flags().exclude, ["foo"]);
        assert!(matches!(depclean.applet, Some(Applet::Depclean(_))));
    }

    #[test]
    fn keep_going_retries_into_emerge() {
        let prefix = parse_cli(&["em", "--keep-going", "-r"]);
        assert!(prefix.mode().resume);
        assert!(prefix.merge_flags().keep_going);
        assert!(matches!(prefix.applet, Some(Applet::Emerge(_))));

        let applet = parse_cli(&["em", "emerge", "-r", "--keep-going"]);
        assert!(applet.mode().resume);
        assert!(applet.merge_flags().keep_going);
    }
}

/// Hidden `em __worker` install child — spawned per package by `build_and_merge`.
///
/// `--quiet` is its own field, not [`QuietArg`] — this is machine-generated
/// argv from `privilege.rs`'s re-exec, not something a person types. `--config-root`
/// is Topology's; this child takes `--worker-config-root` so the two never
/// share a spelling.
#[derive(winnow_args::Args, Debug, Clone)]
pub struct WorkerArgs {
    #[arg(long)]
    pub quiet: bool,
    #[arg(long)]
    pub ebuild: String,
    /// The resolved plan entry's authoritative cpv — see
    /// `privilege::WorkerArgs::cpv`.
    #[arg(long)]
    pub cpv: String,
    #[arg(long)]
    pub use_flags: String,
    #[arg(long)]
    pub work_base: String,
    #[arg(long)]
    pub root: String,
    #[arg(long)]
    pub distdir: Option<String>,
    #[arg(long)]
    pub worker_config_root: Option<String>,
    #[arg(long)]
    pub sysroot: Option<String>,
    #[arg(long)]
    pub eprefix: Option<String>,
    /// Where BDEPEND-class build tools live (`Cli::host_roots()`'s merge root)
    #[arg(long)]
    pub broot: Option<String>,
    /// See `ebuild::RootContext::self_contained_bootstrap`
    #[arg(long)]
    pub self_contained_bootstrap: bool,
    /// See `ebuild::RootContext::extra_path`, `:`-joined
    #[arg(long)]
    pub extra_path: Option<String>,
    /// A pre-built GPKG to merge (`-k`/`-g`)
    #[arg(long)]
    pub binpkg: Option<String>,
    /// `binpkg`'s origin forces cryptographic GPG signature
    /// verification (a `binrepos.conf` entry with
    /// `verify-signature = yes`), independent of
    /// `FEATURES=binpkg-request-signature`.
    #[arg(long)]
    pub force_verify_signature: bool,
    #[arg(long)]
    pub buildpkg: bool,
    /// Parent activity session id — live FS phase updates only
    #[arg(long)]
    pub activity_job_id: Option<String>,
    #[arg(long)]
    pub activity_parent_job_id: Option<String>,
    /// Filesystem root of the parent's live activity sink
    #[arg(long)]
    pub activity_live_root: Option<String>,
    /// `host` or `target` package side for inflight paths
    #[arg(long)]
    pub activity_side: Option<String>,
    /// Unix socket path: stream phase JSONL back to the parent activity bus
    #[arg(long)]
    pub activity_reemit_path: Option<String>,
}

#[derive(winnow_args::Subcommand, Debug, Clone)]
#[allow(clippy::large_enum_variant)] // __worker carries many CLI strings
#[arg(run_async)]
pub enum Applet {
    /// Run one do*/new* install helper standalone against the exported build env
    ///
    /// Internal: backs the PATH shims dropped during a build so `find -exec doman` /
    /// `xargs do*` reach helpers that are in-shell builtins. Not for direct use.
    #[arg(name = "__helper", hide)]
    Helper(HelperArgs),

    /// Internal: the privilege-wrapped install worker (install+qmerge+binpkg
    /// for one package; spawned per package by `build_and_merge`).
    #[arg(name = "__worker", hide)]
    Worker(WorkerArgs),

    #[arg(help = "Execute ebuild phases")]
    Ebuild(EbuildArgs),

    #[arg(help = "System maintenance and health checks")]
    Maint(MaintArgs),

    #[arg(help = "Query Portage internal variables and data")]
    Portageq(PortageqArgs),

    /// Sync ebuild repositories from `repos.conf` (`git` and `rsync`)
    ///
    /// With no names, syncs every entry with `auto-sync = yes` (Portage
    /// default) and a usable `sync-type`/`sync-uri`. Named repos are synced
    /// regardless of `auto-sync`.
    ///
    /// Uses `git` / `rsync` (Portage parity) for the actual transfer.
    ///
    /// Same command as `em maint sync` — this top-level form exists only
    /// because `sync` is common enough to deserve a short invocation,
    /// matching real Portage having both `emerge --sync` and `emaint sync`.
    // Both dispatch to `crate::maint::sync::run`.
    #[arg(help = "Sync repositories (git, rsync)")]
    Sync(SyncArgs),

    #[arg(help = "Remove orphaned/unused packages")]
    Depclean(DepcleanArgs),

    #[arg(help = "Regenerate metadata cache")]
    Regen(RegenArgs),

    #[arg(help = "Create binary packages from installed files")]
    Quickpkg(QuickpkgArgs),

    #[arg(
        name = "mirrordist",
        alias_hidden = "emirrordist",
        help = "Build/maintain a distfiles mirror (emirrordist workalike)",
        long_help = "Walks every ebuild in a repository, fetches every distfile its SRC_URI references (all versions, all USE branches), and verifies each against the repo Manifest — the server side of a Gentoo mirror.\n\nNot to be confused with `em select mirrors`, which chooses which mirrors *this* machine fetches from.\n\nRequires an up-to-date metadata cache: run `em regen <repo>` first for overlays."
    )]
    MirrorDist(MirrorDistArgs),

    #[arg(help = "Query package information")]
    Query(QueryArgs),

    #[arg(help = "Clean distfiles and/or binary packages")]
    Clean(CleanArgs),

    #[arg(help = "Enable/disable/query USE flags in make.conf")]
    Use(UseArgs),

    #[arg(help = "Edit per-package configuration (package.use, .keywords, .mask, .env)")]
    Pkg(PkgArgs),

    #[arg(help = "Rebuild packages with broken shared library deps")]
    Revdep(RevdepArgs),

    #[arg(help = "Display Portage elog files")]
    Read(ReadArgs),

    #[arg(help = "Analyze emerge.log")]
    Log(LogArgs),

    #[arg(help = "Search inside ebuilds and eclasses")]
    Grep(GrepArgs),

    #[arg(help = "Search package names and descriptions")]
    Search(SearchArgs),

    #[arg(help = "Parse/split atom strings")]
    Atom(AtomArgs),

    #[arg(help = "Native config selectors (profile, repos) — eselect-like")]
    Select(SelectArgs),

    /// Register a default `--prefix` / `--local` so bare `em <pkg>` picks it up (dogfooding)
    ///
    /// Explicit `--prefix`/`--local`/`--root` still win. State is stored under
    /// `$XDG_STATE_HOME/em/active`.
    #[arg(help = "Register a default --prefix/--local for bare em invocations")]
    Active(ActiveArgs),

    #[arg(help = "Bootstrap a prefix layout (use with --local or --prefix)")]
    Setup(SetupArgs),

    #[arg(help = "Set up a cross-compilation target (sysroot + overlay) — crossdev workalike")]
    Crossdev(CrossdevArgs),

    #[arg(help = "Bootstrap a self-hosting native toolchain (--root / --prefix / --local)")]
    Toolchain(ToolchainArgs),

    #[arg(help = "Assemble stage-build artifacts (stage1 packages.build) into --root")]
    Stages(StagesArgs),

    #[arg(
        help = "Reconcile pending config files (etc-update / dispatch-conf)",
        alias_hidden = "config",
        alias_hidden = "dispatch"
    )]
    Etc(EtcArgs),

    #[arg(help = "Regenerate /etc/profile.env and ld.so cache")]
    Env(EnvArgs),

    #[arg(help = "Print a shell completion script")]
    Completion(CompletionArgs),

    /// Resolve and merge/unmerge packages (emerge workalike).
    ///
    /// `em <atoms>` and `em emerge <atoms>` parse into the same arguments.
    #[arg(help = "Resolve and merge/unmerge packages (emerge workalike)")]
    Emerge(EmergeArgs),
}

/// Hidden `em __helper` shim — PATH helper dispatched by `find -exec`/`xargs`.
#[derive(winnow_args::Args, Debug, Clone)]
pub struct HelperArgs {
    /// Helper name (e.g. `doman`, `dolib.a`). Automatic so hyphen-prefixed
    /// helper args parse without a `--` (`find -exec doman -m …`).
    #[arg(positional, double_dash = "automatic")]
    pub name: String,
    /// Arguments passed through to the helper
    #[arg(positional, double_dash = "automatic")]
    pub args: Vec<String>,
}

/// `em ebuild` — execute ebuild phases
#[derive(winnow_args::Args, Debug, Clone)]
pub struct EbuildArgs {
    /// Path to the `.ebuild` file to execute
    #[arg(positional)]
    pub ebuild_path: String,
    /// Phase(s) to run in order (e.g. `compile`, `install`, `qmerge`)
    #[arg(positional, required)]
    pub phase: Vec<String>,
    /// Override the build work directory (default: `/var/tmp/portage/<cat>/<pf>`)
    #[arg(from_str, short = 'w', long, value_name = "DIR")]
    pub work_dir: Option<camino::Utf8PathBuf>,
    #[arg(flatten)]
    pub topology: RootedTopology,
    #[arg(flatten)]
    pub repo_arg: RepoArg,
    #[arg(flatten)]
    pub pretend_arg: PretendArg,
}

/// `em maint` — system maintenance and health checks
#[derive(winnow_args::Args, Debug, Clone)]
#[arg(flatten_help)]
pub struct MaintArgs {
    #[arg(subcommand)]
    pub command: MaintCommand,
    #[arg(flatten)]
    pub topology: RootedTopology,
    #[arg(flatten)]
    pub verbose_arg: VerboseArg,
    #[arg(flatten)]
    pub quiet_arg: QuietArg,
    #[arg(flatten)]
    pub arch_arg: ArchArg,
    #[arg(flatten)]
    pub repo_arg: RepoArg,
    #[arg(flatten)]
    pub pretend_arg: PretendArg,
    #[arg(flatten)]
    pub vdb_arg: VdbArg,
}

/// `em portageq` — a subset of Portage's own query interface (byte-compatible
/// where implemented: the installed-package queries and `envvar`)
#[derive(winnow_args::Args, Debug, Clone)]
#[arg(
    effect = "read",
    exit_code(0, "query succeeded"),
    exit_code(1, "no match / unset variable"),
    exit_code(2, "invalid atom, or no variable named"),
    exit_code(64, "<EROOT> is not a directory"),
    example = "em portageq has_version / sys-apps/portage"
)]
pub struct PortageqArgs {
    #[arg(subcommand)]
    pub command: PortageqCommand,
}

#[derive(winnow_args::Subcommand, Debug, Clone)]
#[arg(run_async)]
pub enum PortageqCommand {
    #[arg(
        name = "has_version",
        help = "Exit 0 if ATOM is installed under EROOT, 1 otherwise"
    )]
    HasVersion(HasVersionArgs),
    #[arg(
        name = "best_version",
        help = "Print the highest installed version matching ATOM under EROOT"
    )]
    BestVersion(BestVersionArgs),
    #[arg(
        name = "match",
        help = "List every installed CPV matching ATOM under EROOT, one per line"
    )]
    Match(MatchArgs),
    #[arg(
        name = "envvar",
        help = "Print configuration variables as a build would see them"
    )]
    Envvar(EnvvarArgs),
    #[arg(name = "vdb_path", help = "Print the installed-package database path")]
    VdbPath(VdbPathArgs),
    #[arg(name = "pkgdir", help = "Print the binary-package directory")]
    Pkgdir(PkgdirArgs),
    #[arg(name = "distdir", help = "Print the distfiles directory")]
    Distdir(DistdirArgs),
    #[arg(name = "gentoo_mirrors", help = "Print GENTOO_MIRRORS")]
    GentooMirrors(GentooMirrorsArgs),
    #[arg(name = "config_protect", help = "Print CONFIG_PROTECT")]
    ConfigProtect(ConfigProtectArgs),
    #[arg(name = "config_protect_mask", help = "Print CONFIG_PROTECT_MASK")]
    ConfigProtectMask(ConfigProtectMaskArgs),
    #[arg(name = "portdir", help = "Print the main repository path (deprecated)")]
    Portdir(PortdirArgs),
    #[arg(name = "portdir_overlay", help = "Print overlay paths (deprecated)")]
    PortdirOverlay(PortdirOverlayArgs),
    #[arg(name = "get_repos", help = "List repository names, overlays first")]
    GetRepos(GetReposArgs),
    #[arg(name = "get_repo_path", help = "Print each repository's location")]
    GetRepoPath(GetRepoPathArgs),
    #[arg(
        name = "mass_best_version",
        help = "Print `atom:cpv` for each of several atoms' best installed version"
    )]
    MassBestVersion(MassBestVersionArgs),
}

/// Exit 0 if ATOM is installed under EROOT, 1 otherwise
#[derive(winnow_args::Args, Debug, Clone)]
pub struct HasVersionArgs {
    #[arg(positional, value_name = "EROOT")]
    pub eroot: String,
    #[arg(positional, value_name = "ATOM")]
    pub atom: String,
}

/// Print the highest installed version matching ATOM under EROOT
#[derive(winnow_args::Args, Debug, Clone)]
pub struct BestVersionArgs {
    #[arg(positional, value_name = "EROOT")]
    pub eroot: String,
    #[arg(positional, value_name = "ATOM")]
    pub atom: String,
}

/// List every installed CPV matching ATOM under EROOT, one per line
#[derive(winnow_args::Args, Debug, Clone)]
pub struct MatchArgs {
    #[arg(positional, value_name = "EROOT")]
    pub eroot: String,
    /// Empty string matches every installed package
    #[arg(positional, value_name = "ATOM")]
    pub atom: String,
}

/// Print configuration variables as a build would see them
#[derive(winnow_args::Args, Debug, Clone)]
pub struct EnvvarArgs {
    /// Print `NAME=value` lines, quoted for the shell
    #[arg(short = 'v')]
    pub verbose: bool,
    /// A trailing `*` matches every variable with that prefix
    #[arg(positional, required, value_name = "NAME")]
    pub names: Vec<String>,
}

/// Print the installed-package database path
#[derive(winnow_args::Args, Debug, Clone)]
pub struct VdbPathArgs;

/// Print the binary-package directory
#[derive(winnow_args::Args, Debug, Clone)]
pub struct PkgdirArgs;

/// Print the distfiles directory
#[derive(winnow_args::Args, Debug, Clone)]
pub struct DistdirArgs;

/// Print GENTOO_MIRRORS
#[derive(winnow_args::Args, Debug, Clone)]
pub struct GentooMirrorsArgs;

/// Print CONFIG_PROTECT
#[derive(winnow_args::Args, Debug, Clone)]
pub struct ConfigProtectArgs;

/// Print CONFIG_PROTECT_MASK
#[derive(winnow_args::Args, Debug, Clone)]
pub struct ConfigProtectMaskArgs;

/// Print the main repository path (deprecated)
#[derive(winnow_args::Args, Debug, Clone)]
pub struct PortdirArgs;

/// Print overlay paths (deprecated)
#[derive(winnow_args::Args, Debug, Clone)]
pub struct PortdirOverlayArgs;

/// List repository names, overlays first
#[derive(winnow_args::Args, Debug, Clone)]
pub struct GetReposArgs {
    #[arg(positional, value_name = "EROOT")]
    pub eroot: String,
}

/// Print each repository's location
#[derive(winnow_args::Args, Debug, Clone)]
pub struct GetRepoPathArgs {
    #[arg(positional, value_name = "EROOT")]
    pub eroot: String,
    #[arg(positional, value_name = "REPO")]
    pub repos: Vec<String>,
}

/// Print `atom:cpv` for each of several atoms' best installed version
#[derive(winnow_args::Args, Debug, Clone)]
pub struct MassBestVersionArgs {
    #[arg(positional, value_name = "EROOT")]
    pub eroot: String,
    #[arg(positional, value_name = "ATOM")]
    pub atoms: Vec<String>,
}

/// `em sync` — sync repositories (git, rsync)
#[derive(winnow_args::Args, Debug, Clone)]
pub struct SyncArgs {
    /// Repo names from repos.conf (default: auto-sync enabled repos)
    #[arg(positional)]
    pub repos: Vec<String>,
    #[arg(flatten)]
    pub topology: RootedTopology,
    #[arg(flatten)]
    pub verbose_arg: VerboseArg,
    #[arg(flatten)]
    pub quiet_arg: QuietArg,
    #[arg(flatten)]
    pub pretend_arg: PretendArg,
}

/// `em depclean` — remove orphaned/unused packages
#[derive(winnow_args::Args, Debug, Clone)]
#[arg(effect = "destructive")]
pub struct DepcleanArgs {
    /// Restrict cleaning to these atoms' dependency closure (every other
    /// installed package is protected). Default: the whole `@world` set
    #[arg(positional, double_dash = "automatic")]
    pub atoms: Vec<String>,
    #[arg(flatten)]
    pub topology: RootedTopology,
    /// `--exclude`/`--with-bdeps` only — the rest of `MergeFlags` means
    /// nothing to depclean's own read-then-remove walk.
    #[arg(flatten)]
    pub merge_flags: MergeFlags,
    #[arg(flatten)]
    pub pretend_arg: PretendArg,
    #[arg(flatten)]
    pub vdb_arg: VdbArg,
}

/// `em regen` — regenerate metadata cache
#[derive(winnow_args::Args, Debug, Clone)]
pub struct RegenArgs {
    /// Repo names or paths to regenerate (default: every repo except the
    /// main one, whose cache is normally maintained upstream)
    #[arg(positional)]
    pub repos: Vec<String>,
    /// Write cache files to this directory instead of metadata/md5-cache, keeping its other
    /// entries unless `--replace` is given
    #[arg(short = 'o', long, value_name = "DIR")]
    pub output: Option<std::path::PathBuf>,
    /// Keep the cache's entries this run does not write (the default with `--output`)
    #[arg(long, conflicts("replace"))]
    pub merge: bool,
    /// Make the run's entries the whole cache, dropping those of removed ebuilds (the default
    /// without `--output`)
    #[arg(long)]
    pub replace: bool,
    /// Regenerate only these packages: their entries are replaced, removed versions included, and
    /// the rest of the cache is kept
    #[arg(long, value_name = "CAT/PKG", conflicts("replace"))]
    pub only: Vec<String>,
    /// Directory containing master repositories
    #[arg(long, value_name = "DIR")]
    pub repos_dir: Option<String>,
    /// Number of parallel workers
    #[arg(short = 'j', long)]
    pub jobs: Option<usize>,
    /// Deduplicate top-level dep tokens before writing
    #[arg(long)]
    pub dedup: bool,
    /// Activity-output flags (`--activity-fd`/`--activity-jsonl`/
    /// `--emergelog`) — `em regen` drives its own activity bus.
    #[arg(flatten)]
    pub activity: ActivityArgs,
    #[arg(flatten)]
    pub topology: RootedTopology,
    #[arg(flatten)]
    pub verbose_arg: VerboseArg,
    #[arg(flatten)]
    pub quiet_arg: QuietArg,
}

/// `em quickpkg` — create binary packages from installed files
#[derive(winnow_args::Args, Debug, Clone)]
pub struct QuickpkgArgs {
    /// Atoms, package sets (`@system`), or VDB paths (`/var/db/pkg/cat/pf`)
    #[arg(positional, required)]
    pub atoms: Vec<String>,
    /// Include CONFIG_PROTECT files
    #[arg(long)]
    pub include_config: bool,
    /// Include unmodified CONFIG_PROTECT files
    #[arg(long)]
    pub include_unmodified_config: bool,
    #[arg(flatten)]
    pub topology: RootedTopology,
    #[arg(flatten)]
    pub vdb_arg: VdbArg,
}

/// `em mirrordist` — build/maintain a distfiles mirror
#[derive(winnow_args::Args, Debug, Clone)]
#[arg(effect = "write")]
pub struct MirrorDistArgs {
    /// repos.conf name or path
    ///
    /// Defaults to the main repo (opposite default from `em regen`, which excludes it).
    #[arg(positional)]
    pub repo: Option<String>,
    /// Directory containing master repositories
    #[arg(long, value_name = "DIR")]
    pub repos_dir: Option<String>,
    /// Distfiles directory to populate
    #[arg(from_str, long, value_name = "DIR")]
    pub distfiles: camino::Utf8PathBuf,
    /// Concurrent downloads
    #[arg(short = 'j', long)]
    pub jobs: Option<usize>,
    /// Delete distfiles no longer referenced by any ebuild
    #[arg(long, effect = "destructive")]
    pub delete: bool,
    /// Grace period before an orphaned file is deleted (e.g. `7d`, `72h`)
    #[arg(long, value_name = "DURATION", default = "7d")]
    pub deletion_delay: String,
    /// Deletion-grace state file (default: `$XDG_STATE_HOME/em/mirrordist/<repo>-*.json`)
    #[arg(from_str, long, value_name = "FILE")]
    pub deletion_db: Option<camino::Utf8PathBuf>,
    /// Tab-delimited log of fetched files (appended)
    #[arg(from_str, long, value_name = "FILE")]
    pub success_log: Option<camino::Utf8PathBuf>,
    /// Tab-delimited log of fetch failures (appended)
    #[arg(from_str, long, value_name = "FILE")]
    pub failure_log: Option<camino::Utf8PathBuf>,
    /// Report of files scheduled for deletion, grouped by date (rewritten)
    #[arg(from_str, long, value_name = "FILE")]
    pub scheduled_deletion_log: Option<camino::Utf8PathBuf>,
    /// File(s) listing distfile names --delete must never remove (one
    /// name per line, `#`-comments ignored).
    #[arg(from_str, long, value_name = "FILE")]
    pub whitelist_from: Vec<camino::Utf8PathBuf>,
    /// Re-hash already-present files instead of trusting their size
    #[arg(long)]
    pub verify_existing_digest: bool,
    /// Also try GENTOO_MIRRORS after the ebuild's own URIs (real
    /// emirrordist never does this — off by default).
    #[arg(long)]
    pub gentoo_mirrors_fallback: bool,
    /// Allow --delete even when some ebuilds had no metadata cache entry
    #[arg(long)]
    pub delete_allow_incomplete: bool,
    #[arg(flatten)]
    pub topology: RootedTopology,
    #[arg(flatten)]
    pub pretend_arg: PretendArg,
}

/// `em query` — query package information
#[derive(winnow_args::Args, Debug, Clone)]
#[arg(
    flatten_help,
    effect = "read",
    example = "em query depgraph zlib",
    example = "em query belongs /usr/bin/python",
    example = "em query list -I"
)]
pub struct QueryArgs {
    #[arg(subcommand)]
    pub command: QueryCommand,
    #[arg(flatten)]
    pub topology: RootedTopology,
    #[arg(flatten)]
    pub verbose_arg: VerboseArg,
    #[arg(flatten)]
    pub arch_arg: ArchArg,
    #[arg(flatten)]
    pub repo_arg: RepoArg,
    #[arg(flatten)]
    pub vdb_arg: VdbArg,
}

/// `em clean` — clean distfiles and/or binary packages
#[derive(winnow_args::Args, Debug, Clone)]
#[arg(flatten_help, effect = "destructive")]
pub struct CleanArgs {
    #[arg(subcommand)]
    pub target: CleanTarget,
    #[arg(flatten)]
    pub topology: RootedTopology,
    #[arg(flatten)]
    pub pretend_arg: PretendArg,
}

/// `em use` — enable/disable/query USE flags in make.conf
#[derive(winnow_args::Args, Debug, Clone)]
pub struct UseArgs {
    /// Add (enable) flags — euse calls this --enable/-E
    #[arg(short = 'E', short = 'a', long = "add", value_name = "FLAG")]
    pub add: Vec<String>,
    /// Subtract flags (written with leading '-', e.g. -themes) — euse
    /// calls this --disable/-D
    #[arg(short = 'D', short = 's', long = "subtract", value_name = "FLAG")]
    pub subtract: Vec<String>,
    /// Drop flags entirely (removes both flag and -flag forms) — euse
    /// calls this --remove/-R or --prune/-P
    #[arg(
        short = 'R',
        short = 'P',
        short = 'd',
        long = "drop",
        value_name = "FLAG"
    )]
    pub drop: Vec<String>,
    /// Preview the resulting value without writing make.conf
    #[arg(short = 'n', long = "dry-run")]
    pub dry_run: bool,
    /// Target a USE_EXPAND variable (e.g. VIDEO_CARDS) instead of USE —
    /// -a/-s/-d then edit that variable's value the same way
    #[arg(short = 'e', long = "expand", value_name = "VAR")]
    pub expand: Option<String>,
    /// List every USE_EXPAND variable known to the active profile, each
    /// with its current make.conf value
    #[arg(
        short = 'L',
        long = "list-expand",
        conflicts("add", "subtract", "drop", "expand")
    )]
    pub list_expand: bool,
    /// Show descriptions for the given USE flags (profiles/use.desc and
    /// use.local.desc, searching both unless -g/-l restricts it). With
    /// no flags given, lists every flag in scope
    #[arg(
        short = 'i',
        long = "info",
        value_name = "FLAG",
        conflicts("add", "subtract", "drop", "expand", "list_expand")
    )]
    pub info: Vec<String>,
    /// Restrict -i to global flags only (profiles/use.desc)
    #[arg(
        short = 'g',
        long = "global",
        conflicts("add", "subtract", "drop", "expand", "list_expand", "local_desc")
    )]
    pub global: bool,
    /// Restrict -i to per-package local flags only (profiles/use.local.desc,
    /// searched across every package — see `em query uses <atom>` for a
    /// single package's flags instead)
    #[arg(
        short = 'l',
        long = "local-desc",
        conflicts("add", "subtract", "drop", "expand", "list_expand", "global")
    )]
    pub local_desc: bool,
    /// Path to make.conf (default: resolved like other config commands,
    /// following --config-root/--local/--prefix)
    #[arg(from_str, long = "make-conf", value_name = "PATH")]
    pub make_conf: Option<camino::Utf8PathBuf>,
    #[arg(flatten)]
    pub topology: RootedTopology,
}

/// `em pkg` — edit per-package configuration
#[derive(winnow_args::Args, Debug, Clone)]
pub struct PkgArgs {
    #[arg(subcommand)]
    pub command: PkgCommand,
    #[arg(flatten)]
    pub topology: RootedTopology,
    #[arg(flatten)]
    pub arch_arg: ArchArg,
}

/// `em revdep` — rebuild packages with broken shared library deps
#[derive(winnow_args::Args, Debug, Clone)]
pub struct RevdepArgs {
    /// Only consider consumers of libraries whose soname contains NAME
    #[arg(short = 'L', long, value_name = "NAME")]
    pub library: Option<String>,
    #[arg(flatten)]
    pub topology: RootedTopology,
    #[arg(flatten)]
    pub merge_flags: MergeFlags,
    #[arg(flatten)]
    pub pretend_arg: PretendArg,
    #[arg(flatten)]
    pub vdb_arg: VdbArg,
}

/// `em read` — display Portage elog files
#[derive(winnow_args::Args, Debug, Clone)]
pub struct ReadArgs {
    /// Only show packages whose `<category>/<pf>` contains this text
    #[arg(positional)]
    pub package: Option<String>,
    /// List what is filed instead of printing the messages
    #[arg(short, long)]
    pub list: bool,
    /// Show only this many of the most recent packages; 0 for all
    #[arg(short = 'n', long, default = "10")]
    pub limit: usize,
    /// Remove each file once it has been shown
    #[arg(long)]
    pub delete: bool,
    #[arg(flatten)]
    pub topology: RootedTopology,
}

/// `em log` — analyze emerge.log
#[derive(winnow_args::Args, Debug, Clone)]
#[arg(effect = "read")]
pub struct LogArgs {
    #[arg(subcommand)]
    pub command: Option<LogCommand>,
    #[arg(flatten)]
    pub topology: RootedTopology,
}

/// `em grep` — search inside ebuilds and eclasses (qgrep workalike)
#[derive(winnow_args::Args, Debug, Clone)]
#[arg(
    effect = "read",
    example = "em grep PYTHON_COMPAT dev-python/setuptools"
)]
pub struct GrepArgs {
    /// Pattern to search for (a literal substring unless -e/-x)
    #[arg(positional)]
    pub pattern: String,

    /// Restrict the search to these atoms (`cat/pkg`, `cat/`, a bare package
    /// name, or a full dependency atom like `>=cat/pkg-1.2`); default: every
    /// package in scope
    #[arg(positional, double_dash = "automatic")]
    pub targets: Vec<String>,

    /// Invert the match: print lines that do NOT match
    #[arg(short = 'I', long = "invert-match")]
    pub invert_match: bool,

    /// Case-insensitive match
    #[arg(short = 'i', long = "ignore-case")]
    pub ignore_case: bool,

    /// Label matches with `cat/pkg-ver` instead of the ebuild's path
    #[arg(short = 'N', long = "atom-name")]
    pub atom_name: bool,

    /// Print each matching file's label and its match count instead of the
    /// matching lines
    #[arg(short = 'c', long = "count", conflicts("list", "list_invert"))]
    pub count: bool,

    /// Print only the labels of files that match
    #[arg(short = 'l', long = "list", conflicts("count", "list_invert"))]
    pub list: bool,

    /// Print only the labels of files that do NOT match
    #[arg(short = 'L', long = "invert-list", conflicts("count", "list"))]
    pub list_invert: bool,

    /// Treat PATTERN (and -S) as a regular expression instead of a literal
    /// substring
    #[arg(short = 'e', long = "regexp", short = 'x', long = "extended")]
    pub regexp: bool,

    /// Search installed packages' own ebuild copies (under
    /// --root/--prefix/--local) instead of the repo tree
    #[arg(short = 'J', long = "installed")]
    pub installed: bool,

    /// Search eclasses instead of ebuilds (targets are ignored)
    #[arg(short = 'E', long = "eclass")]
    pub eclass: bool,

    /// Skip lines that are comments (first non-blank character is `#`)
    #[arg(short = 's', long = "skip-comments")]
    pub skip_comments: bool,

    /// Label matches with `cat/pkg-ver::repo` instead of a path; implies -N
    #[arg(short = 'R', long = "show-repo")]
    pub show_repo: bool,

    /// Also skip lines matching this pattern (same literal/regex/case mode
    /// as PATTERN)
    #[arg(short = 'S', long = "skip", value_name = "PATTERN")]
    pub skip: Option<String>,

    /// Print N lines of context before each match
    #[arg(short = 'B', long = "before", value_name = "N")]
    pub before: Option<usize>,

    /// Print N lines of context after each match
    #[arg(short = 'A', long = "after", value_name = "N")]
    pub after: Option<usize>,

    /// Print bare matching lines with no label
    #[arg(short = 'q', long = "no-filename")]
    pub no_filename: bool,

    #[arg(flatten)]
    pub topology: RootedTopology,
    #[arg(flatten)]
    pub repo_arg: RepoArg,
    #[arg(flatten)]
    pub verbose_arg: VerboseArg,
}

/// `em search` — search package names and descriptions
#[derive(winnow_args::Args, Debug, Clone)]
#[arg(effect = "read", example = "em search firefox")]
pub struct SearchArgs {
    /// List all packages (no pattern required)
    #[arg(short = 'a', long)]
    pub all: bool,
    /// Search package descriptions instead of names
    #[arg(short = 'S', long = "desc")]
    pub desc: bool,
    /// Show only package name, no description
    #[arg(short = 'N', long = "name-only")]
    pub name_only: bool,
    /// Show homepage instead of description
    #[arg(short = 'H', long)]
    pub homepage: bool,
    /// Pattern to search (required unless --all)
    #[arg(positional, required_unless = "all")]
    pub pattern: Option<String>,
    #[arg(flatten)]
    pub topology: RootedTopology,
    #[arg(flatten)]
    pub repo_arg: RepoArg,
}

/// `em atom` — parse/split atom strings
#[derive(winnow_args::Args, Debug, Clone)]
#[arg(effect = "read", example = "em atom >=dev-lang/python-3.10")]
pub struct AtomArgs {
    /// Atom strings to parse and print back in normalized form
    #[arg(positional, required)]
    pub atoms: Vec<String>,
}

/// `em select` — native config selectors (profile, repos)
#[derive(winnow_args::Args, Debug, Clone)]
#[arg(flatten_help)]
pub struct SelectArgs {
    #[arg(subcommand)]
    pub command: SelectCommand,
    #[arg(flatten)]
    pub topology: RootedTopology,
    #[arg(flatten)]
    pub arch_arg: ArchArg,
    #[arg(flatten)]
    pub repo_arg: RepoArg,
    #[arg(flatten)]
    pub pretend_arg: PretendArg,
    #[arg(flatten)]
    pub vdb_arg: VdbArg,
}

/// `em active` — register a default `--prefix`/`--local` for bare invocations
#[derive(winnow_args::Args, Debug, Clone)]
#[arg(
    example("em active set --local=", header = "Register ~/.gentoo"),
    example("em active --local= set", header = "Flags may precede set")
)]
pub struct ActiveArgs {
    #[arg(subcommand)]
    pub command: Option<ActiveCommand>,

    /// No `--root` — active is for unprivileged prefix/local dogfooding
    /// only (see `resolve_set_target` in active.rs).
    #[arg(flatten)]
    pub topology: Topology,
}

/// `em etc` — reconcile pending config files
#[derive(winnow_args::Args, Debug, Clone)]
pub struct EtcArgs {
    #[arg(subcommand)]
    pub command: Option<EtcCommand>,
    #[arg(flatten)]
    pub opts: EtcOpts,
    #[arg(flatten)]
    pub topology: RootedTopology,
    #[arg(flatten)]
    pub quiet_arg: QuietArg,
    #[arg(flatten)]
    pub pretend_arg: PretendArg,
}

/// `em env` — regenerate `/etc/profile.env` and ld.so cache
#[derive(winnow_args::Args, Debug, Clone)]
pub struct EnvArgs {
    #[arg(flatten)]
    pub topology: RootedTopology,
}

/// `em completion` — print a shell completion script for `em`.
#[derive(winnow_args::Args, Debug, Clone)]
pub struct CompletionArgs {
    /// Shell to generate the script for
    #[arg(
        positional,
        value_name = "SHELL",
        choices("bash", "zsh", "fish", "powershell", "elvish")
    )]
    pub shell: String,
}

/// `em setup` — bootstrap a prefix layout
#[derive(winnow_args::Args, Default, Debug, Clone)]
#[arg(
    effect = "write",
    example("em setup --local", header = "Bootstrap ~/.gentoo")
)]
pub struct SetupArgs {
    /// Directory holding host tools this prefix should borrow while it has
    /// none of its own, put ahead of the sanitised build `PATH`. Repeatable.
    ///
    /// `--local` only, and only for the setup itself: builds sanitise `$HOME`
    /// and `/usr/local` off `PATH` so a local install cannot shadow the
    /// Gentoo toolchain, which also hides a hand-installed GNU sed/grep from
    /// the very first merges. `em setup --local` finds the usual locations by
    /// itself; this is for anywhere else you keep them.
    #[arg(from_str, long, value_name = "DIR")]
    pub extra_path: Vec<camino::Utf8PathBuf>,

    #[arg(flatten)]
    pub topology: RootedTopology,

    #[arg(flatten)]
    pub merge_flags: MergeFlags,

    #[arg(flatten)]
    pub depgraph_flags: DepgraphFlags,

    #[arg(flatten)]
    pub activity: ActivityArgs,

    #[arg(flatten)]
    pub verbose_arg: VerboseArg,

    #[arg(flatten)]
    pub quiet_arg: QuietArg,

    #[arg(flatten)]
    pub arch_arg: ArchArg,

    #[arg(flatten)]
    pub repo_arg: RepoArg,

    #[arg(flatten)]
    pub pretend_arg: PretendArg,

    /// Privilege backend for this setup run
    #[arg(long, default = "auto", help_heading = "Merge")]
    pub privilege: Privilege,
}

/// `em crossdev` — cross-target setup, mirroring crossdev's option surface (the
/// no-build subset for now; building the toolchain is future work).
///
/// `--root` is not accepted by `crossdev` — the cross toolchain always
/// installs into `/usr/<tuple>` (or the target sysroot under `--target`);
/// there is no separate destination to redirect.
#[derive(winnow_args::Args, Debug, Clone)]
pub struct CrossdevArgs {
    /// Use the LLVM/Clang model (`cross_llvm-*`: host clang cross-targets, no per-target
    /// compiler)
    ///
    /// Rejects glibc — use musl or a bare-metal target.
    #[arg(short = 'L', long)]
    pub llvm: bool,

    /// Lay down the overlay + sysroot config without building anything
    #[arg(long)]
    pub init_target: bool,

    /// Bootstrap the cross toolchain into the prefix (`/usr/<tuple>`): the full
    /// intertwined sequence (binutils → headers → gcc-stage1 → libc →
    /// gcc-stage2). Implies `--init-target`.
    #[arg(long)]
    pub setup: bool,

    /// With `--setup`, skip the bootstrap steps an earlier run completed: steps recorded with
    /// the same spec whose packages are still installed. The first step that does not hold
    /// runs again, and every step after it.
    #[arg(short = 'r', long)]
    pub resume: bool,

    /// Print the derived target configuration and exit (no writes)
    #[arg(long)]
    pub show_target_cfg: bool,

    /// Print the target's built-in sysroot spec as TOML and exit (no writes)
    #[arg(long)]
    pub print_spec: bool,

    /// Build an extra package onto the established cross target (may be given multiple times)
    ///
    /// `CATEGORY/PN` — always runs on the host (like `binutils`/`gcc`), not the target sysroot,
    /// matching real crossdev's `--ex-pkg`.
    ///
    /// Applies to `--init-target`/`--setup` only; named per invocation,
    /// like real crossdev — not remembered across a later run that omits it.
    #[arg(long, value_name = "CATEGORY/PN")]
    pub ex_pkg: Vec<String>,

    /// Build a cross gdb (`dev-debug/gdb`) — shorthand for `--ex-pkg
    /// dev-debug/gdb`, crossdev's own `--ex-gdb`.
    #[arg(long)]
    pub ex_gdb: bool,

    #[arg(flatten)]
    pub merge_flags: MergeFlags,

    #[arg(flatten)]
    pub depgraph_flags: DepgraphFlags,

    #[arg(flatten)]
    pub activity: ActivityArgs,

    #[arg(flatten)]
    pub verbose_arg: VerboseArg,

    #[arg(flatten)]
    pub quiet_arg: QuietArg,

    #[arg(flatten)]
    pub arch_arg: ArchArg,

    #[arg(flatten)]
    pub repo_arg: RepoArg,

    #[arg(flatten)]
    pub pretend_arg: PretendArg,

    /// No `--root` — see `Topology`'s own doc for why this needs the bare
    /// (root-less) mixin instead of `RootedTopology`.
    #[arg(flatten)]
    pub topology: Topology,

    /// Privilege backend for this crossdev run
    #[arg(long, default = "auto", help_heading = "Merge")]
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
#[derive(winnow_args::Args, Debug, Clone)]
#[arg(
    effect = "write",
    example(
        "em toolchain --prefix /p --setup",
        header = "Bootstrap a prefix toolchain"
    )
)]
pub struct ToolchainArgs {
    /// Build and install the toolchain into `--root` (the only action for now;
    /// required, mirroring `crossdev --setup`).
    #[arg(long)]
    pub setup: bool,

    #[arg(flatten)]
    pub topology: RootedTopology,

    #[arg(flatten)]
    pub merge_flags: MergeFlags,

    #[arg(flatten)]
    pub depgraph_flags: DepgraphFlags,

    #[arg(flatten)]
    pub activity: ActivityArgs,

    #[arg(flatten)]
    pub verbose_arg: VerboseArg,

    #[arg(flatten)]
    pub quiet_arg: QuietArg,

    #[arg(flatten)]
    pub arch_arg: ArchArg,

    #[arg(flatten)]
    pub repo_arg: RepoArg,

    #[arg(flatten)]
    pub pretend_arg: PretendArg,

    /// Privilege backend for this toolchain run
    #[arg(long, default = "auto", help_heading = "Merge")]
    pub privilege: Privilege,
}

// `em stages` — assemble stage-build artifacts (stage1/stage3/stage4) *using*
// a toolchain already built by `em toolchain --setup`.
#[derive(winnow_args::Args, Debug, Clone)]
#[arg(
    effect = "write",
    example(
        "em stages --root /var/tmp/stage1 --stage1",
        header = "Stage1 into a root"
    )
)]
pub struct StagesArgs {
    /// Emerge the profile's `packages.build` bootstrap set into `--root`:
    /// baselayout (USE=build, --nodeps) then the minimal stage1 package list
    /// (USE="-* build"), mirroring catalyst's `stage1/chroot.sh`. Requires a
    /// working toolchain already in the root (`em toolchain --setup`).
    #[arg(long)]
    pub stage1: bool,

    /// Emptytree rebuild of `@system` into `--root` (catalyst `stage3/chroot.sh`:
    /// `emerge -e --update --deep --with-bdeps=y @system`). Forces `-e -uD
    /// --with-bdeps y` on top of other merge flags; seeds PKGDIR with `-b` like
    /// stage1. No stage2 (crossdev model). Requires a usable root (typically
    /// after `--stage1` or an unpacked seed).
    #[arg(long)]
    pub stage3: bool,

    #[arg(flatten)]
    pub topology: RootedTopology,

    #[arg(flatten)]
    pub merge_flags: MergeFlags,

    #[arg(flatten)]
    pub depgraph_flags: DepgraphFlags,

    #[arg(flatten)]
    pub activity: ActivityArgs,

    #[arg(flatten)]
    pub verbose_arg: VerboseArg,

    #[arg(flatten)]
    pub quiet_arg: QuietArg,

    #[arg(flatten)]
    pub arch_arg: ArchArg,

    #[arg(flatten)]
    pub repo_arg: RepoArg,

    #[arg(flatten)]
    pub pretend_arg: PretendArg,

    /// Privilege backend for this stages run
    #[arg(long, default = "auto", help_heading = "Merge")]
    pub privilege: Privilege,
}

/// `em emerge` — resolve and merge/unmerge packages (real emerge workalike)
///
/// The explicit, self-contained form of the bare `em <atoms>` path — see
/// [`Applet::Emerge`]'s doc comment.
#[derive(winnow_args::Args, Debug, Clone, Default)]
#[arg(
    effect = "write",
    example = "em emerge firefox",
    output(
        "json",
        framing = "json",
        help = "Machine-parsable JSON (`--json` with `-p` or `--info`)"
    )
)]
pub struct EmergeArgs {
    #[arg(flatten)]
    pub mode: EmergeModeArgs,

    #[arg(flatten)]
    pub merge_flags: MergeFlags,

    #[arg(flatten)]
    pub depgraph_flags: DepgraphFlags,

    #[arg(flatten)]
    pub topology: RootedTopology,

    #[arg(flatten)]
    pub activity: ActivityArgs,

    #[arg(flatten)]
    pub verbose_arg: VerboseArg,

    #[arg(flatten)]
    pub quiet_arg: QuietArg,

    #[arg(flatten)]
    pub arch_arg: ArchArg,

    #[arg(flatten)]
    pub repo_arg: RepoArg,

    #[arg(flatten)]
    pub pretend_arg: PretendArg,

    #[arg(flatten)]
    pub vdb_arg: VdbArg,

    /// Privilege backend for this merge
    #[arg(long, default = "auto", help_heading = "Merge")]
    pub privilege: Privilege,

    /// Atoms, package sets (`@world`), or ebuild paths to act on
    #[arg(positional, value_name = "ATOM")]
    pub atoms: Vec<String>,
}

#[derive(winnow_args::Subcommand, Debug, Clone)]
pub enum MaintCommand {
    #[arg(help = "Generate binary package metadata index")]
    Binhost,
    #[arg(help = "Inspect/verify/prune local binary packages (em-only, no emaint equivalent)")]
    Binpkg {
        #[arg(subcommand)]
        action: BinpkgAction,
    },
    #[arg(help = "No-op: em keeps no config-memory file to go stale")]
    Cleanconfmem,
    #[arg(help = "Discard saved resume lists")]
    Cleanresume {
        /// Actually delete the saved resume/resume-backup lists (default:
        /// just report what's there).
        #[arg(short, long)]
        fix: bool,
    },
    #[arg(help = "Prune the build.log files finished merges leave in the build tree")]
    Logs {
        /// Remove them; without this the logs are only listed
        #[arg(long)]
        fix: bool,
        /// Only consider logs at least this old (e.g. `30d`, `2weeks`)
        #[arg(short = 't', long, value_name = "AGE")]
        older_than: Option<String>,
    },
    #[arg(help = "Unavailable: em keeps no failed-merge registry")]
    Merges,
    #[arg(help = "Apply package moves to binary packages")]
    Movebin,
    #[arg(help = "Apply package moves to installed packages")]
    Moveinst,
    #[arg(help = "Regenerate profiles/use.local.desc from metadata.xml")]
    RegenUse {
        /// Write output here instead of profiles/use.local.desc ('-' for stdout)
        #[arg(short, long, value_name = "PATH")]
        output: Option<String>,
    },
    #[arg(help = "Purge repo revision history from repo_revisions")]
    Revisions {
        /// Purge only these repos (default: all)
        #[arg(positional, value_name = "REPO")]
        repos: Vec<String>,
    },
    /// Same as `em sync` — shared implementation
    #[arg(help = "Sync repositories (git, rsync)")]
    Sync {
        /// Repo names from repos.conf (default: auto-sync enabled repos)
        #[arg(positional)]
        repos: Vec<String>,
    },
    #[arg(help = "Check (and optionally fix) problems in the world file")]
    World {
        /// Remove orphaned entries from the world file
        #[arg(short, long)]
        fix: bool,
    },
}

/// Inspect, verify and prune the binary packages in the local `PKGDIR`
//
// the doc comment above is this subcommand's help, so the
// rationale stays a plain comment: there is no real-portage `emaint` module
// for this (only `emaint binhost`, which just regenerates the index) — it is
// an em-only extension, built on the `Packages` index/reader substrate.
#[derive(winnow_args::Subcommand, Debug, Clone)]
pub enum BinpkgAction {
    #[arg(help = "Check each indexed binpkg's size/MD5/SHA1 against the file on disk")]
    Verify {
        /// Quarantine corrupt containers (rename to `.corrupt`) and drop
        /// missing/corrupt entries from the index by regenerating it.
        #[arg(long)]
        fix: bool,
        /// Reject a container with no OpenPGP signature at all (matches
        /// FEATURES=binpkg-request-signature); with a verify keyring
        /// present (`em maint binpkg gpg-import`), signatures are always
        /// cryptographically checked regardless of this flag.
        #[arg(long)]
        require_signature: bool,
    },
    #[arg(help = "List indexed binary packages (cpv, build-id, size, path)")]
    List,
    #[arg(help = "Keep only the newest BUILD_ID per package, deleting older ones")]
    Prune {
        /// Report what would be deleted without deleting or reindexing
        #[arg(long)]
        dry_run: bool,
    },
    #[arg(help = "Print the build-env key for the current roots' make.conf flags")]
    Fingerprint {
        /// Print the full key (space-joined sokgi hashes) instead of the
        /// short path-safe slug.
        #[arg(long)]
        full: bool,
        /// Fingerprint the host (BROOT) config instead of the target roots
        /// (only differs under --target).
        #[arg(long)]
        host: bool,
    },
    #[arg(help = "Import an armored OpenPGP public key into the GPG verify keyring")]
    GpgImport {
        /// Path to an armored public-key file (e.g. exported via
        /// `gpg --armor --export <key-id>`).
        #[arg(from_str, positional)]
        keyfile: camino::Utf8PathBuf,
    },
}

/// `em select <module>` — native, eselect-like config selectors
#[derive(winnow_args::Subcommand, Debug, Clone)]
pub enum SelectCommand {
    #[arg(help = "Select the system/sysroot profile (cross-aware)")]
    Profile {
        #[arg(subcommand)]
        action: ProfileAction,
    },
    #[arg(alias = "repos", help = "Manage local repositories (overlays)")]
    Repository {
        #[arg(subcommand)]
        action: RepositoryAction,
    },
    #[arg(
        alias = "gcc",
        help = "Select the active compiler profile (gcc-config/eselect gcc workalike)"
    )]
    Compiler {
        #[arg(subcommand)]
        action: CompilerAction,
    },
    #[arg(help = "Select the active binutils profile (binutils-config/eselect binutils workalike)")]
    Binutils {
        #[arg(subcommand)]
        action: BinutilsAction,
    },
    #[arg(help = "Select the active linker profile")]
    Linker {
        #[arg(subcommand)]
        action: LinkerAction,
    },
    #[arg(help = "Select the active LLVM/clang slot")]
    Clang {
        #[arg(subcommand)]
        action: ClangAction,
    },
    #[arg(help = "Select the pkg-config backend and create the <CTARGET>-pkg-config wrapper")]
    Pkgconf {
        #[arg(subcommand)]
        action: PkgconfAction,
    },
    #[arg(
        alias = "mirror",
        help = "Manage Gentoo distfile mirrors (mirrorselect workalike)"
    )]
    Mirrors {
        #[arg(subcommand)]
        action: MirrorAction,
    },
    #[arg(help = "Read/manage GLEP 42 news items (eselect news workalike)")]
    News {
        #[arg(subcommand)]
        command: Option<NewsCommand>,
    },
    #[arg(help = "Check/fix Gentoo Linux Security Advisories (glsa-check workalike)")]
    Glsa {
        #[arg(subcommand)]
        command: Option<GlsaCommand>,
    },
}

/// `em select profile <action>`
#[derive(winnow_args::Subcommand, Debug, Clone)]
pub enum ProfileAction {
    #[arg(help = "List available profiles (marks the current one)")]
    List,
    #[arg(help = "Show the current profile")]
    Show,
    #[arg(help = "Set the profile by list number or path (cross-aware: no arch check)")]
    Set {
        /// Profile list number (from `list`) or path (e.g. `default/linux/riscv/23.0/rv64/lp64d`)
        #[arg(positional)]
        target: String,
    },
}

/// `em select repository <action>` — local repos only (remote sync is a TODO)
#[derive(winnow_args::Subcommand, Debug, Clone)]
pub enum RepositoryAction {
    #[arg(help = "List configured repositories")]
    List,
    #[arg(help = "Register an existing local repository")]
    Add {
        /// Repository name
        #[arg(positional)]
        name: String,
        /// Existing local path to the repository
        #[arg(positional)]
        location: String,
    },
    #[arg(alias = "rm", help = "Remove a repository's repos.conf entry")]
    Remove {
        /// Repository name
        #[arg(positional)]
        name: String,
    },
    #[arg(help = "Create a new local overlay (skeleton + repos.conf entry)")]
    Create {
        /// Repository name
        #[arg(positional)]
        name: String,
        /// Location (default: `<config-root>/var/db/repos/<name>`)
        #[arg(positional)]
        location: Option<String>,
    },
}

/// `em select compiler <action>` — gcc-config workalike
#[derive(winnow_args::Subcommand, Debug, Clone)]
pub enum CompilerAction {
    #[arg(help = "List available compiler profiles")]
    List {
        /// Target tuple (CTARGET) to list profiles for
        #[arg(short, long)]
        target: Option<String>,
    },
    #[arg(help = "Show the current compiler profile")]
    Show {
        /// Target tuple (CTARGET) to show profile for
        #[arg(short, long)]
        target: Option<String>,
    },
    #[arg(help = "Set the active compiler profile")]
    Set {
        /// Compiler profile to activate (e.g., `riscv64-unknown-linux-gnu-16` or `1` for list number)
        #[arg(positional)]
        profile: String,
        /// Target tuple (CTARGET) for cross-compiler selection
        #[arg(short, long)]
        target: Option<String>,
    },
}

/// `em select binutils <action>` — binutils-config workalike
#[derive(winnow_args::Subcommand, Debug, Clone)]
pub enum BinutilsAction {
    #[arg(help = "List available binutils profiles")]
    List {
        /// Target tuple (CTARGET) to list profiles for
        #[arg(short, long)]
        target: Option<String>,
    },
    #[arg(help = "Show the current binutils profile")]
    Show {
        /// Target tuple (CTARGET) to show profile for
        #[arg(short, long)]
        target: Option<String>,
    },
    #[arg(help = "Set the active binutils profile")]
    Set {
        /// Binutils profile to activate (e.g., `riscv64-unknown-linux-gnu-2.46.0` or `1` for list number)
        #[arg(positional)]
        profile: String,
        /// Target tuple (CTARGET) for cross-binutils selection
        #[arg(short, long)]
        target: Option<String>,
    },
}

/// `em select linker <action>` — linker profile selection
#[derive(winnow_args::Subcommand, Debug, Clone)]
pub enum LinkerAction {
    #[arg(help = "List available linker profiles")]
    List {
        /// Target tuple (CTARGET) to list profiles for
        #[arg(short, long)]
        target: Option<String>,
    },
    #[arg(help = "Show the current linker profile")]
    Show {
        /// Target tuple (CTARGET) to show profile for
        #[arg(short, long)]
        target: Option<String>,
    },
    #[arg(help = "Set the active linker profile")]
    Set {
        /// Linker profile to activate (e.g., `riscv64-unknown-linux-gnu-lld-18` or `1` for list number)
        #[arg(positional)]
        profile: String,
        /// Target tuple (CTARGET) for cross-linker selection
        #[arg(short, long)]
        target: Option<String>,
    },
}

/// `em select clang <action>` — LLVM/clang slot selection
#[derive(winnow_args::Subcommand, Debug, Clone)]
pub enum ClangAction {
    #[arg(help = "List available LLVM/clang slots")]
    List,
    #[arg(help = "Show the current LLVM/clang slot")]
    Show,
    #[arg(help = "Set the active LLVM/clang slot")]
    Set {
        /// LLVM slot to activate: a slot (`22`), a slot qualified by where it
        /// lives (`22@host`, `22@prefix`), or a `list` number. A bare slot present
        /// in both resolves to the prefix's..
        #[arg(positional)]
        slot: String,
    },
}

/// `em select pkgconf <action>` — picks the `pkg-config`/`pkgconf` backend
/// and creates the `<CTARGET>-pkg-config` wrapper real crossdev provides but
/// `em` otherwise never builds (`toolchain-funcs.eclass`'s `tc-getPKG_CONFIG`
/// searches `$PATH` for exactly this name).
#[derive(winnow_args::Subcommand, Debug, Clone)]
pub enum PkgconfAction {
    #[arg(help = "List available pkg-config backends (pkgconf, pkg-config)")]
    List {
        /// Target tuple (CTARGET) to show the wrapper for
        #[arg(short, long)]
        target: Option<String>,
    },
    #[arg(help = "Show the backend the <target>-pkg-config wrapper currently points at")]
    Show {
        /// Target tuple (CTARGET) to show the wrapper for
        #[arg(short, long)]
        target: Option<String>,
    },
    #[arg(help = "Create/update the <target>-pkg-config wrapper")]
    Set {
        /// Backend to wrap (`pkgconf`, `pkg-config`, or a list number from `list`)
        #[arg(positional)]
        backend: String,
        /// Target tuple (CTARGET) to create the wrapper for
        #[arg(short, long)]
        target: Option<String>,
    },
}

/// `em select mirrors <action>` — mirrorselect workalike for `GENTOO_MIRRORS`
#[derive(winnow_args::Subcommand, Debug, Clone)]
pub enum MirrorAction {
    /// List available Gentoo distfile mirrors (marks those already selected)
    List {
        /// Keep only mirrors in this ISO country code (e.g. `US`, `DE`)
        #[arg(short, long)]
        country: Option<String>,
        /// Keep only mirrors in this region (e.g. `Europe`, `North America`)
        #[arg(short, long)]
        region: Option<String>,
    },
    /// Show the currently configured `GENTOO_MIRRORS` value
    Show,
    /// Set `GENTOO_MIRRORS`
    Set {
        /// Explicit mirror URLs to use
        ///
        /// If omitted, mirrors are picked from `--country`/`--region` instead.
        #[arg(positional, value_name = "URL")]
        urls: Vec<String>,
        /// Use every mirror in this ISO country code
        #[arg(short, long)]
        country: Option<String>,
        /// Use every mirror in this region
        #[arg(short, long)]
        region: Option<String>,
    },
}

#[derive(winnow_args::Subcommand, Debug, Clone)]
pub enum PkgCommand {
    #[arg(help = "Edit per-package USE flags in package.use")]
    Use {
        /// Package atom (e.g. sys-boot/grub or >=dev-libs/foo-1.0)
        #[arg(positional)]
        atom: String,
        /// Add flags (written verbatim, e.g. truetype) — euse calls this
        /// --enable/-E
        #[arg(short = 'E', short = 'a', long = "add", value_name = "FLAG")]
        add: Vec<String>,
        /// Subtract flags (written with leading '-', e.g. -themes) — euse
        /// calls this --disable/-D
        #[arg(short = 'D', short = 's', long = "subtract", value_name = "FLAG")]
        subtract: Vec<String>,
        /// Drop flags entirely (removes both flag and -flag forms) — euse
        /// calls this --remove/-R or --prune/-P
        #[arg(
            short = 'R',
            short = 'P',
            short = 'd',
            long = "drop",
            value_name = "FLAG"
        )]
        drop: Vec<String>,
        /// Preview the resulting entry without writing package.use
        #[arg(short = 'n', long = "dry-run")]
        dry_run: bool,
        /// Show descriptions for the given USE flags on this package
        /// (metadata.xml/use.local.desc first, falling back to the global
        /// profiles/use.desc)
        #[arg(
            short = 'i',
            long = "info",
            value_name = "FLAG",
            conflicts("add", "subtract", "drop")
        )]
        info: Vec<String>,
        /// Target file inside package.use/ (default: `<cat>-<pkg>`)
        #[arg(from_str, long, value_name = "FILE")]
        path: Option<camino::Utf8PathBuf>,
    },
    #[arg(help = "Edit per-package keywords in package.accept_keywords")]
    Keyword {
        /// Package atom (e.g. sys-boot/grub or >=dev-libs/foo-1.0)
        #[arg(positional)]
        atom: String,
        /// Add keyword tokens (e.g. `~amd64`, `-*`)
        #[arg(short = 'a', long = "add", value_name = "KW")]
        add: Vec<String>,
        /// Subtract keyword tokens (written with leading '-', e.g. `-~amd64`)
        #[arg(short = 's', long = "subtract", value_name = "KW")]
        subtract: Vec<String>,
        /// Drop keyword tokens entirely (removes both the token and its negated form)
        #[arg(short = 'd', long = "drop", value_name = "KW")]
        drop: Vec<String>,
        /// Target file inside package.accept_keywords/ (default: `<cat>-<pkg>`)
        #[arg(from_str, long, value_name = "FILE")]
        path: Option<camino::Utf8PathBuf>,
    },
    #[arg(help = "Add/remove a package from package.mask")]
    Mask {
        /// Package atom (e.g. sys-boot/grub or >=dev-libs/foo-1.0)
        #[arg(positional)]
        atom: String,
        /// Add the atom to package.mask
        #[arg(short = 'a', long = "add")]
        add: bool,
        /// Remove the atom from package.mask
        #[arg(short = 'd', long = "drop")]
        drop: bool,
        /// Target file inside package.mask/ (default: `<cat>-<pkg>`)
        #[arg(from_str, long, value_name = "FILE")]
        path: Option<camino::Utf8PathBuf>,
    },
    #[arg(help = "Edit per-package env files in package.env")]
    Env {
        /// Package atom (e.g. sys-boot/grub or >=dev-libs/foo-1.0)
        #[arg(positional)]
        atom: String,
        /// Add env file name(s) (from `/etc/portage/env/`) to apply to this package
        #[arg(short = 'a', long = "add", value_name = "ENVFILE")]
        add: Vec<String>,
        /// Drop env file name(s) from this package's entry
        #[arg(short = 'd', long = "drop", value_name = "ENVFILE")]
        drop: Vec<String>,
        /// Target file inside package.env/ (default: `<cat>-<pkg>`)
        #[arg(from_str, long, value_name = "FILE")]
        path: Option<camino::Utf8PathBuf>,
    },
}

#[derive(winnow_args::Subcommand, Debug, Clone)]
pub enum QueryCommand {
    #[arg(help = "Find which package owns a file", alias_hidden = "b")]
    Belongs {
        /// File path(s) to look up in the VDB contents records
        #[arg(positional, required)]
        file: Vec<String>,
    },
    #[arg(help = "Verify checksums of installed package", alias_hidden = "k")]
    Check {
        /// Installed package atom(s) to verify
        #[arg(positional, required)]
        atom: Vec<String>,
        #[arg(flatten)]
        format_arg: FormatArg,
    },
    #[arg(help = "List packages depending on an atom", alias_hidden = "d")]
    Depends {
        /// Atom(s) whose dependents to list
        #[arg(positional, required)]
        atom: Vec<String>,
    },
    #[arg(
        help = "Display full dependency tree",
        alias_hidden = "g",
        example = "em query depgraph zlib"
    )]
    Depgraph {
        /// Atom(s) to resolve and display the dependency tree for
        #[arg(positional, required)]
        atom: Vec<String>,
        #[arg(flatten)]
        format_arg: DepgraphFormatArg,
        /// Let the solver choose USE flags to satisfy REQUIRED_USE (Level C)
        #[arg(long)]
        autosolve_use: bool,
        #[arg(flatten)]
        depgraph_flags: DepgraphFlags,
        /// Treat every atom as not-yet-installed (emerge's `-e`/`--emptytree`)
        #[arg(short = 'e', long)]
        emptytree: bool,
        /// Only show dependencies, excluding the given atoms themselves from the tree
        #[arg(short = 'o', long)]
        onlydeps: bool,
        /// With `--deep`, also follow the build-time deps of installed packages that are kept
        #[arg(long)]
        with_bdeps: bool,
        /// emerge's `--root-deps[=rdeps]`: only require RDEPEND (not DEPEND)
        /// to be satisfiable in the merge target.
        #[arg(long = "root-deps")]
        root_deps: bool,
    },
    #[arg(help = "List files installed by a package", alias_hidden = "f")]
    Files {
        /// Atom(s) whose installed file list to show
        #[arg(positional, required)]
        atom: Vec<String>,
    },
    #[arg(
        help = "List installed packages by a VDB field value",
        alias_hidden = "a"
    )]
    Has {
        /// VDB field to match, e.g. `SLOT`, `USE`, `repository`
        #[arg(positional)]
        field: String,
        /// Value the field must contain; omit to list every package whose
        /// field is set at all
        #[arg(positional)]
        value: Option<String>,
    },
    #[arg(
        help = "List packages with a given USE flag in IUSE",
        alias_hidden = "h"
    )]
    Hasuse {
        /// USE flag name(s) to search for in IUSE
        #[arg(positional, required)]
        flag: Vec<String>,
    },
    #[arg(
        help = "Display keyword status across architectures",
        alias_hidden = "y"
    )]
    Keywords {
        /// Atom(s) to show keyword status for
        #[arg(positional, required)]
        atom: Vec<String>,
        #[arg(flatten)]
        format_arg: FormatArg,
    },
    #[arg(help = "List installed/available packages matching a pattern")]
    List {
        /// List only installed packages (from VDB), not available ones
        #[arg(short = 'I', long = "installed")]
        installed: bool,
        /// Glob or substring pattern(s); omit to list all packages
        #[arg(positional)]
        pattern: Vec<String>,
    },
    #[arg(
        help = "Display package metadata (maintainer, homepage, etc.)",
        alias_hidden = "m"
    )]
    Meta {
        /// Atom(s) whose metadata to display
        #[arg(positional, required)]
        atom: Vec<String>,
        #[arg(flatten)]
        format_arg: FormatArg,
    },
    #[arg(help = "Display total file size of a package", alias_hidden = "s")]
    Size {
        /// Atom(s) whose installed file size to sum
        #[arg(positional, required)]
        atom: Vec<String>,
    },
    #[arg(help = "Display USE flags for a package", alias_hidden = "u")]
    Uses {
        /// Atom(s) whose USE flags to display
        #[arg(positional, required)]
        atom: Vec<String>,
    },
    #[arg(
        help = "Print full path to the ebuild for a package",
        alias_hidden = "w"
    )]
    Which {
        /// Atom(s) to resolve to an ebuild path
        #[arg(positional, required)]
        atom: Vec<String>,
    },
}

/// `em etc <command>`
#[derive(winnow_args::Subcommand, Debug, Clone)]
pub enum EtcCommand {
    #[arg(help = "Show what each pending file would change")]
    Diff {
        /// Only files whose path contains this text
        #[arg(positional)]
        path: Option<String>,
    },
    #[arg(help = "Resolve each pending file interactively")]
    Merge,
}

/// Batch resolutions for `em etc`
///
/// Mutually exclusive with each other; without any of them `em etc` lists.
#[derive(winnow_args::Args, Clone, Debug, Default)]
pub struct EtcOpts {
    /// Install every pending file over its target
    #[arg(long, conflicts("use_old", "auto"))]
    pub use_new: bool,
    /// Discard every pending file, keeping what is installed
    #[arg(long, conflicts("use_new", "auto"))]
    pub use_old: bool,
    /// Resolve only what needs no decision: identical files, and those
    /// differing from the installed one in comments or whitespace alone
    #[arg(long, conflicts("use_new", "use_old"))]
    pub auto: bool,
}

#[derive(winnow_args::Subcommand, Debug, Clone)]
pub enum CleanTarget {
    #[arg(
        help = "Remove distfiles no ebuild references",
        alias_hidden = "distfiles",
        alias_hidden = "d"
    )]
    Dist {
        #[arg(flatten)]
        opts: CleanOpts,
    },
    #[arg(
        help = "Remove binary packages no ebuild references",
        alias_hidden = "packages",
        alias_hidden = "p"
    )]
    Pkg {
        #[arg(flatten)]
        opts: CleanOpts,
    },
    #[arg(help = "Everything above, plus the build logs finished merges leave behind")]
    All {
        #[arg(flatten)]
        opts: CleanOpts,
    },
}

/// Filters shared by both clean targets
///
/// Deliberately narrower than `eclean`'s: the destructive/interactive modes it
/// grew are covered here by the global `-p` plus `--deep`, and everything else
/// it offers is a filter on the same candidate set.
#[derive(winnow_args::Args, Clone, Debug, Default)]
pub struct CleanOpts {
    /// Keep only what installed packages still reference, rather than
    /// everything any ebuild in the tree references
    #[arg(short = 'd', long)]
    pub deep: bool,
    /// Skip files smaller than this (e.g. `10M`, `1G`) — clears the big wins
    /// without touching a long tail of small files
    #[arg(short = 's', long, value_name = "SIZE")]
    pub size_limit: Option<String>,
    /// Keep files modified more recently than this (e.g. `2weeks`, `30d`)
    #[arg(short = 't', long, value_name = "AGE")]
    pub time_limit: Option<String>,
}

#[derive(winnow_args::Subcommand, Debug, Clone)]
pub enum NewsCommand {
    #[arg(help = "Count unread news items")]
    Count,
    #[arg(help = "List news items")]
    List,
    #[arg(
        help = "Read news items (numbers/names from `list`; \"new\"/\"all\", or none for all unread)"
    )]
    Read {
        /// Item numbers/names from `list`, the single keyword "new" (every
        /// unread item) or "all" (every item), or omit for "new".
        #[arg(positional)]
        ids: Vec<String>,
    },
    #[arg(help = "Purge read news items")]
    Purge,
}

#[derive(winnow_args::Subcommand, Debug, Clone)]
pub enum GlsaCommand {
    #[arg(help = "List all GLSAs")]
    List,
    #[arg(help = "Check for affected GLSAs")]
    Check {
        /// GLSA id(s) to check (default: every GLSA in the repo)
        #[arg(positional)]
        ids: Vec<String>,
    },
    #[arg(help = "Apply a GLSA fix")]
    Fix {
        /// GLSA id(s) to fix (default: every affected GLSA)
        #[arg(positional)]
        ids: Vec<String>,
    },
}

/// `em active <subcommand>` — persistent default `--prefix` / `--local`
///
/// `set` reads the global `--prefix` / `--local` flags (same shape as
/// `em setup --prefix DIR`), so there is no second set of flag names to
/// collide with the globals.
///
/// Entries can be referenced by name, index (0-based), or exact path.
#[derive(winnow_args::Subcommand, Debug, Clone)]
pub enum ActiveCommand {
    /// Show the registered active context (default when no subcommand)
    #[arg(help = "Show the registered active prefix/local")]
    Show,
    /// Register the invocation's `--prefix` or `--local` as the active context
    ///
    /// Without arguments, reads from `--prefix`/`--local` flags:
    ///   `em active set --prefix /home/me/prefix`
    ///   `em active set --local=`           (default `~/.gentoo`)
    ///   `em active --local= set`           (same; flags may precede set)
    ///   `em active set --local /other`
    ///
    /// With a reference argument, activates an existing entry:
    ///   `em active set my-name`     # by name
    ///   `em active set 0`           # by index
    ///   `em active set /path/to/dir` # by exact path
    #[arg(help = "Register --prefix/--local as active or activate an existing entry")]
    Set {
        /// Reference to an existing entry (name, index, or path) to activate
        ///
        /// If not provided, creates a new entry from --prefix/--local flags.
        #[arg(positional, value_name = "REF")]
        reference: Option<String>,
    },
    /// Clear the registered active context
    ///
    /// Use `--all` to remove all entries, not just the active pointer.
    #[arg(help = "Clear the active context (or all entries with --all)")]
    Clear {
        /// Clear all entries, not just the active pointer
        #[arg(long)]
        all: bool,
    },
    /// Print shell exports for `eval "$(em active env)"` (PATH + markers)
    #[arg(help = "Print shell exports for the active context")]
    Env,
    /// List all registered entries
    #[arg(help = "List all registered prefix/local entries")]
    List,
    /// Add a new entry without activating it
    ///
    /// Examples:
    ///   `em active add --prefix /home/me/prefix my-prefix`
    ///   `em active add --local /home/me/.gentoo my-gentoo`
    ///   `em active add --local=`  # adds ~/.gentoo with auto-generated name
    #[arg(help = "Add a new prefix/local entry")]
    Add {
        /// Optional name for the entry. If not provided, uses path basename
        #[arg(positional, value_name = "NAME")]
        name: Option<String>,
    },
    /// Remove an entry by name, index, or path
    ///
    /// Examples:
    ///   `em active remove my-name`
    ///   `em active remove 0`           # by index
    ///   `em active remove /path/to/dir` # by exact path
    #[arg(help = "Remove a registered entry")]
    Remove {
        /// Reference to the entry to remove (name, index, or path)
        #[arg(positional, value_name = "REF")]
        reference: String,
    },
}

#[derive(winnow_args::Subcommand, Debug, Clone)]
pub enum LogCommand {
    #[arg(help = "Show currently running merges")]
    Current,
    #[arg(help = "Show recent merge history from activity JSONL")]
    List {
        /// Max rows (default 20)
        #[arg(positional)]
        limit: Option<u32>,
    },
    #[arg(help = "Show merge times for a package (or global median)")]
    Time {
        /// Package name/atom substring to filter by; omit for the global median
        #[arg(positional)]
        atom: Option<String>,
    },
    #[arg(help = "ETA for remainder of a live activity session")]
    Predict,
}

/// How an unprivileged build gets root for `chown`/setuid (see `--privilege`)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, winnow_args::ValueEnum)]
pub enum Privilege {
    /// Best compiled-in fake root (pseudoroot, else none) when
    /// unprivileged, real chowns when already root (default).
    #[default]
    Auto,
    /// LD_PRELOAD fake root (`pseudoroot`); ownership faked in-session, no per-syscall tax
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

/// `--format` for `em query depgraph`
#[derive(winnow_args::Args, Debug, Clone, Copy)]
#[arg(
    output("pretty", default, help = "emerge -p style pretend output"),
    output("json", framing = "json", help = "Machine-parsable JSON"),
    output("tree", help = "cargo tree style dependency tree")
)]
pub struct DepgraphFormatArg {
    /// Output format
    #[arg(long, short, default = "pretty")]
    pub format: DepgraphFormat,
}

/// Output format for `em query depgraph`
#[derive(Clone, Copy, Debug, winnow_args::ValueEnum)]
pub enum DepgraphFormat {
    /// emerge -p style pretend output
    Pretty,
    /// Machine-parsable JSON
    Json,
    /// cargo tree style dependency tree
    Tree,
}
