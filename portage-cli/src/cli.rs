use std::collections::HashSet;
use std::ffi::OsString;
use std::str::FromStr;

use clap::builder::styling::{AnsiColor as ClapAnsiColor, Styles};
use clap::error::ErrorKind;
use clap::{CommandFactory, Parser, Subcommand};
use gentoo_core::Arch;
#[cfg(test)]
use portage_atom_pubgrub::DepClass;
use portage_resolve::Roots;

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

const fn cli_styles() -> Styles {
    Styles::styled()
        .header(ClapAnsiColor::Yellow.on_default().bold())
        .usage(ClapAnsiColor::Green.on_default().bold())
        .literal(ClapAnsiColor::Green.on_default())
        .placeholder(ClapAnsiColor::Cyan.on_default())
        .error(ClapAnsiColor::Red.on_default().bold())
        .valid(ClapAnsiColor::Green.on_default())
        .invalid(ClapAnsiColor::Red.on_default())
}

#[derive(Parser)]
#[command(
    name = "em",
    version,
    about = "Gentoo Portage package manager workalike",
    arg_required_else_help = true,
    styles = cli_styles()
)]
pub struct Cli {
    #[command(flatten)]
    pub color: colorchoice_clap::Color,

    /// Show what would be done without actually performing any actions
    #[arg(short = 'p', long, global = true)]
    pub pretend: bool,

    /// Print system/build info: profile, CHOST/CFLAGS/FEATURES/USE (with
    /// USE_EXPAND groups like VIDEO_CARDS broken out), ACCEPT_KEYWORDS/
    /// ACCEPT_LICENSE, and configured repositories — `emerge --info`
    /// workalike. Takes no atoms. Combine with `--json` for structured
    /// output, or `-v` to also list every known `@name` set and its
    /// resolved atoms (neither has a real-emerge equivalent).
    #[arg(long)]
    pub info: bool,

    /// Structured JSON (`em --info --json`, merge-plan `-p --json`)
    #[arg(long)]
    pub json: bool,

    /// Increase verbosity: `-v` labels each build phase, `-vv`/`-vvv` add
    /// `em`'s own debug/trace logs (see also `RUST_LOG`).
    #[arg(short = 'v', long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Suppress non-error output
    #[arg(short = 'q', long, global = true)]
    pub quiet: bool,

    /// Target architecture for operations. Defaults to current system architecture
    #[arg(
        long,
        value_name = "ARCH",
        default_value_t = Arch::current(),
        value_parser = parse_arch,
        global = true
    )]
    pub arch: Arch,

    /// Pin search/query to a single repository
    ///
    /// When unset, repositories are auto-discovered from `repos.conf` (the main repo wins for
    /// single-repo applets; search walks all of them).
    #[arg(long, value_name = "PATH", global = true)]
    pub repo: Option<String>,

    #[command(subcommand)]
    pub applet: Option<Applet>,
}

impl Cli {
    /// The active applet's own `Topology`/`RootArg`, or a harmless bare-host
    /// default for applets that carry neither (the root-independent stubs —
    /// `Helper`/`Worker`/`Portageq`/`Grep`/`Dispatch`/`Etc`/`Clean`/`Atom` —
    /// and the bare "no applet at all" case, which [`parse_cli_from`] never
    /// leaves as `None` once real topology-shaped input is present).
    ///
    /// Every `Cli`-level root-resolution method below is a thin selector over
    /// this — unlike `MergeFlags`/`DepgraphFlags`/`ActivityArgs`, there is no
    /// second, top-level copy to reconcile against (nothing is flattened onto
    /// `Cli`'s own root at all), so this only ever *picks* the one applet-
    /// owned value, never merges two.
    fn topology_and_root(&self) -> (Topology, RootArg) {
        match &self.applet {
            Some(Applet::Emerge(a)) => (a.topology.clone(), a.root_arg.clone()),
            Some(Applet::Crossdev(a)) => (a.topology.clone(), RootArg::default()),
            Some(Applet::Toolchain(a)) => (a.topology.clone(), a.root_arg.clone()),
            Some(Applet::Stages(a)) => (a.topology.clone(), a.root_arg.clone()),
            Some(Applet::Setup(a)) => (a.topology.clone(), a.root_arg.clone()),
            Some(Applet::Ebuild {
                topology, root_arg, ..
            })
            | Some(Applet::Maint {
                topology, root_arg, ..
            })
            | Some(Applet::Sync {
                topology, root_arg, ..
            })
            | Some(Applet::Depclean {
                topology, root_arg, ..
            })
            | Some(Applet::Regen {
                topology, root_arg, ..
            })
            | Some(Applet::Quickpkg {
                topology, root_arg, ..
            })
            | Some(Applet::MirrorDist {
                topology, root_arg, ..
            })
            | Some(Applet::Clean {
                topology, root_arg, ..
            })
            | Some(Applet::Query {
                topology, root_arg, ..
            })
            | Some(Applet::Use {
                topology, root_arg, ..
            })
            | Some(Applet::Pkg {
                topology, root_arg, ..
            })
            | Some(Applet::Revdep {
                topology, root_arg, ..
            })
            | Some(Applet::Read {
                topology, root_arg, ..
            })
            | Some(Applet::Log {
                topology, root_arg, ..
            })
            | Some(Applet::Search {
                topology, root_arg, ..
            })
            | Some(Applet::Select {
                topology, root_arg, ..
            })
            | Some(Applet::Env {
                topology, root_arg, ..
            }) => (topology.clone(), root_arg.clone()),
            Some(Applet::Active { topology, .. }) => (topology.clone(), RootArg::default()),
            _ => (Topology::default(), RootArg::default()),
        }
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

    /// The active applet's `--target` tuple, if any.
    pub(crate) fn target(&self) -> Option<String> {
        self.topology_and_root().0.target
    }

    /// The active applet's `--vdb` override, if any.
    pub fn vdb(&self) -> Option<String> {
        self.topology_and_root().0.vdb
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

    /// Repositories to walk for `em search`
    ///
    /// Honours `--repo` when set; otherwise returns every entry from `repos.conf` (main first).
    pub fn search_repos(&self) -> Vec<std::path::PathBuf> {
        if let Some(p) = &self.repo {
            return vec![std::path::PathBuf::from(p)];
        }
        match self.roots().repos_conf() {
            Ok(rc) if !rc.repos().is_empty() => rc
                .repos()
                .iter()
                .filter_map(|e| e.location.as_path().map(std::path::PathBuf::from))
                .collect(),
            _ => vec![std::path::PathBuf::from("/var/db/repos/gentoo")],
        }
    }

    /// The active applet's own [`MergeFlags`], or the all-`false`/`None` default
    /// for an applet that doesn't carry one.
    pub fn merge_flags(&self) -> MergeFlags {
        let mut flags = match &self.applet {
            Some(Applet::Emerge(a)) => a.merge_flags.clone(),
            Some(Applet::Crossdev(a)) => a.merge_flags.clone(),
            Some(Applet::Toolchain(a)) => a.merge_flags.clone(),
            Some(Applet::Stages(a)) => a.merge_flags.clone(),
            Some(Applet::Setup(a)) => a.merge_flags.clone(),
            Some(Applet::Revdep { merge_flags, .. }) => merge_flags.clone(),
            Some(Applet::Depclean { merge_flags, .. }) => merge_flags.clone(),
            _ => MergeFlags::default(),
        };
        flags.json |= self.json;
        flags
    }

    /// The active applet's own [`DepgraphFlags`], or the all-`false` default
    /// for an applet that doesn't carry one.
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

    /// Effective activity-output flags for the dispatched command: each
    /// build-shaped applet (`emerge`/`regen`/`crossdev`/`toolchain`/`stages`/
    /// `setup`) owns exactly one [`ActivityArgs`] now — this just selects it.
    pub fn effective_activity(&self) -> ActivityArgs {
        match &self.applet {
            Some(Applet::Emerge(a)) => a.activity.clone(),
            Some(Applet::Regen { activity, .. }) => activity.clone(),
            Some(Applet::Crossdev(a)) => a.activity.clone(),
            Some(Applet::Toolchain(a)) => a.activity.clone(),
            Some(Applet::Stages(a)) => a.activity.clone(),
            Some(Applet::Setup(a)) => a.activity.clone(),
            _ => ActivityArgs::default(),
        }
    }

    /// Effective privilege backend for the dispatched command: each
    /// privilege-relevant applet owns exactly one `--privilege` field
    /// (default `Privilege::Auto`) now — this just selects it.
    pub fn effective_privilege(&self) -> Privilege {
        match &self.applet {
            Some(Applet::Emerge(a)) => a.privilege,
            Some(Applet::Crossdev(a)) => a.privilege,
            Some(Applet::Toolchain(a)) => a.privilege,
            Some(Applet::Stages(a)) => a.privilege,
            Some(Applet::Setup(a)) => a.privilege,
            _ => Privilege::Auto,
        }
    }

    /// `Applet::Emerge`'s own mode switches, or the all-`false` default when
    /// it isn't the active applet (unreachable in practice: [`parse_cli_from`]
    /// always resolves a bare invocation into `Applet::Emerge` first).
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

/// Parse argv, making the word `emerge` optional.
///
/// Try the real argv first; on failure, retry with `emerge` after argv0 so
/// `em --root R cat/pkg` ≡ `em emerge --root R cat/pkg`. Help/version is not
/// retried, and a sibling applet in argv is left as a parse error.
pub fn parse_cli_from<I, T>(raw: I) -> Result<Cli, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let raw: Vec<OsString> = raw.into_iter().map(Into::into).collect();
    match Cli::try_parse_from(&raw) {
        Ok(cli) => Ok(cli),
        Err(err) => {
            if is_help_or_version(err.kind()) {
                return Err(err);
            }
            match known_subcommand_token(&raw) {
                Some(name) if name != "emerge" => Err(err),
                _ => Cli::try_parse_from(with_leading_emerge(&raw)),
            }
        }
    }
}

fn is_help_or_version(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::DisplayHelp
            | ErrorKind::DisplayVersion
            | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
    )
}

fn value_taking_flags() -> HashSet<String> {
    let mut cmd = Cli::command();
    cmd.build();
    let mut out = HashSet::new();
    fn walk(cmd: &clap::Command, out: &mut HashSet<String>) {
        for arg in cmd.get_arguments() {
            let Some(range) = arg.get_num_args() else {
                continue;
            };
            // `--local` is `0..=1`; a following applet name is the applet, not the DIR.
            if !range.takes_values() || range.min_values() < 1 {
                continue;
            }
            if let Some(long) = arg.get_long() {
                out.insert(format!("--{long}"));
            }
            if let Some(short) = arg.get_short() {
                out.insert(format!("-{short}"));
            }
        }
        for sub in cmd.get_subcommands() {
            walk(sub, out);
        }
    }
    walk(&cmd, &mut out);
    out
}

fn consumes_next_as_value(flag: &str, next: Option<&OsString>, taking: &HashSet<String>) -> bool {
    if flag.contains('=') {
        return false;
    }
    taking.contains(flag) && next.is_some_and(|n| !n.to_string_lossy().starts_with('-'))
}

fn known_subcommand_token(raw: &[OsString]) -> Option<String> {
    let names: HashSet<String> = Cli::command()
        .get_subcommands()
        .flat_map(|s| {
            std::iter::once(s.get_name().to_string()).chain(s.get_all_aliases().map(str::to_string))
        })
        .collect();
    let taking = value_taking_flags();
    let mut i = 1;
    while i < raw.len() {
        let Some(s) = raw[i].to_str() else {
            i += 1;
            continue;
        };
        if s == "--" {
            break;
        }
        if s.starts_with('-') {
            if consumes_next_as_value(s, raw.get(i + 1), &taking) {
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if names.contains(s) {
            return Some(s.to_string());
        }
        i += 1;
    }
    None
}

fn with_leading_emerge(raw: &[OsString]) -> Vec<OsString> {
    let emerge = OsString::from("emerge");
    let taking = value_taking_flags();
    let mut out = Vec::with_capacity(raw.len() + 1);
    out.push(raw.first().cloned().unwrap_or_else(|| "em".into()));
    out.push(emerge.clone());
    let mut i = 1;
    let mut skipped_applet = false;
    while i < raw.len() {
        let t = &raw[i];
        let s = t.to_string_lossy();
        if s.starts_with('-') {
            out.push(t.clone());
            if consumes_next_as_value(&s, raw.get(i + 1), &taking) {
                i += 1;
                if i < raw.len() {
                    out.push(raw[i].clone());
                }
            }
            i += 1;
            continue;
        }
        if !skipped_applet && t == &emerge {
            skipped_applet = true;
            i += 1;
            continue;
        }
        out.push(t.clone());
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Explicit `emerge`; `parse_cli_from` is what makes the bare form equivalent.
    fn emerge<'a>(argv: impl IntoIterator<Item = &'a str>) -> Cli {
        Cli::parse_from(["em", "emerge"].into_iter().chain(argv))
    }

    // Clap only validates a subcommand's args when that subcommand is actually
    // built, and only under debug_assertions — so a short-flag collision
    // introduced by a flattened mixin stays invisible in release builds until
    // someone runs the one subcommand that has it (this is how `-a` vs `em
    // use`'s `--add` shipped). `debug_assert()` walks the whole tree at once.
    #[test]
    fn every_subcommand_has_unique_flags() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
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
        let cli_set = Cli::parse_from(["em", "active", "--local", local_s, "set"]);
        crate::active::run(
            cli_set.applet.as_ref().and_then(|a| match a {
                Applet::Active { command, .. } => command.as_ref(),
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
        let cli_set = Cli::parse_from(["em", "active", "--prefix", prefix_s, "set"]);
        crate::active::run(
            cli_set.applet.as_ref().and_then(|a| match a {
                Applet::Active { command, .. } => command.as_ref(),
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
        let cli_set = Cli::parse_from(["em", "active", "--prefix", prefix_s, "set"]);
        crate::active::run(
            cli_set.applet.as_ref().and_then(|a| match a {
                Applet::Active { command, .. } => command.as_ref(),
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

    #[test]
    fn bare_and_explicit_emerge_produce_identical_args() {
        let bare = parse_cli_from(["em", "--root", "/srv/x", "-p", "sys-libs/zlib"]).unwrap();
        let explicit =
            parse_cli_from(["em", "emerge", "--root", "/srv/x", "-p", "sys-libs/zlib"]).unwrap();
        let Some(Applet::Emerge(a)) = &bare.applet else {
            panic!("expected Applet::Emerge from bare argv");
        };
        let Some(Applet::Emerge(b)) = &explicit.applet else {
            panic!("expected Applet::Emerge from explicit argv");
        };
        assert_eq!(a.root_arg.root, b.root_arg.root);
        assert_eq!(a.atoms, b.atoms);
        assert!(bare.pretend && explicit.pretend);
    }

    #[test]
    fn flags_before_emerge_word_reorder() {
        let cli =
            parse_cli_from(["em", "--root", "/srv/x", "emerge", "-p", "sys-libs/zlib"]).unwrap();
        let Some(Applet::Emerge(args)) = &cli.applet else {
            panic!("expected Applet::Emerge");
        };
        assert_eq!(args.root_arg.root.as_deref(), Some("/srv/x"));
        assert_eq!(args.atoms, vec!["sys-libs/zlib".to_string()]);
        assert!(cli.pretend);
    }

    #[test]
    fn unrecognized_flag_inside_a_real_subcommand_does_not_retry_as_emerge() {
        let Err(err) = parse_cli_from(["em", "crossdev", "--bogus-flag"]) else {
            panic!("expected parse error");
        };
        let msg = err.to_string();
        assert!(
            msg.contains("crossdev"),
            "error should mention crossdev, got: {msg}"
        );
        assert!(
            !msg.contains("em emerge"),
            "must not rewrite into emerge, got: {msg}"
        );
    }

    #[test]
    fn root_before_crossdev_is_not_rewritten_into_emerge() {
        let Err(err) = parse_cli_from(["em", "--root", "/tmp/r", "crossdev", "--setup"]) else {
            panic!("expected parse error");
        };
        let msg = err.to_string();
        assert!(
            !msg.contains("em emerge"),
            "must not rewrite into emerge, got: {msg}"
        );
    }

    #[test]
    fn top_level_help_is_not_rewritten_into_emerge() {
        let Err(err) = parse_cli_from(["em", "--help"]) else {
            panic!("expected parse error");
        };
        assert_eq!(err.kind(), ErrorKind::DisplayHelp);
        let msg = err.to_string();
        assert!(msg.contains("query") && msg.contains("crossdev"), "{msg}");
        assert!(!msg.contains("Usage: em emerge"), "{msg}");
    }

    #[test]
    fn applet_help_is_not_rewritten_into_emerge() {
        let Err(err) = parse_cli_from(["em", "toolchain", "--help"]) else {
            panic!("expected parse error");
        };
        assert_eq!(err.kind(), ErrorKind::DisplayHelp);
        let msg = err.to_string();
        assert!(msg.contains("Usage: em toolchain"), "{msg}");
        assert!(!msg.contains("Usage: em emerge"), "{msg}");
    }

    #[test]
    fn version_is_not_rewritten_into_emerge() {
        let Err(err) = parse_cli_from(["em", "--version"]) else {
            panic!("expected parse error");
        };
        assert_eq!(err.kind(), ErrorKind::DisplayVersion);
    }

    #[test]
    fn info_json_parses_without_emerge() {
        let cli = parse_cli_from(["em", "--info", "--json"]).unwrap();
        assert!(cli.info);
        assert!(cli.json);
        assert!(cli.applet.is_none());
    }

    #[test]
    fn arch_and_repo_work_on_the_bare_path() {
        let cli = parse_cli_from(["em", "--arch", "amd64", "-p", "sys-libs/zlib"]).unwrap();
        assert_eq!(cli.arch, Arch::from_str("amd64").unwrap());
        let Some(Applet::Emerge(args)) = &cli.applet else {
            panic!("expected Applet::Emerge");
        };
        assert_eq!(args.atoms, vec!["sys-libs/zlib".to_string()]);

        let via_emerge =
            parse_cli_from(["em", "--arch", "amd64", "emerge", "-p", "sys-libs/zlib"]).unwrap();
        assert_eq!(via_emerge.arch, cli.arch);

        let repo = parse_cli_from(["em", "--repo", "/tmp/r", "-p", "sys-libs/zlib"]).unwrap();
        assert_eq!(repo.repo.as_deref(), Some("/tmp/r"));
    }

    #[test]
    fn json_before_emerge_word_is_merge_plan_json() {
        let before = parse_cli_from(["em", "--json", "emerge", "-p", "sys-libs/zlib"]).unwrap();
        assert!(before.merge_flags().json);
        let explicit = parse_cli_from(["em", "emerge", "--json", "-p", "sys-libs/zlib"]).unwrap();
        assert!(explicit.merge_flags().json);
        let bare = parse_cli_from(["em", "--json", "-p", "sys-libs/zlib"]).unwrap();
        assert!(bare.merge_flags().json);
    }

    #[test]
    fn value_taking_flags_include_root_and_exclude() {
        let f = value_taking_flags();
        assert!(f.contains("--root"), "missing --root in {f:?}");
        assert!(f.contains("-X"), "missing -X in {f:?}");
    }

    #[test]
    fn exclude_value_matching_an_applet_name_still_retries() {
        let cli = parse_cli_from(["em", "-X", "search", "-p", "sys-libs/zlib"]).unwrap();
        let Some(Applet::Emerge(args)) = &cli.applet else {
            panic!("expected Applet::Emerge");
        };
        assert_eq!(args.merge_flags.exclude, vec!["search".to_string()]);
        assert_eq!(args.atoms, vec!["sys-libs/zlib".to_string()]);
    }

    #[test]
    fn root_value_named_emerge_is_kept() {
        let cli = parse_cli_from(["em", "--root", "emerge", "-p", "sys-libs/zlib"]).unwrap();
        let Some(Applet::Emerge(args)) = &cli.applet else {
            panic!("expected Applet::Emerge");
        };
        assert_eq!(args.root_arg.root.as_deref(), Some("emerge"));
        assert_eq!(args.atoms, vec!["sys-libs/zlib".to_string()]);
    }

    // `crossdev` deliberately never flattens `RootArg` (see `CrossdevArgs`'s
    // doc comment) — `--root` must be a clap parse error, not merely ignored,
    // in any position.
    #[test]
    fn crossdev_rejects_root_in_any_position() {
        assert!(
            Cli::try_parse_from(["em", "crossdev", "--setup", "--root", "/tmp/a"]).is_err(),
            "--root after crossdev must be a clap error"
        );
        assert!(
            Cli::try_parse_from(["em", "crossdev", "--root", "/tmp/a", "--setup"]).is_err(),
            "--root anywhere in crossdev's own arg list must be a clap error"
        );
        assert!(
            parse_cli_from(["em", "crossdev", "--setup", "--root", "/tmp/a"]).is_err(),
            "retry path must not accept --root on crossdev either"
        );
    }
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)] // __worker carries many CLI strings
pub enum Applet {
    /// Run one do*/new* install helper standalone against the exported build env
    ///
    /// Internal: backs the PATH shims dropped during a build so `find -exec doman` /
    /// `xargs do*` reach helpers that are in-shell builtins. Not for direct use.
    #[command(name = "__helper", hide = true)]
    Helper {
        /// Helper name (e.g. `doman`, `dolib.a`)
        name: String,
        /// Arguments passed through to the helper
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Internal: the privilege-wrapped install worker (install+qmerge+binpkg
    /// for one package; spawned per package by `build_and_merge`).
    #[command(name = "__worker", hide = true)]
    Worker {
        #[arg(long)]
        ebuild: String,
        /// The resolved plan entry's authoritative cpv — see
        /// `privilege::WorkerArgs::cpv`.
        #[arg(long)]
        cpv: String,
        #[arg(long)]
        use_flags: String,
        #[arg(long)]
        work_base: String,
        #[arg(long)]
        root: String,
        #[arg(long)]
        distdir: Option<String>,
        #[arg(long)]
        config_root: Option<String>,
        #[arg(long)]
        sysroot: Option<String>,
        #[arg(long)]
        eprefix: Option<String>,
        /// Where BDEPEND-class build tools live (`Cli::host_roots()`'s merge root)
        #[arg(long)]
        broot: Option<String>,
        /// See `ebuild::RootContext::self_contained_bootstrap`
        #[arg(long)]
        self_contained_bootstrap: bool,
        /// See `ebuild::RootContext::extra_path`, `:`-joined
        #[arg(long)]
        extra_path: Option<String>,
        /// A pre-built GPKG to merge (`-k`/`-g`)
        #[arg(long)]
        binpkg: Option<String>,
        /// `binpkg`'s origin forces cryptographic GPG signature
        /// verification (a `binrepos.conf` entry with
        /// `verify-signature = yes`), independent of
        /// `FEATURES=binpkg-request-signature`.
        #[arg(long)]
        force_verify_signature: bool,
        #[arg(long)]
        buildpkg: bool,
        #[arg(long)]
        quiet: bool,
        /// Parent activity session id — live FS phase updates only
        #[arg(long)]
        activity_job_id: Option<String>,
        #[arg(long)]
        activity_parent_job_id: Option<String>,
        /// Filesystem root of the parent's live activity sink
        #[arg(long)]
        activity_live_root: Option<String>,
        /// `host` or `target` package side for inflight paths
        #[arg(long)]
        activity_side: Option<String>,
        /// Unix socket path: stream phase JSONL back to the parent activity bus
        #[arg(long)]
        activity_reemit_path: Option<String>,
    },

    #[command(about = "Execute ebuild phases")]
    Ebuild {
        /// Path to the `.ebuild` file to execute
        #[arg(required = true)]
        ebuild_path: String,
        /// Phase(s) to run in order (e.g. `compile`, `install`, `qmerge`)
        #[arg(required = true)]
        phase: Vec<String>,
        /// Override the build work directory (default: `/var/tmp/portage/<cat>/<pf>`)
        #[arg(short = 'w', long, value_name = "DIR")]
        work_dir: Option<camino::Utf8PathBuf>,
        #[command(flatten)]
        topology: Topology,
        #[command(flatten)]
        root_arg: RootArg,
    },

    #[command(about = "System maintenance and health checks")]
    Maint {
        #[command(subcommand)]
        command: Option<MaintCommand>,
        #[command(flatten)]
        topology: Topology,
        #[command(flatten)]
        root_arg: RootArg,
    },

    #[command(about = "Query Portage internal variables and data")]
    Portageq {
        /// portageq sub-command to run (e.g. `envvar`, `get_repos`)
        #[arg(required = true)]
        command: String,
        /// Arguments passed through to the sub-command
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },

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
    #[command(about = "Sync repositories (git, rsync)")]
    Sync {
        /// Repo names from repos.conf (default: auto-sync enabled repos)
        repos: Vec<String>,
        #[command(flatten)]
        topology: Topology,
        #[command(flatten)]
        root_arg: RootArg,
    },

    #[command(about = "Remove orphaned/unused packages")]
    Depclean {
        /// Restrict cleaning to these atoms' dependency closure (every other
        /// installed package is protected). Default: the whole `@world` set
        #[arg(trailing_var_arg = true)]
        atoms: Vec<String>,
        #[command(flatten)]
        topology: Topology,
        #[command(flatten)]
        root_arg: RootArg,
        /// `--exclude`/`--with-bdeps` only — the rest of `MergeFlags` means
        /// nothing to depclean's own read-then-remove walk.
        #[command(flatten)]
        merge_flags: MergeFlags,
    },

    #[command(about = "Regenerate metadata cache")]
    Regen {
        /// Repo names or paths to regenerate (default: every repo except the
        /// main one, whose cache is normally maintained upstream)
        repos: Vec<String>,
        /// Write cache files to this directory instead of metadata/md5-cache
        #[arg(short = 'o', long, value_name = "DIR")]
        output: Option<std::path::PathBuf>,
        /// Directory containing master repositories
        #[arg(long, value_name = "DIR")]
        repos_dir: Option<String>,
        /// Number of parallel workers
        #[arg(short = 'j', long)]
        jobs: Option<usize>,
        /// Deduplicate top-level dep tokens before writing
        #[arg(long)]
        dedup: bool,
        /// Activity-output flags (`--activity-fd`/`--activity-jsonl`/
        /// `--emergelog`) — `em regen` drives its own activity bus.
        #[command(flatten)]
        activity: ActivityArgs,
        #[command(flatten)]
        topology: Topology,
        #[command(flatten)]
        root_arg: RootArg,
    },

    #[command(about = "Create binary packages from installed files")]
    Quickpkg {
        /// Atoms, package sets (`@system`), or VDB paths (`/var/db/pkg/cat/pf`)
        #[arg(required = true)]
        atoms: Vec<String>,
        /// Include CONFIG_PROTECT files
        #[arg(long)]
        include_config: bool,
        /// Include unmodified CONFIG_PROTECT files
        #[arg(long)]
        include_unmodified_config: bool,
        #[command(flatten)]
        topology: Topology,
        #[command(flatten)]
        root_arg: RootArg,
    },

    #[command(
        name = "mirrordist",
        alias = "emirrordist",
        about = "Build/maintain a distfiles mirror (emirrordist workalike)",
        long_about = "Walks every ebuild in a repository, fetches every distfile its \
SRC_URI references (all versions, all USE branches), and verifies each against \
the repo Manifest — the server side of a Gentoo mirror.\n\n\
Not to be confused with `em select mirrors`, which chooses which mirrors *this* \
machine fetches from.\n\n\
Requires an up-to-date metadata cache: run `em regen <repo>` first for overlays."
    )]
    MirrorDist {
        /// repos.conf name or path
        ///
        /// Defaults to the main repo (opposite default from `em regen`, which excludes it).
        repo: Option<String>,
        /// Directory containing master repositories
        #[arg(long, value_name = "DIR")]
        repos_dir: Option<String>,
        /// Distfiles directory to populate
        #[arg(long, value_name = "DIR", required = true)]
        distfiles: camino::Utf8PathBuf,
        /// Concurrent downloads
        #[arg(short = 'j', long)]
        jobs: Option<usize>,
        /// Delete distfiles no longer referenced by any ebuild
        #[arg(long)]
        delete: bool,
        /// Grace period before an orphaned file is deleted (e.g. `7d`, `72h`)
        #[arg(long, value_name = "DURATION", default_value = "7d")]
        deletion_delay: String,
        /// Deletion-grace state file (default: `$XDG_STATE_HOME/em/mirrordist/<repo>-*.json`)
        #[arg(long, value_name = "FILE")]
        deletion_db: Option<camino::Utf8PathBuf>,
        /// Tab-delimited log of fetched files (appended)
        #[arg(long, value_name = "FILE")]
        success_log: Option<camino::Utf8PathBuf>,
        /// Tab-delimited log of fetch failures (appended)
        #[arg(long, value_name = "FILE")]
        failure_log: Option<camino::Utf8PathBuf>,
        /// Report of files scheduled for deletion, grouped by date (rewritten)
        #[arg(long, value_name = "FILE")]
        scheduled_deletion_log: Option<camino::Utf8PathBuf>,
        /// File(s) listing distfile names --delete must never remove (one
        /// name per line, `#`-comments ignored).
        #[arg(long, value_name = "FILE")]
        whitelist_from: Vec<camino::Utf8PathBuf>,
        /// Re-hash already-present files instead of trusting their size
        #[arg(long)]
        verify_existing_digest: bool,
        /// Also try GENTOO_MIRRORS after the ebuild's own URIs (real
        /// emirrordist never does this — off by default).
        #[arg(long)]
        gentoo_mirrors_fallback: bool,
        /// Allow --delete even when some ebuilds had no metadata cache entry
        #[arg(long)]
        delete_allow_incomplete: bool,
        #[command(flatten)]
        topology: Topology,
        #[command(flatten)]
        root_arg: RootArg,
    },

    #[command(about = "Query package information")]
    Query {
        #[command(subcommand)]
        command: QueryCommand,
        #[command(flatten)]
        topology: Topology,
        #[command(flatten)]
        root_arg: RootArg,
    },

    #[command(about = "Clean distfiles and/or binary packages")]
    Clean {
        #[command(subcommand)]
        target: Option<CleanTarget>,
        #[command(flatten)]
        topology: Topology,
        #[command(flatten)]
        root_arg: RootArg,
    },

    #[command(about = "Enable/disable/query USE flags in make.conf")]
    Use {
        /// Add (enable) flags — euse calls this --enable/-E
        #[arg(
            short = 'a',
            long = "add",
            visible_short_alias = 'E',
            value_name = "FLAG"
        )]
        add: Vec<String>,
        /// Subtract flags (written with leading '-', e.g. -themes) — euse
        /// calls this --disable/-D
        #[arg(
            short = 's',
            long = "subtract",
            visible_short_alias = 'D',
            value_name = "FLAG"
        )]
        subtract: Vec<String>,
        /// Drop flags entirely (removes both flag and -flag forms) — euse
        /// calls this --remove/-R or --prune/-P
        #[arg(
            short = 'd',
            long = "drop",
            visible_short_aliases = ['R', 'P'],
            value_name = "FLAG"
        )]
        drop: Vec<String>,
        /// Preview the resulting value without writing make.conf
        #[arg(short = 'n', long = "dry-run")]
        dry_run: bool,
        /// Target a USE_EXPAND variable (e.g. VIDEO_CARDS) instead of USE —
        /// -a/-s/-d then edit that variable's value the same way
        #[arg(short = 'e', long = "expand", value_name = "VAR")]
        expand: Option<String>,
        /// List every USE_EXPAND variable known to the active profile, each
        /// with its current make.conf value
        #[arg(
            short = 'L',
            long = "list-expand",
            conflicts_with_all = ["add", "subtract", "drop", "expand"]
        )]
        list_expand: bool,
        /// Show descriptions for the given USE flags (profiles/use.desc and
        /// use.local.desc, searching both unless -g/-l restricts it). With
        /// no flags given, lists every flag in scope
        #[arg(
            short = 'i',
            long = "info",
            value_name = "FLAG",
            conflicts_with_all = ["add", "subtract", "drop", "expand", "list_expand"]
        )]
        info: Vec<String>,
        /// Restrict -i to global flags only (profiles/use.desc)
        #[arg(
            short = 'g',
            long = "global",
            conflicts_with_all = ["add", "subtract", "drop", "expand", "list_expand", "local_desc"]
        )]
        global: bool,
        /// Restrict -i to per-package local flags only (profiles/use.local.desc,
        /// searched across every package — see `em query uses <atom>` for a
        /// single package's flags instead)
        #[arg(
            short = 'l',
            long = "local-desc",
            conflicts_with_all = ["add", "subtract", "drop", "expand", "list_expand", "global"]
        )]
        local_desc: bool,
        /// Path to make.conf (default: resolved like other config commands,
        /// following --config-root/--local/--prefix)
        #[arg(long = "make-conf", value_name = "PATH")]
        make_conf: Option<camino::Utf8PathBuf>,
        #[command(flatten)]
        topology: Topology,
        #[command(flatten)]
        root_arg: RootArg,
    },

    #[command(about = "Edit per-package configuration (package.use, .keywords, .mask, .env)")]
    Pkg {
        #[command(subcommand)]
        command: PkgCommand,
        #[command(flatten)]
        topology: Topology,
        #[command(flatten)]
        root_arg: RootArg,
    },

    #[command(about = "Rebuild packages with broken shared library deps")]
    Revdep {
        /// Only consider consumers of libraries whose soname contains NAME
        #[arg(short = 'L', long, value_name = "NAME")]
        library: Option<String>,
        #[command(flatten)]
        topology: Topology,
        #[command(flatten)]
        root_arg: RootArg,
        #[command(flatten)]
        merge_flags: MergeFlags,
    },

    #[command(about = "Display Portage elog files")]
    Read {
        /// Only show packages whose `<category>/<pf>` contains this text
        package: Option<String>,
        /// List what is filed instead of printing the messages
        #[arg(short, long)]
        list: bool,
        /// Show only this many of the most recent packages; 0 for all
        #[arg(short = 'n', long, default_value_t = 10)]
        limit: usize,
        /// Remove each file once it has been shown
        #[arg(long)]
        delete: bool,
        #[command(flatten)]
        topology: Topology,
        #[command(flatten)]
        root_arg: RootArg,
    },

    #[command(about = "Analyze emerge.log")]
    Log {
        #[command(subcommand)]
        command: Option<LogCommand>,
        #[command(flatten)]
        topology: Topology,
        #[command(flatten)]
        root_arg: RootArg,
    },

    #[command(about = "Search inside ebuilds and eclasses")]
    Grep {
        /// Pattern to search for
        #[arg(required = true)]
        pattern: String,
        /// Restrict the search to these ebuild/eclass paths (default: the whole repo)
        #[arg(trailing_var_arg = true)]
        paths: Vec<String>,
    },

    #[command(about = "Search package names and descriptions")]
    Search {
        /// List all packages (no pattern required)
        #[arg(short = 'a', long)]
        all: bool,
        /// Search package descriptions instead of names
        #[arg(short = 'S', long = "desc")]
        desc: bool,
        /// Show only package name, no description
        #[arg(short = 'N', long = "name-only")]
        name_only: bool,
        /// Show homepage instead of description
        #[arg(short = 'H', long)]
        homepage: bool,
        /// Pattern to search (required unless --all)
        #[arg(required_unless_present = "all")]
        pattern: Option<String>,
        #[command(flatten)]
        topology: Topology,
        #[command(flatten)]
        root_arg: RootArg,
    },

    #[command(about = "Parse/split atom strings")]
    Atom {
        /// Atom strings to parse and print back in normalized form
        #[arg(required = true)]
        atoms: Vec<String>,
    },

    #[command(about = "Native config selectors (profile, repos) — eselect-like")]
    Select {
        #[command(subcommand)]
        command: SelectCommand,
        #[command(flatten)]
        topology: Topology,
        #[command(flatten)]
        root_arg: RootArg,
    },

    /// Register a default `--prefix` / `--local` so bare `em <pkg>` picks it up (dogfooding)
    ///
    /// Explicit `--prefix`/`--local`/`--root` still win. State is stored under
    /// `$XDG_STATE_HOME/em/active`.
    #[command(about = "Register a default --prefix/--local for bare em invocations")]
    Active {
        #[command(subcommand)]
        command: Option<ActiveCommand>,
        /// `--root` is deliberately not part of this: it is never registerable
        /// (see the module doc in `crate::active`).
        #[command(flatten)]
        topology: Topology,
    },

    #[command(about = "Bootstrap a prefix layout (use with --local or --prefix)")]
    Setup(SetupArgs),

    #[command(about = "Set up a cross-compilation target (sysroot + overlay) — crossdev workalike")]
    Crossdev(CrossdevArgs),

    #[command(
        about = "Bootstrap a self-hosting native toolchain into --root (the stages' compiler)"
    )]
    Toolchain(ToolchainArgs),

    #[command(about = "Assemble stage-build artifacts (stage1 packages.build) into --root")]
    Stages(StagesArgs),

    #[command(about = "Safe configuration file updates (dispatch-conf)")]
    Dispatch,

    #[command(about = "Interactive configuration file updates (etc-update)")]
    Etc,

    #[command(about = "Regenerate /etc/profile.env and ld.so cache")]
    Env {
        #[command(flatten)]
        topology: Topology,
        #[command(flatten)]
        root_arg: RootArg,
    },

    /// Resolve and merge/unmerge packages (emerge workalike).
    ///
    /// `em <atoms>` and `em emerge <atoms>` parse into the same arguments.
    #[command(about = "Resolve and merge/unmerge packages (emerge workalike)")]
    Emerge(EmergeArgs),
}

/// `em setup` — bootstrap a prefix layout
#[derive(clap::Args, Default)]
pub struct SetupArgs {
    /// Directory holding host tools this prefix should borrow while it has
    /// none of its own, put ahead of the sanitised build `PATH`. Repeatable.
    ///
    /// `--local` only, and only for the setup itself: builds sanitise `$HOME`
    /// and `/usr/local` off `PATH` so a local install cannot shadow the
    /// Gentoo toolchain, which also hides a hand-installed GNU sed/grep from
    /// the very first merges. `em setup --local` finds the usual locations by
    /// itself; this is for anywhere else you keep them.
    #[arg(long, value_name = "DIR")]
    pub extra_path: Vec<camino::Utf8PathBuf>,

    #[command(flatten)]
    pub topology: Topology,

    #[command(flatten)]
    pub root_arg: RootArg,

    #[command(flatten)]
    pub depgraph_flags: DepgraphFlags,

    #[command(flatten)]
    pub merge_flags: MergeFlags,

    #[command(flatten)]
    pub activity: ActivityArgs,

    /// Privilege backend for this setup run
    #[arg(long, value_enum, default_value_t = Privilege::Auto, env = "EM_PRIVILEGE")]
    pub privilege: Privilege,
}

/// `em crossdev` — cross-target setup, mirroring crossdev's option surface (the
/// no-build subset for now; building the toolchain is future work).
#[derive(clap::Args)]
pub struct CrossdevArgs {
    /// `--prefix`/`--local`/`--config-root`/`--vdb`/`--target` for this cross
    /// target. Deliberately no [`RootArg`]: none of `crossdev`'s three actions
    /// (`--init-target`/`--setup`/`--show-target-cfg`) read `--root` — it is a
    /// clap-level parse error, in any position, when `crossdev` is active.
    #[command(flatten)]
    pub topology: Topology,

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

    /// Print the derived target configuration and exit (no writes)
    #[arg(long)]
    pub show_target_cfg: bool,

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

    #[command(flatten)]
    pub depgraph_flags: DepgraphFlags,

    #[command(flatten)]
    pub merge_flags: MergeFlags,

    #[command(flatten)]
    pub activity: ActivityArgs,

    /// Privilege backend for this crossdev run
    #[arg(long, value_enum, default_value_t = Privilege::Auto, env = "EM_PRIVILEGE")]
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
#[derive(clap::Args, Debug, Clone)]
pub struct ToolchainArgs {
    /// Build and install the toolchain into `--root` (the only action for now;
    /// required, mirroring `crossdev --setup`).
    #[arg(long)]
    pub setup: bool,

    #[command(flatten)]
    pub topology: Topology,

    #[command(flatten)]
    pub root_arg: RootArg,

    #[command(flatten)]
    pub depgraph_flags: DepgraphFlags,

    #[command(flatten)]
    pub merge_flags: MergeFlags,

    #[command(flatten)]
    pub activity: ActivityArgs,

    /// Privilege backend for this toolchain run
    #[arg(long, value_enum, default_value_t = Privilege::Auto, env = "EM_PRIVILEGE")]
    pub privilege: Privilege,
}

// `em stages` — assemble stage-build artifacts (stage1/stage3/stage4) *using*
// a toolchain already built by `em toolchain --setup`.
#[derive(clap::Args, Debug, Clone)]
pub struct StagesArgs {
    /// Emerge the profile's `packages.build` bootstrap set into `--root`:
    /// baselayout (USE=build, --nodeps) then the minimal stage1 package list
    /// (USE="-* build"), mirroring catalyst's `stage1/chroot.sh`. Requires a
    /// working toolchain already in the root (`em toolchain --setup`).
    #[arg(long)]
    pub stage1: bool,

    /// Emptytree rebuild of `@system` into `--root` (catalyst `stage3/chroot.sh`:
    /// `emerge -e --update --deep --with-bdeps=y @system`). Forces `-e -uD
    /// --with-bdeps` on top of other merge flags; seeds PKGDIR with `-b` like
    /// stage1. No stage2 (crossdev model). Requires a usable root (typically
    /// after `--stage1` or an unpacked seed).
    #[arg(long)]
    pub stage3: bool,

    #[command(flatten)]
    pub topology: Topology,

    #[command(flatten)]
    pub root_arg: RootArg,

    #[command(flatten)]
    pub depgraph_flags: DepgraphFlags,

    #[command(flatten)]
    pub merge_flags: MergeFlags,

    #[command(flatten)]
    pub activity: ActivityArgs,

    /// Privilege backend for this stages run
    #[arg(long, value_enum, default_value_t = Privilege::Auto, env = "EM_PRIVILEGE")]
    pub privilege: Privilege,
}

/// `em emerge` — resolve and merge/unmerge packages (real emerge workalike)
///
/// The explicit, self-contained form of the bare `em <atoms>` path — see
/// [`Applet::Emerge`]'s doc comment.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct EmergeArgs {
    #[command(flatten)]
    pub topology: Topology,

    #[command(flatten)]
    pub root_arg: RootArg,

    #[command(flatten)]
    pub mode: EmergeModeArgs,

    #[command(flatten)]
    pub merge_flags: MergeFlags,

    #[command(flatten)]
    pub depgraph_flags: DepgraphFlags,

    #[command(flatten)]
    pub activity: ActivityArgs,

    /// Privilege backend for this merge
    #[arg(long, value_enum, default_value_t = Privilege::Auto, env = "EM_PRIVILEGE")]
    pub privilege: Privilege,

    /// Atoms, package sets (`@world`), or ebuild paths to act on
    #[arg(value_name = "ATOM")]
    pub atoms: Vec<String>,
}

#[derive(Subcommand)]
pub enum MaintCommand {
    #[command(about = "Run all maintenance tasks")]
    All,
    #[command(about = "Generate binary package metadata index")]
    Binhost,
    #[command(about = "Inspect/verify/prune local binary packages (em-only, no emaint equivalent)")]
    Binpkg {
        #[command(subcommand)]
        action: BinpkgAction,
    },
    #[command(about = "Discard stale config tracker entries")]
    Cleanconfmem,
    #[command(about = "Discard saved resume lists")]
    Cleanresume {
        /// Actually delete the saved resume/resume-backup lists (default:
        /// just report what's there).
        #[arg(short, long)]
        fix: bool,
    },
    #[command(about = "Clean old Portage build logs")]
    Logs,
    #[command(about = "Scan for and fix failed merges")]
    Merges,
    #[command(about = "Apply package moves to binary packages")]
    Movebin,
    #[command(about = "Apply package moves to installed packages")]
    Moveinst,
    #[command(about = "Regenerate profiles/use.local.desc from metadata.xml")]
    RegenUse {
        /// Write output here instead of profiles/use.local.desc ('-' for stdout)
        #[arg(short, long, value_name = "PATH")]
        output: Option<String>,
    },
    #[command(about = "Purge repo revision history from repo_revisions")]
    Revisions {
        /// Purge only these repos (default: all)
        #[arg(value_name = "REPO")]
        repos: Vec<String>,
    },
    /// Same as `em sync` — shared implementation
    #[command(about = "Sync repositories (git, rsync)")]
    Sync {
        /// Repo names from repos.conf (default: auto-sync enabled repos)
        repos: Vec<String>,
    },
    #[command(about = "Check (and optionally fix) problems in the world file")]
    World {
        /// Remove orphaned entries from the world file
        #[arg(short, long)]
        fix: bool,
    },
}

/// `em maint binpkg <action>` — local `PKGDIR` maintenance built on the `Packages`
/// index/reader substrate
///
/// No real-portage `emaint` module exists for this (only `emaint binhost`, which just
/// regenerates the index); this is an em-only extension.
#[derive(Subcommand)]
pub enum BinpkgAction {
    #[command(about = "Check each indexed binpkg's size/MD5/SHA1 against the file on disk")]
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
    #[command(about = "List indexed binary packages (cpv, build-id, size, path)")]
    List,
    #[command(about = "Keep only the newest BUILD_ID per package, deleting older ones")]
    Prune {
        /// Report what would be deleted without deleting or reindexing
        #[arg(long)]
        dry_run: bool,
    },
    #[command(about = "Print the build-env key for the current roots' make.conf flags")]
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
    #[command(about = "Import an armored OpenPGP public key into the GPG verify keyring")]
    GpgImport {
        /// Path to an armored public-key file (e.g. exported via
        /// `gpg --armor --export <key-id>`).
        keyfile: camino::Utf8PathBuf,
    },
}

/// `em select <module>` — native, eselect-like config selectors
#[derive(Subcommand)]
pub enum SelectCommand {
    #[command(about = "Select the system/sysroot profile (cross-aware)")]
    Profile {
        #[command(subcommand)]
        action: ProfileAction,
    },
    #[command(
        visible_alias = "repos",
        about = "Manage local repositories (overlays)"
    )]
    Repository {
        #[command(subcommand)]
        action: RepositoryAction,
    },
    #[command(
        visible_alias = "gcc",
        about = "Select the active compiler profile (gcc-config/eselect gcc workalike)"
    )]
    Compiler {
        #[command(subcommand)]
        action: CompilerAction,
    },
    #[command(
        about = "Select the active binutils profile (binutils-config/eselect binutils workalike)"
    )]
    Binutils {
        #[command(subcommand)]
        action: BinutilsAction,
    },
    #[command(about = "Select the active linker profile")]
    Linker {
        #[command(subcommand)]
        action: LinkerAction,
    },
    #[command(about = "Select the active LLVM/clang slot")]
    Clang {
        #[command(subcommand)]
        action: ClangAction,
    },
    #[command(about = "Select the pkg-config backend and create the <CTARGET>-pkg-config wrapper")]
    Pkgconf {
        #[command(subcommand)]
        action: PkgconfAction,
    },
    #[command(
        visible_alias = "mirror",
        about = "Manage Gentoo distfile mirrors (mirrorselect workalike)"
    )]
    Mirrors {
        #[command(subcommand)]
        action: MirrorAction,
    },
    #[command(about = "Read/manage GLEP 42 news items (eselect news workalike)")]
    News {
        #[command(subcommand)]
        command: Option<NewsCommand>,
    },
    #[command(about = "Check/fix Gentoo Linux Security Advisories (glsa-check workalike)")]
    Glsa {
        #[command(subcommand)]
        command: Option<GlsaCommand>,
    },
}

/// `em select profile <action>`
#[derive(Subcommand)]
pub enum ProfileAction {
    #[command(about = "List available profiles (marks the current one)")]
    List,
    #[command(about = "Show the current profile")]
    Show,
    #[command(about = "Set the profile by list number or path (cross-aware: no arch check)")]
    Set {
        /// Profile list number (from `list`) or path (e.g. `default/linux/riscv/23.0/rv64/lp64d`)
        target: String,
    },
}

/// `em select repository <action>` — local repos only (remote sync is a TODO)
#[derive(Subcommand)]
pub enum RepositoryAction {
    #[command(about = "List configured repositories")]
    List,
    #[command(about = "Register an existing local repository")]
    Add {
        /// Repository name
        name: String,
        /// Existing local path to the repository
        location: String,
    },
    #[command(visible_alias = "rm", about = "Remove a repository's repos.conf entry")]
    Remove {
        /// Repository name
        name: String,
    },
    #[command(about = "Create a new local overlay (skeleton + repos.conf entry)")]
    Create {
        /// Repository name
        name: String,
        /// Location (default: `<config-root>/var/db/repos/<name>`)
        location: Option<String>,
    },
}

/// `em select compiler <action>` — gcc-config workalike
#[derive(Subcommand)]
pub enum CompilerAction {
    #[command(about = "List available compiler profiles")]
    List {
        /// Target tuple (CTARGET) to list profiles for
        #[arg(short, long)]
        target: Option<String>,
    },
    #[command(about = "Show the current compiler profile")]
    Show {
        /// Target tuple (CTARGET) to show profile for
        #[arg(short, long)]
        target: Option<String>,
    },
    #[command(about = "Set the active compiler profile")]
    Set {
        /// Compiler profile to activate (e.g., `riscv64-unknown-linux-gnu-16` or `1` for list number)
        profile: String,
        /// Target tuple (CTARGET) for cross-compiler selection
        #[arg(short, long)]
        target: Option<String>,
    },
}

/// `em select binutils <action>` — binutils-config workalike
#[derive(Subcommand)]
pub enum BinutilsAction {
    #[command(about = "List available binutils profiles")]
    List {
        /// Target tuple (CTARGET) to list profiles for
        #[arg(short, long)]
        target: Option<String>,
    },
    #[command(about = "Show the current binutils profile")]
    Show {
        /// Target tuple (CTARGET) to show profile for
        #[arg(short, long)]
        target: Option<String>,
    },
    #[command(about = "Set the active binutils profile")]
    Set {
        /// Binutils profile to activate (e.g., `riscv64-unknown-linux-gnu-2.46.0` or `1` for list number)
        profile: String,
        /// Target tuple (CTARGET) for cross-binutils selection
        #[arg(short, long)]
        target: Option<String>,
    },
}

/// `em select linker <action>` — linker profile selection
#[derive(Subcommand)]
pub enum LinkerAction {
    #[command(about = "List available linker profiles")]
    List {
        /// Target tuple (CTARGET) to list profiles for
        #[arg(short, long)]
        target: Option<String>,
    },
    #[command(about = "Show the current linker profile")]
    Show {
        /// Target tuple (CTARGET) to show profile for
        #[arg(short, long)]
        target: Option<String>,
    },
    #[command(about = "Set the active linker profile")]
    Set {
        /// Linker profile to activate (e.g., `riscv64-unknown-linux-gnu-lld-18` or `1` for list number)
        profile: String,
        /// Target tuple (CTARGET) for cross-linker selection
        #[arg(short, long)]
        target: Option<String>,
    },
}

/// `em select clang <action>` — LLVM/clang slot selection
#[derive(Subcommand)]
pub enum ClangAction {
    #[command(about = "List available LLVM/clang slots")]
    List,
    #[command(about = "Show the current LLVM/clang slot")]
    Show,
    #[command(about = "Set the active LLVM/clang slot")]
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
#[derive(Subcommand)]
pub enum PkgconfAction {
    #[command(about = "List available pkg-config backends (pkgconf, pkg-config)")]
    List {
        /// Target tuple (CTARGET) to show the wrapper for
        #[arg(short, long)]
        target: Option<String>,
    },
    #[command(about = "Show the backend the <target>-pkg-config wrapper currently points at")]
    Show {
        /// Target tuple (CTARGET) to show the wrapper for
        #[arg(short, long)]
        target: Option<String>,
    },
    #[command(about = "Create/update the <target>-pkg-config wrapper")]
    Set {
        /// Backend to wrap (`pkgconf`, `pkg-config`, or a list number from `list`)
        backend: String,
        /// Target tuple (CTARGET) to create the wrapper for
        #[arg(short, long)]
        target: Option<String>,
    },
}

/// `em select mirrors <action>` — mirrorselect workalike for `GENTOO_MIRRORS`
#[derive(Subcommand)]
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
        #[arg(value_name = "URL")]
        urls: Vec<String>,
        /// Use every mirror in this ISO country code
        #[arg(short, long)]
        country: Option<String>,
        /// Use every mirror in this region
        #[arg(short, long)]
        region: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum PkgCommand {
    #[command(about = "Edit per-package USE flags in package.use")]
    Use {
        /// Package atom (e.g. sys-boot/grub or >=dev-libs/foo-1.0)
        atom: String,
        /// Add flags (written verbatim, e.g. truetype) — euse calls this
        /// --enable/-E
        #[arg(
            short = 'a',
            long = "add",
            visible_short_alias = 'E',
            value_name = "FLAG"
        )]
        add: Vec<String>,
        /// Subtract flags (written with leading '-', e.g. -themes) — euse
        /// calls this --disable/-D
        #[arg(
            short = 's',
            long = "subtract",
            visible_short_alias = 'D',
            value_name = "FLAG"
        )]
        subtract: Vec<String>,
        /// Drop flags entirely (removes both flag and -flag forms) — euse
        /// calls this --remove/-R or --prune/-P
        #[arg(
            short = 'd',
            long = "drop",
            visible_short_aliases = ['R', 'P'],
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
            conflicts_with_all = ["add", "subtract", "drop"]
        )]
        info: Vec<String>,
        /// Target file inside package.use/ (default: `<cat>-<pkg>`)
        #[arg(long, value_name = "FILE")]
        path: Option<camino::Utf8PathBuf>,
    },
    #[command(about = "Edit per-package keywords in package.accept_keywords")]
    Keyword {
        /// Package atom (e.g. sys-boot/grub or >=dev-libs/foo-1.0)
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
        #[arg(long, value_name = "FILE")]
        path: Option<camino::Utf8PathBuf>,
    },
    #[command(about = "Add/remove a package from package.mask")]
    Mask {
        /// Package atom (e.g. sys-boot/grub or >=dev-libs/foo-1.0)
        atom: String,
        /// Add the atom to package.mask
        #[arg(short = 'a', long = "add")]
        add: bool,
        /// Remove the atom from package.mask
        #[arg(short = 'd', long = "drop")]
        drop: bool,
        /// Target file inside package.mask/ (default: `<cat>-<pkg>`)
        #[arg(long, value_name = "FILE")]
        path: Option<camino::Utf8PathBuf>,
    },
    #[command(about = "Edit per-package env files in package.env")]
    Env {
        /// Package atom (e.g. sys-boot/grub or >=dev-libs/foo-1.0)
        atom: String,
        /// Add env file name(s) (from `/etc/portage/env/`) to apply to this package
        #[arg(short = 'a', long = "add", value_name = "ENVFILE")]
        add: Vec<String>,
        /// Drop env file name(s) from this package's entry
        #[arg(short = 'd', long = "drop", value_name = "ENVFILE")]
        drop: Vec<String>,
        /// Target file inside package.env/ (default: `<cat>-<pkg>`)
        #[arg(long, value_name = "FILE")]
        path: Option<camino::Utf8PathBuf>,
    },
}

#[derive(Subcommand)]
pub enum QueryCommand {
    #[command(about = "Find which package owns a file", alias = "b")]
    Belongs {
        /// File path(s) to look up in the VDB contents records
        #[arg(required = true)]
        file: Vec<String>,
    },
    #[command(about = "Verify checksums of installed package", alias = "k")]
    Check {
        /// Installed package atom(s) to verify
        #[arg(required = true)]
        atom: Vec<String>,
    },
    #[command(about = "List packages depending on an atom", alias = "d")]
    Depends {
        /// Atom(s) whose dependents to list
        #[arg(required = true)]
        atom: Vec<String>,
    },
    #[command(about = "Display full dependency tree", alias = "g")]
    Depgraph {
        /// Atom(s) to resolve and display the dependency tree for
        #[arg(required = true)]
        atom: Vec<String>,
        /// Output format
        #[arg(long, short, value_enum, default_value = "pretty")]
        format: DepgraphFormat,
        /// Let the solver choose USE flags to satisfy REQUIRED_USE (Level C)
        #[arg(long)]
        autosolve_use: bool,
        #[command(flatten)]
        depgraph_flags: DepgraphFlags,
        /// Treat every atom as not-yet-installed (emerge's `-e`/`--emptytree`)
        #[arg(short = 'e', long)]
        emptytree: bool,
        /// Only show dependencies, excluding the given atoms themselves from the tree
        #[arg(short = 'o', long)]
        onlydeps: bool,
        /// Include build-time dependencies (BDEPEND) in the resolution
        #[arg(long)]
        with_bdeps: bool,
        /// emerge's `--root-deps[=rdeps]`: only require RDEPEND (not DEPEND)
        /// to be satisfiable in the merge target.
        #[arg(long = "root-deps")]
        root_deps: bool,
    },
    #[command(about = "List files installed by a package", alias = "f")]
    Files {
        /// Atom(s) whose installed file list to show
        #[arg(required = true)]
        atom: Vec<String>,
    },
    #[command(about = "List installed packages by a VDB field value", alias = "a")]
    Has {
        /// VDB field to match, e.g. `SLOT`, `USE`, `repository`
        field: String,
        /// Value the field must contain; omit to list every package whose
        /// field is set at all
        value: Option<String>,
    },
    #[command(about = "List packages with a given USE flag in IUSE", alias = "h")]
    Hasuse {
        /// USE flag name(s) to search for in IUSE
        #[arg(required = true)]
        flag: Vec<String>,
    },
    #[command(about = "Display keyword status across architectures", alias = "y")]
    Keywords {
        /// Atom(s) to show keyword status for
        #[arg(required = true)]
        atom: Vec<String>,
    },
    #[command(about = "List installed/available packages matching a pattern")]
    List {
        /// List only installed packages (from VDB), not available ones
        #[arg(short = 'I', long = "installed")]
        installed: bool,
        /// Glob or substring pattern(s); omit to list all packages
        #[arg()]
        pattern: Vec<String>,
    },
    #[command(
        about = "Display package metadata (maintainer, homepage, etc.)",
        alias = "m"
    )]
    Meta {
        /// Atom(s) whose metadata to display
        #[arg(required = true)]
        atom: Vec<String>,
    },
    #[command(about = "Display total file size of a package", alias = "s")]
    Size {
        /// Atom(s) whose installed file size to sum
        #[arg(required = true)]
        atom: Vec<String>,
    },
    #[command(about = "Display USE flags for a package", alias = "u")]
    Uses {
        /// Atom(s) whose USE flags to display
        #[arg(required = true)]
        atom: Vec<String>,
    },
    #[command(about = "Print full path to the ebuild for a package", alias = "w")]
    Which {
        /// Atom(s) to resolve to an ebuild path
        #[arg(required = true)]
        atom: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum CleanTarget {
    #[command(
        about = "Remove distfiles no ebuild references",
        alias = "distfiles",
        alias = "d"
    )]
    Dist {
        #[command(flatten)]
        opts: CleanOpts,
    },
    #[command(
        about = "Remove binary packages no ebuild references",
        alias = "packages",
        alias = "p"
    )]
    Pkg {
        #[command(flatten)]
        opts: CleanOpts,
    },
}

/// Filters shared by both clean targets
///
/// Deliberately narrower than `eclean`'s: the destructive/interactive modes it
/// grew are covered here by the global `-p` plus `--deep`, and everything else
/// it offers is a filter on the same candidate set.
#[derive(clap::Args, Clone, Debug, Default)]
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

#[derive(Subcommand)]
pub enum NewsCommand {
    #[command(about = "Count unread news items")]
    Count,
    #[command(about = "List news items")]
    List,
    #[command(
        about = "Read news items (numbers/names from `list`; \"new\"/\"all\", or none for all unread)"
    )]
    Read {
        /// Item numbers/names from `list`, the single keyword "new" (every
        /// unread item) or "all" (every item), or omit for "new".
        ids: Vec<String>,
    },
    #[command(about = "Purge read news items")]
    Purge,
}

#[derive(Subcommand)]
pub enum GlsaCommand {
    #[command(about = "List all GLSAs")]
    List,
    #[command(about = "Check for affected GLSAs")]
    Check {
        /// GLSA id(s) to check (default: every GLSA in the repo)
        ids: Vec<String>,
    },
    #[command(about = "Apply a GLSA fix")]
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
#[derive(Subcommand)]
pub enum ActiveCommand {
    /// Show the registered active context (default when no subcommand)
    #[command(about = "Show the registered active prefix/local")]
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
    /// Note: `em --local active set` is wrong — clap takes `active` as the
    /// `--local` path. Use `em --local=` or pass an explicit directory.
    #[command(about = "Register --prefix/--local as active or activate an existing entry")]
    Set {
        /// Reference to an existing entry (name, index, or path) to activate
        ///
        /// If not provided, creates a new entry from --prefix/--local flags.
        #[arg(value_name = "REF")]
        reference: Option<String>,
    },
    /// Clear the registered active context
    ///
    /// Use `--all` to remove all entries, not just the active pointer.
    #[command(about = "Clear the active context (or all entries with --all)")]
    Clear {
        /// Clear all entries, not just the active pointer
        #[arg(long)]
        all: bool,
    },
    /// Print shell exports for `eval "$(em active env)"` (PATH + markers)
    #[command(about = "Print shell exports for the active context")]
    Env,
    /// List all registered entries
    #[command(about = "List all registered prefix/local entries")]
    List,
    /// Add a new entry without activating it
    ///
    /// Examples:
    ///   `em --prefix /home/me/prefix active add my-prefix`
    ///   `em --local /home/me/.gentoo active add my-gentoo`
    ///   `em --local= active add`  # adds ~/.gentoo with auto-generated name
    #[command(about = "Add a new prefix/local entry")]
    Add {
        /// Optional name for the entry. If not provided, uses path basename
        #[arg(value_name = "NAME")]
        name: Option<String>,
    },
    /// Remove an entry by name, index, or path
    ///
    /// Examples:
    ///   `em active remove my-name`
    ///   `em active remove 0`           # by index
    ///   `em active remove /path/to/dir` # by exact path
    #[command(about = "Remove a registered entry")]
    Remove {
        /// Reference to the entry to remove (name, index, or path)
        #[arg(value_name = "REF")]
        reference: String,
    },
}

#[derive(Subcommand)]
pub enum LogCommand {
    #[command(about = "Show currently running merges")]
    Current,
    #[command(about = "Show recent merge history from activity JSONL")]
    List {
        /// Max rows (default 20)
        limit: Option<u32>,
    },
    #[command(about = "Show merge times for a package (or global median)")]
    Time {
        /// Package name/atom substring to filter by; omit for the global median
        atom: Option<String>,
    },
    #[command(about = "ETA for remainder of a live activity session")]
    Predict,
}

/// How an unprivileged build gets root for `chown`/setuid (see `--privilege`)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
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
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum DepgraphFormat {
    /// emerge -p style pretend output
    Pretty,
    /// Machine-parsable JSON
    Json,
    /// cargo tree style dependency tree
    Tree,
}

fn parse_arch(s: &str) -> std::result::Result<Arch, String> {
    Arch::from_str(s).map_err(|e| e.to_string())
}
