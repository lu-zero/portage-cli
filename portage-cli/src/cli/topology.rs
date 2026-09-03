//! Root-topology flags: `--prefix`/`--local`/`--config-root`/`--vdb`/`--target`
//! ([`Topology`]) and `--root` ([`RootArg`], kept separate — see its own doc).
//!
//! [`Topology`] is mounted once on [`crate::cli::Cli`] with inner fields
//! `global`. [`RootArg`] is flattened onto Roots-consuming applets (inner
//! field `global` so `--root` cascades into `em query depgraph --root R`);
//! `Cli` holds a raw non-global `--root` for prefix-position default emerge.
//! Crossdev, Active, and Worker omit `RootArg`.

use camino::Utf8PathBuf;
use portage_resolve::Roots;

/// Topology after resolving CLI flags + optional `em active` registration
///
/// Explicit `--local` / `--prefix` / `--root` always win. When none are set,
/// a previously registered active context (see [`crate::active`]) supplies
/// prefix/local so bare `em <pkg>` dogfooding needs no per-invocation flags.
enum TopologySource {
    Local(Utf8PathBuf),
    Prefix(Utf8PathBuf),
    /// `--root R`
    ///
    /// The path itself is read from `root.root` at the one site that needs
    /// it (`base_roots`), so this variant carries no payload.
    Root,
    Host,
}

/// `s.as_deref()` parsed as a path, or `None`
fn opt_path(s: &Option<String>) -> Option<Utf8PathBuf> {
    s.as_deref().map(Utf8PathBuf::from)
}

/// `--prefix`/`--local`/`--config-root`/`--vdb`/`--target`: which build
/// context an applet resolves against. `--root` (the merge-destination
/// override) is deliberately a separate mixin — see [`RootArg`].
#[derive(usage::Args, Debug, Clone, Default)]
pub struct Topology {
    /// Unprivileged offset: ROOT/VDB/distfiles/build trees under DIR; config
    /// still from the host (use --root for a config offset).
    #[usage(long, global, value_name = "DIR")]
    pub prefix: Option<String>,

    /// Unprivileged, standalone Gentoo-Prefix: own VDB/BROOT/config, not
    /// overlaid on the host (see --prefix for the overlay). Defaults to
    /// ~/.gentoo (EPREFIX=~/.gentoo) when no DIR is given.
    #[usage(long, global, default_missing = "", value_name = "DIR")]
    pub local: Option<String>,

    /// Read config (profile, make.conf) from this root instead of `--root`
    #[usage(long, global, value_name = "PATH")]
    pub config_root: Option<String>,

    /// Override VDB path (default: $ROOT/var/db/pkg)
    #[usage(long, global, value_name = "PATH")]
    pub vdb: Option<String>,

    /// Cross-build/setup for a crossdev target tuple
    ///
    /// The single source for "which tuple" everywhere: `em crossdev --target T --init-target`
    /// sets T up; `em stages --target T --stage1` (or any plain atom build) resolves/installs
    /// into the target sysroot `<EROOT>/usr/<TUPLE>` — sugar for
    /// `--config-root <sysroot> --root <sysroot>`.
    ///
    /// Cross context (CHOST/CBUILD, `--root-deps=rdeps`) is read from the
    /// sysroot make.conf. One flag for both roles — `crossdev` no longer
    /// has its own `-t`/`--target`.
    #[usage(long, short = 'T', global, value_name = "TUPLE")]
    pub target: Option<String>,
}

/// Installation root override — the offset an applet installs into / queries.
///
/// Also settable via `ROOT` in the environment (lowest precedence), applied
/// once by [`resolved_root`]. Inner field is `global` so nested `--root`
/// cascades inside an applet; not mounted on `Cli` (that copy is a raw field).
#[derive(usage::Args, Debug, Clone, Default)]
pub struct RootArg {
    /// Installation root (the offset an applet installs into / queries)
    #[usage(long, global, value_name = "PATH")]
    pub root: Option<String>,
}

/// `root.root`, falling back to the `ROOT` environment variable.
pub fn resolved_root(root: &RootArg) -> Option<String> {
    root.root.clone().or_else(|| std::env::var("ROOT").ok())
}

impl Topology {
    /// Resolve topology from explicit flags, else the `em active` registration
    ///
    /// Precedence: `--local` > `--prefix` > `--root` > active state > bare host.
    /// Active state is only consulted when no root-topology flag is present, so
    /// `em --root R …` never accidentally inherits a registered prefix.
    fn topology_source(&self, root: &RootArg) -> TopologySource {
        if let Some(local) = &self.local {
            let path = if local.is_empty() {
                crate::xdg::home().join(".gentoo")
            } else {
                Utf8PathBuf::from(local)
            };
            return TopologySource::Local(path);
        }
        if let Some(prefix) = opt_path(&self.prefix) {
            return TopologySource::Prefix(prefix);
        }
        if resolved_root(root).is_some() {
            return TopologySource::Root;
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

    /// Resolve the root model (docs/design/root-topology.md) from the topology flags
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
    pub fn roots(&self, root: &RootArg) -> Roots {
        // --target: layer the sysroot on top of the overlay target (the prefix),
        // not base_roots's BROOT (host /). Under --prefix the cross sysroot is
        // <prefix>/usr/<tuple>, and base_roots's merge_root is the host — so
        // derive the sysroot from the overlay's prefix (eprefix) when set.
        let Some(tuple) = self.target.as_deref() else {
            return self.outer_roots(root);
        };
        // The outer EROOT the sysroot sits under: `outer_roots().eprefix()`
        // under `--prefix`/`--local` — the pure prefix identity, unmoved by
        // an explicit `--root` board-root override, same as bare's `/`
        // anchor is unmoved by `--root`. The toolchain always lives at
        // `P/usr/<tuple>` (or bare `/usr/<tuple>`); only the *destination*
        // `stages` installs into moves with `--root`.
        let outer = self.outer_roots(root);
        let has_own_build_context = outer.eprefix().is_some();
        let anchor = if has_own_build_context {
            outer
                .eprefix()
                .expect("has_own_build_context checked eprefix().is_some() above")
                .to_owned()
        } else {
            Utf8PathBuf::from("/")
        };
        let sysroot = anchor.join("usr").join(tuple);
        // An explicit `--root` is always a genuine board-root override,
        // whether or not `--prefix`/`--local` also anchors the toolchain
        // itself; else plain `--target` installs into its own sysroot.
        let merge_target = resolved_root(root)
            .map(Utf8PathBuf::from)
            .unwrap_or_else(|| sysroot.clone());
        // `base` stays the sysroot unconditionally — never `merge_target`.
        // `build_sysroot()` returns `None` when `base == target`, which
        // would drop the toolchain from the compiler's context (confirmed
        // live: `sys-libs/zlib` couldn't find its sysroot's `sys/types.h`).
        // The installed-view fix lives at the call site instead, via
        // `Roots::with_target_only_installed_view()` (`crossdev/mod.rs`).
        Roots::default()
            .with_config(Some(sysroot.clone()))
            .with_base(Some(sysroot))
            .with_target(Some(merge_target))
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
    /// **unconditionally** regardless of `self.target`. This is the "outer
    /// EROOT" every crossdev *setup* action (`crossdev/mod.rs`: `sysroot`,
    /// `setup_root`, `ensure_self_contained_prefix`, `main_repo`) must
    /// anchor to instead of `roots()`.
    ///
    /// Using `roots()` there was a real bug: if `--target T` is also set on
    /// the same invocation as `crossdev --init-target`, `roots()` is
    /// *already* the sysroot, so appending `usr/T` again doubly-nested it
    /// (`<EROOT>/usr/T/usr/T`) — reproduced live.
    ///
    /// `stage1()`/`profile_stack()`/`resolve_gcc_version` deliberately keep
    /// using plain `roots()` — those genuinely want `--target`'s sysroot
    /// substitution (`em stages --target T --stage1` builds *into* the
    /// sysroot, by design).
    pub fn outer_roots(&self, root: &RootArg) -> Roots {
        let base = self.base_roots(root);
        // Under `--target`, an explicit `--root` is `roots()`'s board-root
        // override — where the *stage packages* install — never this
        // function's own "true outer/host location". Suppressed here
        // (rather than left to each branch) so every branch below stays
        // bit-identical to its non-`--target` behavior once `--target`
        // clears it: the toolchain and its host-side `cross-*` packages
        // stay where `--prefix`/`--local` (flag or active registration)
        // put them, or the real host `/` when neither applies.
        let root_redirect = resolved_root(root)
            .map(Utf8PathBuf::from)
            .filter(|_| self.target.is_none());
        if let Some(prefix) = base.eprefix().filter(|_| base.is_overlay()) {
            let prefix = prefix.to_path_buf();
            let anchored = self.overlay_anchor(&base, prefix);
            // An explicit `--root B` alongside `--prefix A` (no `--target`)
            // redirects only the merge *destination* — EPREFIX/BROOT/
            // config-overlay stay anchored to the prefix itself (`--prefix`
            // still supplies the build context: host-shared toolchain,
            // relocatable shebangs, overlay config). Without this, `--root`
            // was silently discarded the instant `--prefix` matched first in
            // `topology_source()`.
            return match root_redirect {
                Some(target) => anchored.with_target(Some(target)),
                None => anchored,
            };
        }
        // Non-overlay: `Local`/`Root`/`Host`. `--local`'s own `base_roots()`
        // arm already bakes an explicit `--root` into its `target` (so a
        // plain `--local L --root B` redirects correctly with no `--target`
        // involved) — undo that here when `--target` is also set, the same
        // way the overlay branch above suppresses its own redirect, so the
        // toolchain stays at `L` regardless of any board-root override.
        // Matched on `topology_source()`, not raw flags, so an `em active`-
        // registered local is honored the same as an explicit `--local`.
        match self.topology_source(root) {
            TopologySource::Local(prefix) => base
                .with_base(Some(prefix.clone()))
                .with_target(Some(root_redirect.unwrap_or(prefix))),
            // `Root`/`Host` already fold `--root` (or its absence) into both
            // `base` and `target` identically inside `base_roots()`, so this
            // just re-asserts the same values — a no-op outside `--target`,
            // and exactly the existing bare-`--target` guard's board-root
            // stripping once `root_redirect` is cleared above.
            _ => match root_redirect {
                Some(target) => base
                    .with_base(Some(target.clone()))
                    .with_target(Some(target)),
                None => base.with_target(None).with_base(None),
            },
        }
    }

    /// The overlay's own anchor, ignoring any `--root` override — always `prefix` itself
    ///
    /// Shared by `outer_roots()` (which then applies an explicit `--root` redirect on top, for
    /// the merge *destination* only) and `host_roots()` (which must NOT apply that redirect: it
    /// answers "where do the overlay's own host-shared build tools live", which never moves
    /// just because `--root` retargets where packages install).
    fn overlay_anchor(&self, base: &Roots, prefix: Utf8PathBuf) -> Roots {
        Roots::default()
            .with_config(base.config().map(|p| p.to_owned()))
            .with_base(None)
            .with_target(Some(prefix.clone()))
            .with_broot(base.broot().map(|p| p.to_owned()))
            .with_cross_arch(base.is_cross_arch())
            .with_eprefix(Some(prefix.clone()))
            .with_config_overlay(Some(prefix.join("etc/portage")))
            .with_relocate(true)
            .with_config_root_explicit(base.config_root_explicit().map(|p| p.to_owned()))
    }

    /// The root model from `--local`/`--prefix`/`--root`/`--config-root`, before
    /// any `--target` sysroot override (see [`roots`](Self::roots)). Public so
    /// the staged-build driver can install `cross-*` toolchain packages (which
    /// always live in the outer EROOT, never the sysroot subdirectory — see
    /// `crossdev/mod.rs`'s module doc) even from a `--target`-active invocation.
    ///
    /// `merge_root()` of the returned `Roots` is **the outer EROOT** (with
    /// `--target`'s sysroot substitution undone) — where `use_outer_eroot`
    /// toolchain-install steps land and where `write_cross_env`/
    /// `write_sysroot_config` (`crossdev/mod.rs`) write config. Under
    /// `--prefix` that's the host `/`; under `--local`/`--root` it's the
    /// offset itself.
    ///
    /// **Not necessarily BROOT** — for plain `--root` the two differ (BROOT
    /// is always the host, see [`host_roots`](Self::host_roots)); they only
    /// coincide for `--prefix`/`--local`, which is why this function used
    /// to be (mis)used for BDEPEND checks too. Use `host_roots` for that.
    pub fn base_roots(&self, root: &RootArg) -> Roots {
        let path = opt_path;
        match self.topology_source(root) {
            // `--local` (or active local): standalone Gentoo-Prefix, own BROOT.
            // Full closure (base == target == the prefix), self-contained VDB.
            // EPREFIX makes installed scripts relocatable (shebangs reference
            // ${EPREFIX}/usr/bin/...). See docs/design/root-topology.md § "Override
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
                // An explicit `--root B` alongside `--local` redirects only the
                // merge *destination* — EPREFIX/BROOT/config-overlay stay
                // anchored to the local prefix itself (own build context is
                // the whole point of `--local`). Without this, `--root` was
                // silently discarded the instant `--local` matched first in
                // `topology_source()`.
                let target = resolved_root(root)
                    .map(Utf8PathBuf::from)
                    .unwrap_or_else(|| prefix.clone());
                Roots::default()
                    .with_config(config)
                    .with_base(Some(prefix.clone()))
                    .with_target(Some(target))
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
                .with_broot(Some(Utf8PathBuf::from("/")))
                .with_cross_arch(false)
                .with_eprefix(Some(prefix.clone()))
                .with_config_overlay(Some(prefix.join("etc/portage")))
                .with_relocate(true)
                .with_config_root_explicit(path(&self.config_root)),
            // Bare host or `--root` offset.
            TopologySource::Root | TopologySource::Host => {
                let root_path = resolved_root(root).map(Utf8PathBuf::from);
                Roots::default()
                    // config: --config-root, else host `/` — true portage `ROOT=`
                    // parity (`PORTAGE_CONFIGROOT` defaults to `/` regardless of
                    // `ROOT`). The 2026-07-09 "own everything" self-contained
                    // default (config following `--root` itself) was reverted
                    // benefit `--root --config-root <same dir>` didn't already
                    // give explicitly, and made a bare `--root DIR` behave unlike
                    // anything a real emerge user would expect.
                    .with_config(path(&self.config_root))
                    // base: --root; host otherwise.
                    .with_base(root_path.clone())
                    // target: --root (install destination). This is "the outer
                    // EROOT" (use_outer_eroot, write_cross_env/
                    // write_sysroot_config in crossdev/mod.rs all rely on this
                    // staying the offset for --root) — a DIFFERENT thing from
                    // BROOT, see satisfaction_root's doc comment.
                    .with_target(root_path)
                    // BROOT is always the real host `/` for `--root`/bare (portage
                    // `ROOT=`/`{target}-emerge` parity) — an offset install borrows
                    // the host's BDEPEND tools, never its own copy.
                    .with_broot(Some(Utf8PathBuf::from("/")))
                    .with_cross_arch(false)
                    .with_eprefix(None)
                    .with_config_overlay(None)
                    .with_relocate(false)
                    .with_config_root_explicit(path(&self.config_root))
            }
        }
    }

    /// The full `Roots` a `MergeRoot::Host`-stamped plan entry actually
    /// merges into (`merge/mod.rs`'s `entry_roots`) — as opposed to
    /// [`satisfaction_root`](Roots::satisfaction_root), which only gives a
    /// bare path for checking whether one is already satisfied.
    ///
    /// Two different answers depending on privilege:
    /// - `--root` (privileged offset, portage `ROOT=` parity): the real host
    ///   `/` — an unsatisfied Host-routed BDEPEND installs there because the
    ///   invocation has root to do so.
    ///
    /// - `--prefix` (unprivileged overlay): the prefix itself — it cannot
    ///   write the real host `/`, so an unsatisfied BDEPEND must land there
    ///   instead. Only the *satisfaction check* stays host-anchored, via
    ///   `satisfaction_root`/`is_overlay`'s VDB-weave callers.
    ///
    /// - `--local`/bare: BROOT already equals the merge root, so the two
    ///   questions coincide.
    pub fn host_roots(&self, root: &RootArg) -> Roots {
        let base = self.base_roots(root);
        if let Some(prefix) = base.eprefix().filter(|_| base.is_overlay()) {
            // Deliberately the un-redirected anchor (`overlay_anchor`), not
            // `outer_roots()` — an explicit `--root` retargets the merge
            // *destination* but never the overlay's own host-shared build
            // context, so this must stay stable even when `--root` is set.
            return self.overlay_anchor(&base, prefix.to_path_buf());
        }
        // BROOT for a non-overlay topology: `--local` owns its own BROOT (the
        // prefix itself, so a finished `--local` tree is self-hosting /
        // relocatable); `--root`/bare borrow the real host `/`.
        let broot = match self.topology_source(root) {
            TopologySource::Local(prefix) => prefix,
            _ => Utf8PathBuf::from("/"),
        };
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

    /// The toolchain sysroot as its own merge destination, for a
    /// `MergeRoot::Base` plan entry (`merge/mod.rs`'s `entry_roots`) —
    /// `roots().base_merge_root()` promoted to a full `Roots` whose own
    /// `merge_root()` is the sysroot itself, not the board root.
    ///
    /// `None` outside the board-root topology (`--target T --root R`, where
    /// `base` and `target` genuinely differ): there is no separate sysroot
    /// merge destination for a `Base` entry to route to there, and
    /// `root_closure::base` never produces one.
    pub fn sysroot_roots(&self, root: &RootArg) -> Option<Roots> {
        let roots = self.roots(root);
        let sysroot = roots.base_merge_root()?.to_owned();
        Some(
            Roots::default()
                .with_config(Some(sysroot.clone()))
                .with_base(Some(sysroot.clone()))
                .with_target(Some(sysroot))
                .with_broot(roots.broot().map(|p| p.to_owned()))
                .with_cross_arch(true)
                .with_eprefix(roots.eprefix().map(|p| p.to_owned()))
                .with_config_overlay(roots.config_overlay().map(|p| p.to_owned()))
                .with_relocate(roots.relocate())
                .with_config_root_explicit(roots.config_root_explicit().map(|p| p.to_owned())),
        )
    }

    /// Reject an action (`toolchain --setup`, `stages --stage1`/`--stage3`)
    /// whose resolved destination equals the host install path
    /// (`host_roots()`) — bare `--local`, bare `--prefix`, bare host, and
    /// `--local --root <the same local path>` all collapse to this.
    ///
    /// `--root DIR` alone, `--prefix P --target T`, and an explicit
    /// `--root B` redirecting away from `--prefix`/`--local`'s own anchor
    /// all genuinely differ from `host_roots()` and pass. Replaces an older,
    /// narrower `merge_root == "/"` check, too narrow to catch a real
    /// `--prefix --target` bug where a package's `.pc` file baked in the
    /// outer prefix's path even though nothing was installed there.
    pub fn require_root_distinct_from_host(
        &self,
        root: &RootArg,
        resolved: &Roots,
        action: &str,
    ) -> anyhow::Result<()> {
        // `--local` is deliberately exempt: it's self-contained by
        // construction (never shared for anything else), so bootstrapping
        // directly into a bare `--local` — no separate `--root` — is the
        // established, working recipe for standing one up from nothing, not
        // a footgun. `--prefix` (the overlay case) IS the footgun this
        // guards: an unredirected `--prefix P` shares its own tree with
        // whatever else that overlay is used for (`is_overlay()`'s own
        // "unsatisfied BDEPEND lands in the prefix" role, `host_roots()`'s
        // doc comment) — building an entire `stages` snapshot straight into
        // it collides with that role.
        //
        // `self.base_roots(root).is_overlay()`, not `resolved.is_overlay()`:
        // under `--target`, `roots()` always sets `base = Some(sysroot)`
        // (never `None`), so `resolved.is_overlay()` is always false there
        // and this guard never fired — `--prefix P --root P --target T
        // stages --stage1` sailed straight past it into the live prefix.
        // `base_roots()` reflects "is this an overlay at all," independent
        // of any later `--target` substitution.
        if self.base_roots(root).is_overlay() && resolved.eprefix() == Some(resolved.merge_root()) {
            anyhow::bail!(
                "{action} needs an explicit --root that doesn't equal the host \
                 install path ({})",
                resolved.merge_root()
            );
        }
        Self::require_destination_not_bare_host(resolved, action)
    }

    /// Narrower guard, also used standalone by `toolchain --setup`: only
    /// rejects the true bare-host case (no `--prefix`/`--local`/`--root`
    /// given at all — bootstrapping a fresh compiler into the real host `/`
    /// is meaningless).
    ///
    /// Unlike [`Self::require_root_distinct_from_host`], a toolchain bootstrap
    /// directly into a bare `--prefix`/`--local` (no separate `--root`) is
    /// the intended, already-verified recipe for giving that overlay/tree
    /// its own compiler. Takes only `resolved` — no topology/root state
    /// feeds this check — so it doesn't need `&self`.
    pub fn require_destination_not_bare_host(resolved: &Roots, action: &str) -> anyhow::Result<()> {
        if resolved.merge_root().as_str() == "/"
            && resolved.base().is_none()
            && resolved.eprefix().is_none()
        {
            anyhow::bail!(
                "{action} needs --prefix/--local/--root: a bootstrap into the bare \
                 host / is meaningless (use the host toolchain directly)"
            );
        }
        Ok(())
    }
}
