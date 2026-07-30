use std::str::FromStr;

use clap::builder::styling::{AnsiColor as ClapAnsiColor, Styles};
use clap::{Parser, Subcommand};
use gentoo_core::Arch;
#[cfg(test)]
use portage_atom_pubgrub::DepClass;
use portage_resolve::Roots;

mod activity;
mod depgraph_flags;
mod merge_flags;
pub use activity::ActivityArgs;
pub use depgraph_flags::DepgraphFlags;
pub use merge_flags::MergeFlags;

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

    #[command(flatten)]
    pub depgraph_flags: DepgraphFlags,

    /// Show what would be done without actually performing any actions.
    #[arg(short = 'p', long, global = true)]
    pub pretend: bool,

    /// Activity-output flags (`--activity-fd`/`--activity-jsonl`/`--emergelog`)
    /// for the merge path. Flattened (not `global = true`) so they only appear
    /// on commands that drive an activity bus; the merge path reads the
    /// applet-merged set via [`Cli::effective_activity`].
    #[command(flatten)]
    pub activity: ActivityArgs,

    /// Increase verbosity: `-v` labels each build phase, `-vv`/`-vvv` add
    /// `em`'s own debug/trace logs (see also `RUST_LOG`).
    #[arg(short = 'v', long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Suppress non-error output.
    #[arg(short = 'q', long, global = true)]
    pub quiet: bool,

    /// Target architecture for operations. Defaults to current system architecture.
    #[arg(long, value_name = "ARCH", default_value_t = Arch::current(), value_parser = parse_arch)]
    pub arch: Arch,

    /// Pin search/query to a single repository. When unset, repositories are
    /// auto-discovered from `repos.conf` (the main repo wins for single-repo
    /// applets; search walks all of them).
    #[arg(long, value_name = "PATH")]
    pub repo: Option<String>,

    /// Unprivileged offset: ROOT/VDB/distfiles/build trees under DIR; config
    /// still from the host (use --root for a config offset).
    #[arg(long, value_name = "DIR", global = true)]
    pub prefix: Option<String>,

    /// Unprivileged, standalone Gentoo-Prefix: own VDB/BROOT/config, not
    /// overlaid on the host (see --prefix for the overlay). Defaults to
    /// ~/.gentoo (EPREFIX=~/.gentoo) when no DIR is given.
    #[arg(long, global = true, num_args = 0..=1, default_missing_value = "", value_name = "DIR")]
    pub local: Option<String>,

    /// How an unprivileged build gets root for chown/setuid: auto (best
    /// compiled-in fake root), pseudoroot, fakeroost, hakoniwa (userns mapped
    /// root), sudo (real root), or none; backends unsupported on this platform
    /// are compiled out. Ignored when already root.
    ///
    /// Not `global`: it is read at process start ([`crate::privilege`]'s
    /// supervisor re-exec, before dispatch) from the top-level value; the
    /// staged-build applets (`crossdev`/`toolchain`/`stages`) carry their own
    /// optional override, merged by [`Cli::effective_privilege`].
    #[arg(long, value_enum, default_value_t = Privilege::Auto, env = "EM_PRIVILEGE")]
    pub privilege: Privilege,

    /// Search package names (each argument is a pattern).
    #[arg(short = 's', long)]
    pub search: bool,

    /// Search package names and descriptions.
    #[arg(short = 'S', long)]
    pub searchdesc: bool,

    /// Skip dependency resolution and only merge specified packages.
    #[arg(short = 'O', long)]
    pub nodeps: bool,

    /// Remove the matching installed packages completely, without regard to
    /// dependencies. Matches every installed slot/version of each atom. For
    /// removing unneeded dependencies too, use `depclean` instead.
    #[arg(short = 'C', long)]
    pub unmerge: bool,

    /// Remove installed packages that are not needed by @world (with no
    /// atoms, cleans everything unreachable; with atoms, only considers
    /// removing those, protecting everything else). Unlike `-C`, this
    /// walks the installed dependency graph first — matches real emerge's
    /// safe alternative to `-C`.
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

    /// Resume the last saved merge (see `em maint cleanresume` to discard
    /// it instead). Atoms are not accepted together with this flag — the
    /// package list comes from the saved state. Combine with other flags
    /// (e.g. `-r --keep-going`, `-r -X stuck/atom`) to adjust the resumed
    /// run.
    #[arg(short = 'r', long)]
    pub resume: bool,

    #[command(flatten)]
    pub merge_flags: MergeFlags,

    /// Installation root (the offset all applets install into / query).
    #[arg(long, env = "ROOT", value_name = "PATH", global = true)]
    pub root: Option<String>,

    /// Read config (profile, make.conf) from this root instead of `--root`.
    #[arg(long, value_name = "PATH", global = true)]
    pub config_root: Option<String>,

    /// Override VDB path (default: $ROOT/var/db/pkg)
    #[arg(long, value_name = "PATH", global = true)]
    pub vdb: Option<String>,

    /// Cross-build/setup for a crossdev target tuple. The single source for
    /// "which tuple" everywhere: `em --target T crossdev --init-target`
    /// sets T up; `em --target T stages --stage1` (or any plain atom build)
    /// resolves/installs into the target sysroot `<EROOT>/usr/<TUPLE>` (the
    /// crossdev `<TUPLE>-emerge` entry point) — sugar for `--config-root
    /// <sysroot> --root <sysroot>`, with the cross context (CHOST/CBUILD,
    /// `--root-deps=rdeps`) read from the sysroot make.conf. One flag for
    /// both roles, not two that can disagree — `crossdev` no longer has its
    /// own `-t`/`--target`.
    #[arg(long, short = 'T', value_name = "TUPLE", global = true)]
    pub target: Option<String>,

    #[command(subcommand)]
    pub applet: Option<Applet>,

    #[arg(num_args = 1..)]
    pub atoms: Vec<String>,
}

/// The user's home directory from `$HOME`, falling back to `/root` only if
/// unset (matching how unprivileged tools resolve `~`).
fn home_dir() -> camino::Utf8PathBuf {
    crate::xdg::home()
}

/// Topology after resolving CLI flags + optional `em active` registration.
///
/// Explicit `--local` / `--prefix` / `--root` always win. When none are set,
/// a previously registered active context (see [`crate::active`]) supplies
/// prefix/local so bare `em <pkg>` dogfooding needs no per-invocation flags.
enum TopologySource {
    Local(camino::Utf8PathBuf),
    Prefix(camino::Utf8PathBuf),
    Root(camino::Utf8PathBuf),
    Host,
}

// The four filesystem roles (docs/root-topology.md § "The four roles"),
// collapsed by how many coincide. `Cli::base_roots()` (BROOT view) and
// `Cli::roots()` (install-target view) both derive from the same
// `Cli::root_set()`, so they can't drift independently.
// Root topology refactoring is tracked in todo/root-topology-refactor.md.
enum RootSet {
    /// All four roles collapse to one path: the bare invocation, or
    /// `--local` (a standalone Gentoo-Prefix owns its own BROOT too).
    Single { root: camino::Utf8PathBuf },
    /// BROOT distinct from the install target. `--root R`: BROOT is always
    /// the real host `/` (portage `ROOT=`/`{target}-emerge` parity) — an
    /// offset install borrows the host's own BDEPEND tools; it does not
    /// need its own copy of them.
    #[allow(dead_code)] // target isn't read yet: base_roots()/roots() keep their
    // own, separate "outer EROOT" derivation (see broot()'s doc comment on
    // why that's a different question from BROOT). Kept here to match
    // docs/root-topology.md's proposed shape for the fuller migration.
    // Root topology refactoring is tracked in todo/root-topology-refactor.md.
    Dual {
        broot: camino::Utf8PathBuf,
        target: camino::Utf8PathBuf,
    },
    /// BROOT, base (build-against sysroot), and target all distinct.
    /// `--prefix P`: broot = base = the host `/` (the overlay borrows host
    /// tools and builds against them), target = P.
    #[allow(dead_code)] // base/target aren't read yet, same reason as Dual above.
    Overlayed {
        broot: camino::Utf8PathBuf,
        base: camino::Utf8PathBuf,
        target: camino::Utf8PathBuf,
    },
}

impl RootSet {
    /// Where `BDEPEND` tools run and are checked against (BROOT).
    fn broot(&self) -> &camino::Utf8Path {
        match self {
            RootSet::Single { root } => root,
            RootSet::Dual { broot, .. } | RootSet::Overlayed { broot, .. } => broot,
        }
    }
}

/// `s.as_deref()` parsed as a path, or `None`.
fn opt_path(s: &Option<String>) -> Option<camino::Utf8PathBuf> {
    s.as_deref().map(camino::Utf8PathBuf::from)
}

impl Cli {
    /// Resolve topology from explicit flags, else the `em active` registration.
    ///
    /// Precedence: `--local` > `--prefix` > `--root` > active state > bare host.
    /// Active state is only consulted when no root-topology flag is present, so
    /// `em --root R …` never accidentally inherits a registered prefix.
    fn topology_source(&self) -> TopologySource {
        if let Some(local) = &self.local {
            let root = if local.is_empty() {
                home_dir().join(".gentoo")
            } else {
                camino::Utf8PathBuf::from(local)
            };
            return TopologySource::Local(root);
        }
        if let Some(prefix) = opt_path(&self.prefix) {
            return TopologySource::Prefix(prefix);
        }
        if let Some(root) = opt_path(&self.root) {
            return TopologySource::Root(root);
        }
        match crate::active::load_active_context() {
            Ok(Some(ctx)) => match ctx.kind {
                crate::active::ActiveKind::Local => TopologySource::Local(ctx.path),
                crate::active::ActiveKind::Prefix => TopologySource::Prefix(ctx.path),
            },
            // Missing or unreadable state → bare host (same as no registration).
            _ => TopologySource::Host,
        }
    }

    /// The root model (docs/root-topology.md) from `--local`/`--prefix`/
    /// `--root` (or the active registration), before config/overlay concerns.
    fn root_set(&self) -> RootSet {
        let host = camino::Utf8PathBuf::from("/");
        match self.topology_source() {
            TopologySource::Local(root) => RootSet::Single { root },
            TopologySource::Prefix(target) => RootSet::Overlayed {
                broot: host.clone(),
                base: host,
                target,
            },
            TopologySource::Root(target) => RootSet::Dual {
                broot: host,
                target,
            },
            TopologySource::Host => RootSet::Single { root: host },
        }
    }
}

impl Cli {
    /// Resolve the root model (docs/root-topology.md) from the global flags.
    ///
    /// `--target <tuple>` layers on top of the base model: it targets the crossdev
    /// sysroot `<EROOT>/usr/<tuple>` as both config-root and root (crossdev's
    /// `PORTAGE_CONFIGROOT == ROOT == SYSROOT`). The `<EROOT>` it sits under still
    /// comes from `--local`/`--prefix`/`--root`, so `em --local --target <t>`
    /// targets `~/.gentoo/usr/<t>`.
    ///
    /// Under `--prefix`, the returned `Roots`'s `merge_root()` is the **prefix**
    /// (install destination), while `base_roots()` returns a separate view whose
    /// `merge_root()` is the **host `/`** (BROOT, for BDEPEND checks). The two
    /// genuinely differ for an overlay; this split is what lets preflight check
    /// BDEPEND against the host while the merge lands in the prefix.
    pub fn roots(&self) -> Roots {
        // --target: layer the sysroot on top of the overlay target (the prefix),
        // not base_roots's BROOT (host /). Under --prefix the cross sysroot is
        // <prefix>/usr/<tuple>, and base_roots's merge_root is the host — so
        // derive the sysroot from the overlay's prefix (eprefix) when set.
        let Some(tuple) = self.target.as_deref() else {
            return self.outer_roots();
        };
        // The outer EROOT the sysroot sits under: the overlay prefix when set
        // (--prefix), else the offset (--root) or host / (bare) — never
        // `base_roots()`/`roots()` directly, which would double-apply this
        // same substitution if called recursively; `outer_roots()` is always
        // the pre-substitution view.
        let outer = self.outer_roots();
        let eroot = outer.merge_root().to_owned();
        let sysroot = eroot.join("usr").join(tuple);
        Roots::default()
            .with_config(Some(sysroot.clone()))
            .with_base(Some(sysroot.clone()))
            .with_target(Some(sysroot))
            // BROOT never moves with `--target`: BDEPEND always resolves on
            // the true build host, carried over from the outer (pre-
            // substitution) view rather than left as the sysroot itself.
            .with_broot(outer.broot().map(|p| p.to_owned()))
            // `--target` is crossdev's cross-tuple flag; every real
            // invocation of it is a foreign-arch build (a same-arch use
            // would just be `--root`). No `IDepend` caller exists yet to
            // need finer CHOST/CBUILD-derived precision than this.
            .with_cross_arch(true)
            // Preserve the outer overlay identity: under `--prefix`/`--local`,
            // distfiles and work trees live under the outer EROOT (via
            // eprefix + relocate), and user config under `config_overlay`
            // (`P/etc/portage`). Clearing these forced host `/var/cache/
            // distfiles` and dropped overlay package.use for target builds.
            // eprefix stays the *outer* prefix path so relocate anchors there
            // rather than under the sysroot (`P/usr/T/...`).
            .with_eprefix(outer.eprefix().map(|p| p.to_owned()))
            .with_config_overlay(outer.config_overlay().map(|p| p.to_owned()))
            .with_relocate(outer.relocate())
            .with_config_root_explicit(outer.config_root_explicit().map(|p| p.to_owned()))
    }

    /// The root view with any `--target` sysroot substitution undone: what
    /// [`roots`](Self::roots) returns when `--target` isn't set, computed
    /// **unconditionally** regardless of whether `self.target` happens to
    /// also be set. This is the "outer EROOT" — `--local`/`--prefix`'s
    /// prefix, `--root`'s offset, or host `/` — that every crossdev *setup*
    /// action (`crossdev/mod.rs`: `sysroot`, `setup_root`,
    /// `ensure_self_contained_prefix`, `ensure_prefix_profile`, `main_repo`,
    /// and `setup()`/`toolchain()`'s own top-level checks) must anchor to
    /// instead of `roots()`. Using `roots()` there was a real bug: if
    /// `--target T` happens to also be set on the same invocation as
    /// `crossdev -t T --init-target`, `roots()` is *already* the sysroot,
    /// so appending `usr/T` again doubly-nested it
    /// (`<EROOT>/usr/T/usr/T` instead of `<EROOT>/usr/T`) — reproduced live.
    // Root topology refactoring details are in todo/root-topology-refactor.md.
    ///
    /// `stage1()`/`profile_stack()`/`resolve_gcc_version` deliberately keep
    /// using plain `roots()` — those genuinely want `--target`'s sysroot
    /// substitution (`em --target T stages --stage1` builds *into* the
    /// sysroot, by design).
    pub(crate) fn outer_roots(&self) -> Roots {
        let base = self.base_roots();
        if let Some(prefix) = base.eprefix().filter(|_| base.is_overlay()) {
            let prefix = prefix.to_path_buf();
            return Roots::default()
                .with_config(base.config().map(|p| p.to_owned()))
                .with_base(None)
                .with_target(Some(prefix.clone()))
                .with_broot(base.broot().map(|p| p.to_owned()))
                .with_cross_arch(base.is_cross_arch())
                .with_eprefix(Some(prefix.clone()))
                .with_config_overlay(Some(prefix.join("etc/portage")))
                .with_relocate(true)
                .with_config_root_explicit(base.config_root_explicit().map(|p| p.to_owned()));
        }
        base
    }

    /// The root model from `--local`/`--prefix`/`--root`/`--config-root`, before
    /// any `--target` sysroot override (see [`roots`](Self::roots)). Exposed at
    /// `pub(crate)` so the staged-build driver can install `cross-*` toolchain
    /// packages (which always live in the outer EROOT, never the sysroot
    /// subdirectory — see `crossdev/mod.rs`'s module doc) even from a
    /// `--target`-active invocation.
    ///
    /// `merge_root()` of the returned `Roots` is **the outer EROOT** (with
    /// `--target`'s sysroot substitution undone) — where `bypass_cross_root`
    /// toolchain-install steps land and where `write_cross_env`/
    /// `write_sysroot_config` (`crossdev/mod.rs`) write config. Under
    /// `--prefix` that's the host `/` (the overlay borrows host tools);
    /// under `--local`/`--root` it's the offset itself. **This is not
    /// necessarily BROOT** — for plain `--root` the two differ (BROOT is
    /// always the host, see [`broot`](Self::broot)); they only coincide for
    /// `--prefix`/`--local`, which is why this function used to be (mis)used
    /// for BDEPEND checks too. Use [`broot`](Self::broot) for that.
    pub(crate) fn base_roots(&self) -> Roots {
        let path = opt_path;
        match self.topology_source() {
            // `--local` (or active local): standalone Gentoo-Prefix, own BROOT.
            // Full closure (base == target == the prefix), self-contained VDB.
            // EPREFIX makes installed scripts relocatable (shebangs reference
            // ${EPREFIX}/usr/bin/...). See docs/root-topology.md § "Override
            // semantics".
            TopologySource::Local(prefix) => {
                // Prefer the prefix's own make.profile when present so a
                // bootstrapped `--local` tree is self-hosting; fall back to
                // host config until the first `em --config-root … select
                // profile` (or setup) lands one. Explicit `--config-root`
                // still wins via with_config_root_explicit.
                let prefix_profile = prefix.join("etc/portage/make.profile");
                let config = if self.config_root.is_some() {
                    path(&self.config_root)
                } else if prefix_profile.exists() {
                    Some(prefix.clone())
                } else {
                    None
                };
                Roots::default()
                    .with_config(config)
                    .with_base(Some(prefix.clone()))
                    .with_target(Some(prefix.clone()))
                    .with_broot(Some(prefix.clone()))
                    .with_cross_arch(false)
                    .with_eprefix(Some(prefix.clone()))
                    .with_config_overlay(Some(prefix.join("etc/portage")))
                    .with_relocate(true)
                    .with_config_root_explicit(path(&self.config_root))
            }
            // `--prefix` overlay (or active prefix): BROOT is the host `/`.
            // The prefix is the install destination (target), but
            // base_roots()'s merge_root() must be the host because that's
            // what preflight/bdepend_avail check BDEPEND against. roots()
            // reconstructs the prefix-target view on top of this.
            TopologySource::Prefix(prefix) => Roots::default()
                .with_config(path(&self.config_root))
                .with_base(None)
                .with_target(None) // BROOT = host `/`, NOT the prefix
                .with_broot(Some(camino::Utf8PathBuf::from("/")))
                .with_cross_arch(false)
                .with_eprefix(Some(prefix.clone()))
                .with_config_overlay(Some(prefix.join("etc/portage")))
                .with_relocate(true)
                .with_config_root_explicit(path(&self.config_root)),
            // Bare host or `--root` offset.
            TopologySource::Root(_) | TopologySource::Host => Roots::default()
                // config: --config-root, else host `/` — true portage `ROOT=`
                // parity (`PORTAGE_CONFIGROOT` defaults to `/` regardless of
                // `ROOT`). The 2026-07-09 "own everything" self-contained
                // default (config following `--root` itself) was reverted
                // 2026-07-11: it diverged from real `ROOT=` semantics for no
                // benefit `--root --config-root <same dir>` didn't already
                // give explicitly, and made a bare `--root DIR` behave unlike
                // anything a real emerge user would expect.
                // Root topology details: todo/root-topology-refactor.md.
                .with_config(path(&self.config_root))
                // base: --root; host otherwise.
                .with_base(path(&self.root))
                // target: --root (install destination). This is "the outer
                // EROOT" (bypass_cross_root, write_cross_env/
                // write_sysroot_config in crossdev/mod.rs all rely on this
                // staying the offset for --root) — a DIFFERENT thing from
                // BROOT, see satisfaction_root's doc comment.
                .with_target(path(&self.root))
                .with_broot(Some(self.root_set().broot().to_owned()))
                .with_cross_arch(false)
                .with_eprefix(None)
                .with_config_overlay(None)
                .with_relocate(false)
                .with_config_root_explicit(path(&self.config_root)),
        }
    }

    /// The full `Roots` a `MergeRoot::Host`-stamped plan entry actually
    /// merges into (`merge/mod.rs`'s `entry_roots`) — as opposed to
    /// [`satisfaction_root`](Roots::satisfaction_root), which only gives a
    /// bare path for checking whether one is already satisfied.
    ///
    /// Two different answers depending on privilege:
    /// - `--root` (privileged offset, portage `ROOT=` parity): the real host
    ///   `/`, same as `root_set().broot()` — an unsatisfied Host-routed
    ///   BDEPEND installs there because the invocation has root to do so.
    /// - `--prefix` (unprivileged overlay): the prefix itself
    ///   (`outer_roots()`, whose `merge_root()` is already the promoted
    ///   prefix-target view) — the overlay cannot write the real host `/`,
    ///   so an unsatisfied BDEPEND must land in the prefix instead. Only the
    ///   *satisfaction check* (is it already present) stays host-anchored,
    ///   via `satisfaction_root`/`is_overlay`'s VDB-weave callers.
    /// - `--local`/bare: BROOT already equals the merge root, so the two
    ///   questions coincide.
    pub(crate) fn broot(&self) -> Roots {
        let base = self.base_roots();
        if base.is_overlay() {
            return self.outer_roots();
        }
        let broot = self.root_set().broot().to_owned();
        Roots::default()
            .with_config(base.config().map(|p| p.to_owned()))
            .with_base(Some(broot.clone()))
            .with_target(Some(broot))
            .with_broot(base.broot().map(|p| p.to_owned()))
            .with_cross_arch(base.is_cross_arch())
            .with_eprefix(base.eprefix().map(|p| p.to_owned()))
            .with_config_overlay(base.config_overlay().map(|p| p.to_owned()))
            .with_relocate(base.relocate())
            .with_config_root_explicit(base.config_root_explicit().map(|p| p.to_owned()))
    }

    /// Path used by single-repo applets. Falls back to `/var/db/repos/gentoo`
    /// when neither `--repo` nor `repos.conf` is available.
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

    /// Repositories to walk for `em search`. Honours `--repo` when set;
    /// otherwise returns every entry from `repos.conf` (main first).
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

    /// Effective activity-output flags for the dispatched command: the
    /// applet's own flattened [`ActivityArgs`] (Regen / the crossdev staged
    /// builds) merged over the top-level set, subcommand winning when set —
    /// the same precedence `crossdev::merge_merge_flags` uses for `MergeFlags`.
    /// Applets without their own activity args just get the top-level set.
    pub fn effective_activity(&self) -> ActivityArgs {
        use crate::cli::activity::merge_activity_args_fields;
        let sub = match &self.applet {
            Some(Applet::Regen { activity, .. }) => activity,
            Some(Applet::Crossdev(a)) => &a.activity,
            Some(Applet::Toolchain(a)) => &a.activity,
            Some(Applet::Stages(a)) => &a.activity,
            _ => return self.activity.clone(),
        };
        merge_activity_args_fields(&self.activity, sub)
    }

    /// Effective privilege backend for the dispatched command. `--privilege`
    /// is read at process start (the supervisor re-exec, before dispatch), so
    /// the top-level `Cli::privilege` is the base; the crossdev staged applets
    /// (`crossdev`/`toolchain`/`stages`) carry an optional override that wins
    /// when set, so `em crossdev --setup --privilege sudo` and
    /// `em --privilege sudo crossdev --setup` both land on `sudo`.
    pub fn effective_privilege(&self) -> Privilege {
        let sub = match &self.applet {
            Some(Applet::Crossdev(a)) => &a.privilege,
            Some(Applet::Toolchain(a)) => &a.privilege,
            Some(Applet::Stages(a)) => &a.privilege,
            _ => return self.privilege,
        };
        sub.unwrap_or(self.privilege)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let cli = Cli::parse_from([
            "em",
            "-t",
            "-T",
            "riscv64-unknown-linux-gnu",
            "sys-libs/zlib",
        ]);
        assert!(cli.merge_flags.tree);
        assert_eq!(cli.target.as_deref(), Some("riscv64-unknown-linux-gnu"));
    }

    #[test]
    fn cross_targets_sysroot_under_eroot() {
        // `--target` sits under the `--root` EROOT and pins config == base ==
        // target to `<EROOT>/usr/<tuple>` (PORTAGE_CONFIGROOT == ROOT == SYSROOT).
        let cli = Cli::parse_from([
            "em",
            "--root",
            "/srv/x",
            "--target",
            "riscv64-unknown-linux-gnu",
            "-p",
            "sys-libs/zlib",
        ]);
        let r = cli.roots();
        let sysroot = "/srv/x/usr/riscv64-unknown-linux-gnu";
        assert_eq!(r.config().unwrap().as_str(), sysroot);
        assert_eq!(r.merge_root().as_str(), sysroot);
        assert_eq!(r.base().unwrap().as_str(), sysroot);
        assert_eq!(r.config(), r.target());
    }

    #[test]
    fn cross_defaults_to_root_eroot() {
        // Isolate active state so a developer-registered prefix cannot
        // rewrite bare-host topology under us.
        let (_tmp, _g) = crate::test_support::isolate_active_state();
        // No `--root`: EROOT is `/`, so the sysroot is `/usr/<tuple>`.
        let cli = Cli::parse_from(["em", "--target", "riscv64-unknown-linux-gnu", "-p", "zlib"]);
        assert_eq!(
            cli.roots().merge_root().as_str(),
            "/usr/riscv64-unknown-linux-gnu"
        );
    }

    /// `--prefix P --target T` must keep distfiles/work under P (relocate +
    /// eprefix), not fall back to host paths or nest under the sysroot.
    #[test]
    fn prefix_plus_target_preserves_overlay_relocate() {
        let cli = Cli::parse_from([
            "em",
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
    }

    #[test]
    fn no_cross_keeps_base_roots() {
        let (_tmp, _g) = crate::test_support::isolate_active_state();
        let cli = Cli::parse_from(["em", "-p", "sys-libs/zlib"]);
        let r = cli.roots();
        assert_eq!(r.config(), None);
        assert_eq!(r.merge_root().as_str(), "/");
    }

    /// A registered active `--prefix` is applied when no explicit topology
    /// flag is given (dogfooding path for `em active set`).
    #[test]
    fn active_prefix_applies_when_no_explicit_flag() {
        let (tmp, _g) = crate::test_support::isolate_active_state();
        let prefix = tmp.path().join("pfx");
        std::fs::create_dir_all(&prefix).unwrap();
        let prefix_s = prefix.to_str().unwrap();
        let cli_set = Cli::parse_from(["em", "--prefix", prefix_s, "active", "set"]);
        crate::active::run(
            cli_set.applet.as_ref().and_then(|a| match a {
                Applet::Active { command } => command.as_ref(),
                _ => None,
            }),
            &cli_set,
        )
        .unwrap();

        let bare = Cli::parse_from(["em", "-p", "sys-libs/zlib"]);
        let r = bare.roots();
        let canon = prefix.canonicalize().unwrap();
        let canon_s = canon.to_str().unwrap();
        assert_eq!(r.eprefix().map(|p| p.as_str()), Some(canon_s));
        assert_eq!(r.merge_root().as_str(), canon_s);
        // Overlay: BROOT satisfaction stays the host.
        assert_eq!(bare.base_roots().merge_root().as_str(), "/");
    }

    /// Explicit `--root` wins over a registered active prefix.
    #[test]
    fn explicit_root_overrides_active_prefix() {
        let (tmp, _g) = crate::test_support::isolate_active_state();
        let prefix = tmp.path().join("pfx");
        std::fs::create_dir_all(&prefix).unwrap();
        let prefix_s = prefix.to_str().unwrap();
        let cli_set = Cli::parse_from(["em", "--prefix", prefix_s, "active", "set"]);
        crate::active::run(
            cli_set.applet.as_ref().and_then(|a| match a {
                Applet::Active { command } => command.as_ref(),
                _ => None,
            }),
            &cli_set,
        )
        .unwrap();

        let cli = Cli::parse_from(["em", "--root", "/srv/x", "-p", "sys-libs/zlib"]);
        let r = cli.roots();
        assert_eq!(r.merge_root().as_str(), "/srv/x");
        assert_eq!(
            r.eprefix(),
            None,
            "active prefix must not leak under --root"
        );
    }

    /// `--local` is a standalone prefix: base == target == ~/.gentoo (full
    /// closure, own VDB), not an overlay (base would be the host). Previously
    /// base was None (host) — wrong for cross on a foreign host, where there's
    /// no host VDB to seed the plan. See docs/root-topology.md § "Override
    /// semantics".
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
        let cli = Cli::parse_from(["em", "--local", "-p", "sys-libs/zlib"]);
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
        let cli = Cli::parse_from(["em", "--local", prefix_s, "-p", "sys-libs/zlib"]);
        let r = cli.base_roots();
        assert_eq!(
            r.config().map(|p| p.as_str()),
            Some(prefix_s),
            "--local with make.profile must use the prefix as config root"
        );
    }

    /// `--prefix` sets EPREFIX: the installed tree is relocatable, so ebuilds
    /// bake ${EPREFIX}/usr/bin/pythonX.Y into shebangs. The overlay then
    /// symlinks host python there (setup.rs) to satisfy them without building
    /// a prefix python. See docs/root-topology.md § "Override semantics".
    #[test]
    fn prefix_sets_eprefix_for_relocatable_overlay() {
        let cli = Cli::parse_from(["em", "--prefix", "/opt/p", "-p", "sys-libs/zlib"]);
        let r = cli.base_roots();
        assert_eq!(
            r.eprefix().unwrap().as_str(),
            "/opt/p",
            "--prefix must set EPREFIX (relocatable installed tree)"
        );
        // Overlay: base is the host (None), not the prefix.
        assert_eq!(r.base(), None, "--prefix base is the host (overlay)");
    }

    /// `--prefix` BROOT is the host: `base_roots().merge_root()` (BROOT, where
    /// preflight checks BDEPEND) is `/`, while `roots().merge_root()` (the
    /// actual install target) is the prefix. These two genuinely differ for an
    /// overlay; conflating them made preflight check jinja2's BDEPEND against
    /// the empty prefix VDB instead of the host, failing the build.
    /// See docs/root-topology.md § "Override semantics".
    #[test]
    fn prefix_overlay_broot_is_host_not_prefix() {
        let cli = Cli::parse_from(["em", "--prefix", "/opt/p", "-p", "sys-libs/zlib"]);
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

    /// `--prefix` is an unprivileged overlay: it cannot write the real host
    /// `/`, so an unsatisfied `MergeRoot::Host` plan entry (`entry_roots()`
    /// in `merge/mod.rs`, fed by `Cli::broot()`) must merge into the prefix
    /// instead — unlike `--root`, where the same entry correctly lands on
    /// the real host because that invocation has root. `broot()`'s `.broot`
    /// field (the *satisfaction* root) stays the host either way; only the
    /// merge destination (`merge_root()`) differs here.
    #[test]
    fn prefix_overlay_broot_merges_into_prefix_not_host() {
        let cli = Cli::parse_from(["em", "--prefix", "/opt/p", "-p", "sys-libs/zlib"]);
        let broot = cli.broot();
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

    /// Portage `ROOT=`/`{target}-emerge` parity: `--root R`'s BROOT is the
    /// real host `/`, not `R`. `R` only receives the *install*; BDEPEND
    /// tools run against (and are checked against) the host, exactly like
    /// `--prefix`. Previously `base_roots().merge_root()` was (mis)used for
    /// this and returned `R`, making an offset build check BDEPEND against
    /// the (usually near-empty) offset VDB instead of the host's —
    /// `roots().satisfaction_root(DepClass::BDepend)` is the dedicated
    /// accessor now; `base_roots()` keeps its own, different "outer EROOT"
    /// meaning (see both their doc comments).
    // Root topology refactoring is tracked in todo/root-topology-refactor.md.
    #[test]
    fn root_broot_is_host_not_offset() {
        let cli = Cli::parse_from(["em", "--root", "/srv/x", "-p", "sys-libs/zlib"]);
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

    /// `--local DIR` uses `DIR` directly as the standalone prefix root (not
    /// `DIR/.gentoo` — that expansion only applies to the bare-flag default,
    /// covered by `local_is_standalone_not_overlay`).
    #[test]
    fn local_with_path_uses_dir_directly() {
        let cli = Cli::parse_from(["em", "--local", "/tmp/x", "-p", "sys-libs/zlib"]);
        let r = cli.base_roots();
        assert_eq!(r.base().unwrap().as_str(), "/tmp/x");
        assert_eq!(r.target().unwrap().as_str(), "/tmp/x");
        assert_eq!(r.eprefix().unwrap().as_str(), "/tmp/x");
    }
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)] // __worker carries many CLI strings
pub enum Applet {
    /// Run one do*/new* install helper standalone against the exported build
    /// env. Internal: backs the PATH shims dropped during a build so
    /// `find -exec doman` / `xargs do*` reach helpers that are in-shell
    /// builtins. Not for direct use.
    #[command(name = "__helper", hide = true)]
    Helper {
        /// Helper name (e.g. `doman`, `dolib.a`).
        name: String,
        /// Arguments passed through to the helper.
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
        /// Where BDEPEND-class build tools live (`Cli::broot()`'s merge root).
        #[arg(long)]
        broot: Option<String>,
        /// See `ebuild::RootContext::self_contained_bootstrap`.
        #[arg(long)]
        self_contained_bootstrap: bool,
        /// A pre-built GPKG to merge (`-k`/`-g`).
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
        /// Parent activity session id — live FS phase updates only.
        #[arg(long)]
        activity_job_id: Option<String>,
        #[arg(long)]
        activity_parent_job_id: Option<String>,
        /// Filesystem root of the parent's live activity sink.
        #[arg(long)]
        activity_live_root: Option<String>,
        /// `host` or `target` package side for inflight paths.
        #[arg(long)]
        activity_side: Option<String>,
        /// Unix socket path: stream phase JSONL back to the parent activity bus.
        #[arg(long)]
        activity_reemit_path: Option<String>,
    },

    #[command(about = "Execute ebuild phases")]
    Ebuild {
        #[arg(required = true)]
        ebuild_path: String,
        #[arg(required = true)]
        phase: Vec<String>,
        /// Override the build work directory (default: `/var/tmp/portage/<cat>/<pf>`)
        #[arg(short = 'w', long, value_name = "DIR")]
        work_dir: Option<camino::Utf8PathBuf>,
    },

    #[command(about = "System maintenance and health checks")]
    Maint {
        #[command(subcommand)]
        command: Option<MaintCommand>,
    },

    #[command(about = "Query Portage internal variables and data")]
    Portageq {
        #[arg(required = true)]
        command: String,
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },

    /// Sync ebuild repositories from `repos.conf` (`git` and `rsync`).
    ///
    /// With no names, syncs every entry with `auto-sync = yes` (Portage
    /// default) and a usable `sync-type`/`sync-uri`. Named repos are synced
    /// regardless of `auto-sync`.
    ///
    /// Default backends shell out to `git` / `rsync` (Portage parity). Build
    /// with `--features sync-gix` for the experimental pure-gix git path.
    #[command(about = "Sync repositories (git, rsync)")]
    Sync {
        /// Repo names from repos.conf (default: auto-sync enabled repos)
        repos: Vec<String>,
    },

    #[command(about = "Remove orphaned/unused packages")]
    Depclean {
        #[arg(trailing_var_arg = true)]
        atoms: Vec<String>,
    },

    #[command(about = "Regenerate metadata cache")]
    Regen {
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
    },

    #[command(about = "Create binary packages from installed files")]
    Quickpkg {
        /// Atoms, package sets (`@system`), or VDB paths (`/var/db/pkg/cat/pf`)
        #[arg(required = true)]
        atoms: Vec<String>,
        /// Include CONFIG_PROTECT files (`y`/`n`, default `n`)
        #[arg(long, value_name = "y|n", default_value = "n")]
        include_config: String,
        /// Include unmodified CONFIG_PROTECT files (`y`/`n`, default `n`)
        #[arg(long, value_name = "y|n", default_value = "n")]
        include_unmodified_config: String,
    },

    #[command(about = "Fetch/mirror distfiles")]
    Mirror {
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },

    #[command(about = "Query package information")]
    Query {
        #[command(subcommand)]
        command: QueryCommand,
    },

    #[command(about = "Clean distfiles and/or binary packages")]
    Clean {
        #[command(subcommand)]
        target: Option<CleanTarget>,
    },

    #[command(about = "Enable/disable/query USE flags in make.conf")]
    Use {
        /// Add (enable) flags
        #[arg(short = 'a', long = "add", value_name = "FLAG")]
        add: Vec<String>,
        /// Remove (disable) flags
        #[arg(short = 'r', long = "remove", value_name = "FLAG")]
        remove: Vec<String>,
        /// Path to make.conf (default: /etc/portage/make.conf)
        #[arg(long = "make-conf", value_name = "PATH")]
        make_conf: Option<camino::Utf8PathBuf>,
    },

    #[command(about = "Edit per-package configuration (package.use, .keywords, .mask, .env)")]
    Pkg {
        #[command(subcommand)]
        command: PkgCommand,
    },

    #[command(about = "Rebuild packages with broken shared library deps")]
    Revdep {
        /// Only consider consumers of libraries whose soname contains NAME.
        #[arg(short = 'L', long, value_name = "NAME")]
        library: Option<String>,
    },

    #[command(about = "Display Portage elog files")]
    Read {
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },

    #[command(about = "Read/manage GLEP 42 news items")]
    News {
        #[command(subcommand)]
        command: Option<NewsCommand>,
    },

    #[command(about = "Check Gentoo Linux Security Advisories")]
    Glsa {
        #[command(subcommand)]
        command: Option<GlsaCommand>,
    },

    #[command(about = "Analyze emerge.log")]
    Log {
        #[command(subcommand)]
        command: Option<LogCommand>,
    },

    #[command(about = "Search inside ebuilds and eclasses")]
    Grep {
        #[arg(required = true)]
        pattern: String,
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
    },

    #[command(about = "Parse/split atom strings")]
    Atom {
        #[arg(required = true)]
        atoms: Vec<String>,
    },

    #[command(about = "Native config selectors (profile, repos) — eselect-like")]
    Select {
        #[command(subcommand)]
        command: SelectCommand,
    },

    /// Register a default `--prefix` / `--local` so bare `em <pkg>` picks it
    /// up (dogfooding). Explicit `--prefix`/`--local`/`--root` still win.
    /// State: `$XDG_STATE_HOME/em/active`. See `em active --help`.
    #[command(about = "Register a default --prefix/--local for bare em invocations")]
    Active {
        #[command(subcommand)]
        command: Option<ActiveCommand>,
    },

    #[command(about = "Bootstrap a prefix layout (use with --local or --prefix)")]
    Setup,

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
    Env,
}

/// `em crossdev` — cross-target setup, mirroring crossdev's option surface (the
/// no-build subset for now; building the toolchain is future work).
#[derive(clap::Args)]
pub struct CrossdevArgs {
    /// Use the LLVM/Clang model (`cross_llvm-*`: host clang cross-targets, no
    /// per-target compiler). Rejects glibc — use musl or a bare-metal target.
    #[arg(short = 'L', long)]
    pub llvm: bool,

    /// Lay down the overlay + sysroot config without building anything.
    #[arg(long)]
    pub init_target: bool,

    /// Bootstrap the cross toolchain into the prefix (`/usr/<tuple>`): the full
    /// intertwined sequence (binutils → headers → gcc-stage1 → libc →
    /// gcc-stage2). Implies `--init-target`.
    #[arg(long)]
    pub setup: bool,

    /// Print the derived target configuration and exit (no writes).
    #[arg(long)]
    pub show_target_cfg: bool,

    /// Build an extra package onto the established cross target (may be
    /// given multiple times). `CATEGORY/PN` — crossdev's own `--ex-pkg`: it
    /// always runs on the host (like `binutils`/`gcc`), not the target
    /// sysroot, matching real crossdev's `set_env` treatment of `--ex-pkg`
    /// extras. Applies to `--init-target`/`--setup` only (a config-time
    /// concern, not a build one); named per invocation, like real crossdev —
    /// not remembered across a later run that omits it.
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

    /// Override the top-level `--privilege` for this crossdev run only (see
    /// [`Cli::effective_privilege`]).
    #[arg(long, value_enum, value_name = "MODE")]
    pub privilege: Option<Privilege>,
}

/// `em toolchain` — bootstrap a self-hosting native toolchain into `--root`.
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
    pub depgraph_flags: DepgraphFlags,

    #[command(flatten)]
    pub merge_flags: MergeFlags,

    #[command(flatten)]
    pub activity: ActivityArgs,

    /// Override the top-level `--privilege` for this toolchain run only (see
    /// [`Cli::effective_privilege`]).
    #[arg(long, value_enum, value_name = "MODE")]
    pub privilege: Option<Privilege>,
}

// `em stages` — assemble stage-build artifacts (stage1/stage3/stage4) *using*
// a toolchain already built by `em toolchain --setup`.
// Stages and binhosts design is documented in todo/em-stages-and-binhosts.md.
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
    pub depgraph_flags: DepgraphFlags,

    #[command(flatten)]
    pub merge_flags: MergeFlags,

    #[command(flatten)]
    pub activity: ActivityArgs,

    /// Override the top-level `--privilege` for this stages run only (see
    /// [`Cli::effective_privilege`]).
    #[arg(long, value_enum, value_name = "MODE")]
    pub privilege: Option<Privilege>,
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
    /// Same as `em sync` — shared implementation.
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

/// `em maint binpkg <action>` — local `PKGDIR` maintenance built on the
/// `Packages` index/reader substrate. No real-portage `emaint` module exists
/// for this (only `emaint binhost`, which just regenerates the index); this
/// is an em-only extension.
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
        /// Report what would be deleted without deleting or reindexing.
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

/// `em select <module>` — native, eselect-like config selectors.
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
}

/// `em select profile <action>`.
#[derive(Subcommand)]
pub enum ProfileAction {
    #[command(about = "List available profiles (marks the current one)")]
    List,
    #[command(about = "Show the current profile")]
    Show,
    #[command(about = "Set the profile by list number or path (cross-aware: no arch check)")]
    Set {
        /// Profile list number (from `list`) or path (e.g. `default/linux/riscv/23.0/rv64/lp64d`).
        target: String,
    },
}

/// `em select repository <action>` — local repos only (remote sync is a TODO).
#[derive(Subcommand)]
pub enum RepositoryAction {
    #[command(about = "List configured repositories")]
    List,
    #[command(about = "Register an existing local repository")]
    Add {
        /// Repository name.
        name: String,
        /// Existing local path to the repository.
        location: String,
    },
    #[command(visible_alias = "rm", about = "Remove a repository's repos.conf entry")]
    Remove {
        /// Repository name.
        name: String,
    },
    #[command(about = "Create a new local overlay (skeleton + repos.conf entry)")]
    Create {
        /// Repository name.
        name: String,
        /// Location (default: `<config-root>/var/db/repos/<name>`).
        location: Option<String>,
    },
}

/// `em select compiler <action>` — gcc-config workalike.
#[derive(Subcommand)]
pub enum CompilerAction {
    #[command(about = "List available compiler profiles")]
    List {
        /// Target tuple (CTARGET) to list profiles for.
        #[arg(short, long)]
        target: Option<String>,
    },
    #[command(about = "Show the current compiler profile")]
    Show {
        /// Target tuple (CTARGET) to show profile for.
        #[arg(short, long)]
        target: Option<String>,
    },
    #[command(about = "Set the active compiler profile")]
    Set {
        /// Compiler profile to activate (e.g., `riscv64-unknown-linux-gnu-16` or `1` for list number).
        profile: String,
        /// Target tuple (CTARGET) for cross-compiler selection.
        #[arg(short, long)]
        target: Option<String>,
    },
}

/// `em select binutils <action>` — binutils-config workalike.
#[derive(Subcommand)]
pub enum BinutilsAction {
    #[command(about = "List available binutils profiles")]
    List {
        /// Target tuple (CTARGET) to list profiles for.
        #[arg(short, long)]
        target: Option<String>,
    },
    #[command(about = "Show the current binutils profile")]
    Show {
        /// Target tuple (CTARGET) to show profile for.
        #[arg(short, long)]
        target: Option<String>,
    },
    #[command(about = "Set the active binutils profile")]
    Set {
        /// Binutils profile to activate (e.g., `riscv64-unknown-linux-gnu-2.46.0` or `1` for list number).
        profile: String,
        /// Target tuple (CTARGET) for cross-binutils selection.
        #[arg(short, long)]
        target: Option<String>,
    },
}

/// `em select linker <action>` — linker profile selection.
#[derive(Subcommand)]
pub enum LinkerAction {
    #[command(about = "List available linker profiles")]
    List {
        /// Target tuple (CTARGET) to list profiles for.
        #[arg(short, long)]
        target: Option<String>,
    },
    #[command(about = "Show the current linker profile")]
    Show {
        /// Target tuple (CTARGET) to show profile for.
        #[arg(short, long)]
        target: Option<String>,
    },
    #[command(about = "Set the active linker profile")]
    Set {
        /// Linker profile to activate (e.g., `riscv64-unknown-linux-gnu-lld-18` or `1` for list number).
        profile: String,
        /// Target tuple (CTARGET) for cross-linker selection.
        #[arg(short, long)]
        target: Option<String>,
    },
}

/// `em select clang <action>` — LLVM/clang slot selection.
#[derive(Subcommand)]
pub enum ClangAction {
    #[command(about = "List available LLVM/clang slots")]
    List,
    #[command(about = "Show the current LLVM/clang slot")]
    Show,
    #[command(about = "Set the active LLVM/clang slot")]
    Set {
        /// LLVM slot to activate (e.g., `22` or `1` for list number).
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
        /// Target tuple (CTARGET) to show the wrapper for.
        #[arg(short, long)]
        target: Option<String>,
    },
    #[command(about = "Show the backend the <target>-pkg-config wrapper currently points at")]
    Show {
        /// Target tuple (CTARGET) to show the wrapper for.
        #[arg(short, long)]
        target: Option<String>,
    },
    #[command(about = "Create/update the <target>-pkg-config wrapper")]
    Set {
        /// Backend to wrap (`pkgconf`, `pkg-config`, or a list number from `list`).
        backend: String,
        /// Target tuple (CTARGET) to create the wrapper for.
        #[arg(short, long)]
        target: Option<String>,
    },
}

/// `em select mirrors <action>` — mirrorselect workalike for `GENTOO_MIRRORS`.
#[derive(Subcommand)]
pub enum MirrorAction {
    /// List available Gentoo distfile mirrors (marks those already selected).
    List {
        /// Keep only mirrors in this ISO country code (e.g. `US`, `DE`).
        #[arg(short, long)]
        country: Option<String>,
        /// Keep only mirrors in this region (e.g. `Europe`, `North America`).
        #[arg(short, long)]
        region: Option<String>,
    },
    /// Show the currently configured `GENTOO_MIRRORS` value.
    Show,
    /// Set `GENTOO_MIRRORS`.
    Set {
        /// Explicit mirror URLs to use. If omitted, mirrors are picked from
        /// `--country`/`--region` instead.
        #[arg(value_name = "URL")]
        urls: Vec<String>,
        /// Use every mirror in this ISO country code.
        #[arg(short, long)]
        country: Option<String>,
        /// Use every mirror in this region.
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
        /// Add flags (written verbatim, e.g. truetype)
        #[arg(short = 'a', long = "add", value_name = "FLAG")]
        add: Vec<String>,
        /// Subtract flags (written with leading '-', e.g. -themes)
        #[arg(short = 's', long = "subtract", value_name = "FLAG")]
        subtract: Vec<String>,
        /// Drop flags entirely (removes both flag and -flag forms)
        #[arg(short = 'd', long = "drop", value_name = "FLAG")]
        drop: Vec<String>,
        /// Target file inside package.use/ (default: `<cat>-<pkg>`)
        #[arg(long, value_name = "FILE")]
        path: Option<camino::Utf8PathBuf>,
    },
    #[command(about = "Edit per-package keywords in package.accept_keywords")]
    Keyword {
        atom: String,
        #[arg(short = 'a', long = "add", value_name = "KW")]
        add: Vec<String>,
        #[arg(short = 's', long = "subtract", value_name = "KW")]
        subtract: Vec<String>,
        #[arg(short = 'd', long = "drop", value_name = "KW")]
        drop: Vec<String>,
        #[arg(long, value_name = "FILE")]
        path: Option<camino::Utf8PathBuf>,
    },
    #[command(about = "Add/remove a package from package.mask")]
    Mask {
        atom: String,
        /// Add the atom to package.mask
        #[arg(short = 'a', long = "add")]
        add: bool,
        /// Remove the atom from package.mask
        #[arg(short = 'd', long = "drop")]
        drop: bool,
        #[arg(long, value_name = "FILE")]
        path: Option<camino::Utf8PathBuf>,
    },
    #[command(about = "Edit per-package env files in package.env")]
    Env {
        atom: String,
        #[arg(short = 'a', long = "add", value_name = "ENVFILE")]
        add: Vec<String>,
        #[arg(short = 'd', long = "drop", value_name = "ENVFILE")]
        drop: Vec<String>,
        #[arg(long, value_name = "FILE")]
        path: Option<camino::Utf8PathBuf>,
    },
}

#[derive(Subcommand)]
pub enum QueryCommand {
    #[command(about = "Find which package owns a file", alias = "b")]
    Belongs {
        #[arg(required = true)]
        file: Vec<String>,
    },
    #[command(about = "Verify checksums of installed package", alias = "k")]
    Check {
        #[arg(required = true)]
        atom: Vec<String>,
    },
    #[command(about = "List packages depending on an atom", alias = "d")]
    Depends {
        #[arg(required = true)]
        atom: Vec<String>,
    },
    #[command(about = "Display full dependency tree", alias = "g")]
    Depgraph {
        #[arg(required = true)]
        atom: Vec<String>,
        /// Output format
        #[arg(long, short, value_enum, default_value = "pretty")]
        format: DepgraphFormat,
        /// Let the solver choose USE flags to satisfy REQUIRED_USE (Level C).
        #[arg(long)]
        autosolve_use: bool,
        #[command(flatten)]
        depgraph_flags: DepgraphFlags,
        /// Treat every atom as not-yet-installed (emerge's `-e`/`--emptytree`).
        #[arg(short = 'e', long)]
        emptytree: bool,
        #[arg(short = 'o', long)]
        onlydeps: bool,
        /// Include build-time dependencies (BDEPEND) in the resolution.
        #[arg(long)]
        with_bdeps: bool,
        /// emerge's `--root-deps[=rdeps]`: only require RDEPEND (not DEPEND)
        /// to be satisfiable in the merge target.
        #[arg(long = "root-deps")]
        root_deps: bool,
    },
    #[command(about = "List files installed by a package", alias = "f")]
    Files {
        #[arg(required = true)]
        atom: Vec<String>,
    },
    #[command(about = "List packages matching env data", alias = "a")]
    Has {
        #[arg(required = true)]
        atom: Vec<String>,
    },
    #[command(about = "List packages with a given USE flag in IUSE", alias = "h")]
    Hasuse {
        #[arg(required = true)]
        flag: Vec<String>,
    },
    #[command(about = "Display keyword status across architectures", alias = "y")]
    Keywords {
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
        #[arg(required = true)]
        atom: Vec<String>,
    },
    #[command(about = "Display total file size of a package", alias = "s")]
    Size {
        #[arg(required = true)]
        atom: Vec<String>,
    },
    #[command(about = "Display USE flags for a package", alias = "u")]
    Uses {
        #[arg(required = true)]
        atom: Vec<String>,
    },
    #[command(about = "Print full path to the ebuild for a package", alias = "w")]
    Which {
        #[arg(required = true)]
        atom: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum CleanTarget {
    #[command(about = "Clean outdated distfiles")]
    Dist,
    #[command(about = "Clean outdated binary packages")]
    Pkg,
}

#[derive(Subcommand)]
pub enum NewsCommand {
    #[command(about = "Count unread news items")]
    Count,
    #[command(about = "List news items")]
    List,
    #[command(about = "Read a news item")]
    Read { id: Option<String> },
    #[command(about = "Purge read news items")]
    Purge,
}

#[derive(Subcommand)]
pub enum GlsaCommand {
    #[command(about = "List all GLSAs")]
    List,
    #[command(about = "Check for affected GLSAs")]
    Check { ids: Vec<String> },
    #[command(about = "Apply a GLSA fix")]
    Fix { ids: Vec<String> },
}

/// `em active <subcommand>` — persistent default `--prefix` / `--local`.
///
/// `set` reads the global `--prefix` / `--local` flags (same shape as
/// `em --prefix DIR setup`), so there is no second set of flag names to
/// collide with the globals.
///
/// Entries can be referenced by name, index (0-based), or exact path.
#[derive(Subcommand)]
pub enum ActiveCommand {
    /// Show the registered active context (default when no subcommand).
    #[command(about = "Show the registered active prefix/local")]
    Show,
    /// Register the invocation's `--prefix` or `--local` as the active context.
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
        /// Reference to an existing entry (name, index, or path) to activate.
        /// If not provided, creates a new entry from --prefix/--local flags.
        #[arg(value_name = "REF")]
        reference: Option<String>,
    },
    /// Clear the registered active context.
    ///
    /// Use `--all` to remove all entries, not just the active pointer.
    #[command(about = "Clear the active context (or all entries with --all)")]
    Clear {
        /// Clear all entries, not just the active pointer.
        #[arg(long)]
        all: bool,
    },
    /// Print shell exports for `eval "$(em active env)"` (PATH + markers).
    #[command(about = "Print shell exports for the active context")]
    Env,
    /// List all registered entries.
    #[command(about = "List all registered prefix/local entries")]
    List,
    /// Add a new entry without activating it.
    ///
    /// Examples:
    ///   `em --prefix /home/me/prefix active add my-prefix`
    ///   `em --local /home/me/.gentoo active add my-gentoo`
    ///   `em --local= active add`  # adds ~/.gentoo with auto-generated name
    #[command(about = "Add a new prefix/local entry")]
    Add {
        /// Optional name for the entry. If not provided, uses path basename.
        #[arg(value_name = "NAME")]
        name: Option<String>,
    },
    /// Remove an entry by name, index, or path.
    ///
    /// Examples:
    ///   `em active remove my-name`
    ///   `em active remove 0`           # by index
    ///   `em active remove /path/to/dir` # by exact path
    #[command(about = "Remove a registered entry")]
    Remove {
        /// Reference to the entry to remove (name, index, or path).
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
        /// Package atom / Cpn / Cpv substring; omit for global median
        atom: Option<String>,
    },
    #[command(about = "ETA for remainder of a live activity session")]
    Predict,
}

/// How an unprivileged build gets root for `chown`/setuid (see `--privilege`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Privilege {
    /// Best compiled-in fake root (pseudoroot, else fakeroost, else none) when
    /// unprivileged, real chowns when already root (default).
    #[default]
    Auto,
    /// Pure-Rust ptrace+seccomp fake root; ownership faked in-session.
    #[cfg(all(feature = "fakeroost", target_os = "linux"))]
    Fakeroost,
    /// LD_PRELOAD fake root (`pseudoroot`); ownership faked in-session, no ptrace tax.
    #[cfg(all(feature = "pseudoroot", any(target_os = "linux", target_os = "macos")))]
    Pseudoroot,
    /// User-namespace sandbox with build-user→0 map; real chowns in-box.
    #[cfg(all(feature = "hakoniwa", target_os = "linux"))]
    Hakoniwa,
    /// Re-exec under `sudo` for real root (root-owned tree, real setuid).
    Sudo,
    /// No wrapping; run unprivileged (chowns best-effort, may not stick).
    None,
}

/// Output format for `em query depgraph`.
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
