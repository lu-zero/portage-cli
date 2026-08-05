//! The resolved root model (`ROOT`/`EROOT`/`BROOT`/`ESYSROOT` topology) that
//! every resolution/policy decision in this crate is parameterized over.

use camino::{Utf8Path, Utf8PathBuf};
use portage_atom_pubgrub::DepClass;

/// The resolved set of roots for a command (see `docs/root-topology.md`):
/// config source, the planner's installed base, and the install target.
/// Built once from `em`'s global flags (`portage-cli`'s `Cli::roots`/
/// `base_roots`/`outer_roots`/`broot`, via the `with_*` builder methods
/// below — the fields are private, so construction always goes through
/// them) and passed around as a unit.
#[derive(Debug, Clone, Default)]
pub struct Roots {
    config: Option<Utf8PathBuf>,
    base: Option<Utf8PathBuf>,
    target: Option<Utf8PathBuf>,
    /// Where `BDEPEND`/`IDEPEND` (cross) resolve — always the true build
    /// host, independent of any `--target` sysroot substitution. `None`
    /// only where it trivially equals `merge_root()` (bare, `--local`).
    /// See [`satisfaction_root`](Self::satisfaction_root).
    broot: Option<Utf8PathBuf>,
    /// `CHOST != CBUILD` for the currently active topology — the one cell
    /// `satisfaction_root` needs it for (`IDEPEND`).
    is_cross_arch: bool,
    /// `EPREFIX`: when set (`--local`), packages are configured for and
    /// installed in place at this offset (`target == eprefix`, so `EROOT ==
    /// target` and `ROOT == /`). `None` for ROOT-offset / host builds.
    eprefix: Option<Utf8PathBuf>,
    /// A user-writable config dir overlaid on the host config for
    /// `package.use`/`bashrc` (the `~/.gentoo/etc/portage` of `--local`),
    /// so an unprivileged user can override without touching `/etc/portage`.
    config_overlay: Option<Utf8PathBuf>,
    relocate: bool,
    /// The literal `--config-root` value, if the user gave one — unlike
    /// [`config`](Self::config), never derived from `--root`. See
    /// [`config_root_explicit`](Self::config_root_explicit).
    config_root_explicit: Option<Utf8PathBuf>,
    /// See [`with_target_only_installed_view`](Self::with_target_only_installed_view).
    installed_view_target_only: bool,
}

impl Roots {
    /// `PORTAGE_CONFIGROOT`: where profile and make.conf are read.
    pub fn config(&self) -> Option<&Utf8Path> {
        self.config.as_deref()
    }

    /// The literal `--config-root` value, if given — unlike
    /// [`config`](Self::config), never derived from `--root`. `em select`
    /// uses this instead of `config()`, matching real eselect's own
    /// behavior (its `profile.eselect` module only ever honours an explicit
    /// `PORTAGE_CONFIGROOT`/`EROOT`, never derives a config root from `ROOT`
    /// alone) — so a bare `em --root R select ...` operates on the host's
    /// config unless `--config-root R` is also given, instead of silently
    /// picking up whatever `--root`'s self-contained-bootstrap default
    /// resolved `config()` to.
    pub fn config_root_explicit(&self) -> Option<&Utf8Path> {
        self.config_root_explicit.as_deref()
    }

    /// The base root whose VDB seeds the planner's "installed" view.
    pub fn base(&self) -> Option<&Utf8Path> {
        self.base.as_deref()
    }

    /// The install target: where new packages land and the delta VDB lives.
    pub fn target(&self) -> Option<&Utf8Path> {
        self.target.as_deref()
    }

    /// The install/merge root (`EROOT`), defaulting to `/`. With `--local`
    /// this is the prefix (`target == eprefix`); files and the VDB land here.
    pub fn merge_root(&self) -> &Utf8Path {
        self.target.as_deref().unwrap_or(Utf8Path::new("/"))
    }

    /// Where `BDEPEND`/`IDEPEND` (cross) resolve — the true build host,
    /// independent of any `--target` sysroot substitution.
    pub fn broot(&self) -> Option<&Utf8Path> {
        self.broot.as_deref()
    }

    /// Whether `CHOST != CBUILD` for the currently active topology.
    pub fn is_cross_arch(&self) -> bool {
        self.is_cross_arch
    }

    /// `EPREFIX` for an in-place prefix build (`--local`), else `None`.
    pub fn eprefix(&self) -> Option<&Utf8Path> {
        self.eprefix.as_deref()
    }

    /// Whether this is an overlay view (EPREFIX set, base is the host): the
    /// `--prefix` case where `base_roots()`'s merge_root is the host but the
    /// actual install target is the prefix. `roots()` uses this to reconstruct
    /// the prefix-target view on top of `base_roots()`.
    pub fn is_overlay(&self) -> bool {
        self.eprefix.is_some() && self.base.is_none()
    }

    /// Whether this is a self-contained `--root DIR` topology (own config,
    /// own everything — `setup.rs`'s "self-contained offset" mode): no
    /// EPREFIX, base == target, and not the bare host. Topology-only — a
    /// robust replacement for the old `config().is_some()` proxy
    /// (`config()` incidentally happens to be `Some` for exactly this
    /// topology too, but that's no longer the *reason* to detect it — see
    /// `config_root_explicit`). Used by `crossdev/mod.rs`'s
    /// `ensure_self_contained_prefix`/`ensure_prefix_profile`.
    pub fn is_self_contained_root(&self) -> bool {
        self.eprefix.is_none() && self.base == self.target && self.merge_root().as_str() != "/"
    }

    /// For internal orchestration only (`crossdev::activate_toolchain`):
    /// a self-contained `--root` build's own `gcc-config`/`binutils-config`
    /// slot files must live under *its own* `etc/env.d`, not the host's —
    /// unlike `em select`'s user-facing config-root resolution
    /// (`config_root_explicit`), which deliberately does NOT infer this from
    /// `--root` alone (see that method's doc comment). The internal
    /// orchestrator already knows it just bootstrapped this exact offset, so
    /// it forces its own config root explicitly rather than requiring the
    /// user to also type `--config-root` on every crossdev invocation.
    pub fn with_own_config_root_if_self_contained(mut self) -> Self {
        if self.is_self_contained_root() {
            self.config_root_explicit = Some(self.merge_root().to_owned());
        }
        self
    }

    /// Force the planner's installed view to `VDB(target)` alone, dropping
    /// the general `VDB(base) ∪ VDB(target)` overlay-sharing model
    /// (`docs/root-model.md`) for this one operation.
    ///
    /// The native toolchain bootstrap (`em toolchain --setup`,
    /// `stages::toolchain_plan(Native, ..)`) is unconditionally
    /// self-contained regardless of `--root`/`--prefix` topology — it must
    /// place a working compiler and libc *inside* the target, not treat
    /// whatever the host already has installed as satisfying it. Under
    /// `--root` this was accidental (`base == target` there already, so
    /// there was nothing to share), but under `--prefix` `base` is `None`
    /// (bare host `/`), so `virtual/os-headers`/`sys-kernel/linux-headers`
    /// resolved as already-satisfied from the *host's* real VDB and never
    /// got merged into the prefix — glibc's own `--with-headers` then
    /// pointed at an empty directory and failed to configure. Found live
    /// 2026-07-16 running the toolchain bootstrap into a fresh `--prefix`
    /// for the first time (previously only tested via `-p`/individual
    /// package builds, never a real end-to-end run).
    ///
    /// Deliberately does *not* touch `base` itself: `base` also drives
    /// [`build_sysroot`](Self::build_sysroot) (SYSROOT/ESYSROOT — what the
    /// build compiles *against*) and [`satisfaction_root`](Self::satisfaction_root)
    /// (`DepClass::Depend`'s resolution root). Overwriting `base` to force a
    /// target-only installed view once broke both of those for the exact
    /// same native-toolchain-bootstrap steps this method targets: with
    /// `base == target`, `build_sysroot()` returned `None` (SYSROOT defaults
    /// to ROOT, i.e. the prefix itself, same as EPREFIX) and gcc's own
    /// `ESYSROOT = SYSROOT + EPREFIX` formula doubled the prefix path
    /// (`--with-build-sysroot=<prefix>/<prefix>/`), which broke gcc's
    /// self-build (wrong `-I` search order picked up glibc's own bundled
    /// `obstack.h` over gcc's newer bundled copy). Found live 2026-07-16,
    /// same session, testing this exact fix end to end.
    pub fn with_target_only_installed_view(mut self) -> Self {
        self.installed_view_target_only = true;
        self
    }

    /// See [`with_target_only_installed_view`](Self::with_target_only_installed_view).
    pub fn installed_view_target_only(&self) -> bool {
        self.installed_view_target_only
    }

    /// User config overlay dir (`package.use`/`bashrc` layered on host config).
    pub fn config_overlay(&self) -> Option<&Utf8Path> {
        self.config_overlay.as_deref()
    }

    /// The build-against sysroot (`SYSROOT`/`ESYSROOT`) to hand the shell:
    /// `None` means "same as the install target" (full offset / host), so the
    /// shell defaults `SYSROOT = ROOT`. `Some` only for an overlay where the
    /// base differs from the target (`--prefix`), where the base is the system
    /// to build against and the target is layered on top.
    pub fn build_sysroot(&self) -> Option<&Utf8Path> {
        if self.base.as_deref() != self.target.as_deref() {
            Some(self.base.as_deref().unwrap_or(Utf8Path::new("/")))
        } else {
            None
        }
    }

    /// Whether `--prefix` relocates distfiles and the build trees under the
    /// target (a self-contained tree).
    pub fn relocate(&self) -> bool {
        self.relocate
    }

    /// Directory under which relocated distfiles / work trees live when
    /// [`relocate`](Self::relocate) is true.
    ///
    /// Prefer [`eprefix`](Self::eprefix) when set so that under
    /// `--prefix P --target T` (or `--local` + `--target`) trees stay under
    /// the outer prefix `P`, not under the cross sysroot `P/usr/T`. Falls
    /// back to [`merge_root`](Self::merge_root) for self-contained offsets
    /// where eprefix is unset. `None` when relocate is off.
    pub fn relocate_root(&self) -> Option<&Utf8Path> {
        if !self.relocate {
            return None;
        }
        Some(self.eprefix().unwrap_or_else(|| self.merge_root()))
    }

    /// Where an unsatisfied dependency of `class` resolves and is checked
    /// against (docs/root-topology.md's satisfaction-root table, PMS table
    /// 8.2): `BDEPEND` always resolves on `broot` (the true build host,
    /// independent of any `--target` sysroot substitution); `IDEPEND` is
    /// `broot` for a cross build, else the same as `RDEPEND`/`PDEPEND`;
    /// `DEPEND` resolves against `base` when it genuinely differs from the
    /// target (an overlay, e.g. `--prefix`); otherwise, for a native
    /// (same-arch) build, `broot` — there's no separate build sysroot
    /// distinct from the host when `CBUILD==CHOST`, confirmed empirically
    /// against real portage (`ROOT=X emerge sys-devel/gcc` against an empty
    /// `X` doesn't need `os-headers`/`perl`/etc. built fresh into `X`; glibc
    /// and gcc's own DEPEND is satisfied by the host, 2026-07-11) — only a
    /// genuine cross build
    /// (`--target`, foreign-arch) keeps DEPEND pinned to the target sysroot,
    /// since the host's own-arch VDB can't satisfy a foreign-arch DEPEND at
    /// all; `RDEPEND`/`PDEPEND` always resolve against the target
    /// (`merge_root()`).
    ///
    /// This replaces threading a second `host_roots: &Roots` alongside
    /// `roots` everywhere just to answer the `BDEPEND` question — `broot`
    /// is carried on the same `Roots` value now, so one value answers both.
    pub fn satisfaction_root(&self, class: DepClass) -> &Utf8Path {
        match class {
            DepClass::Bdepend => self.broot.as_deref().unwrap_or_else(|| self.merge_root()),
            DepClass::Idepend if self.is_cross_arch => self.satisfaction_root(DepClass::Bdepend),
            DepClass::Idepend | DepClass::Rdepend | DepClass::Pdepend => self.merge_root(),
            DepClass::Depend => {
                if let Some(base) = self.base.as_deref()
                    && base != self.merge_root()
                {
                    base
                } else if self.is_cross_arch {
                    self.merge_root()
                } else {
                    self.satisfaction_root(DepClass::Bdepend)
                }
            }
        }
    }

    /// `ESYSROOT` / cross sysroot: `PORTAGE_CONFIGROOT` when set, else base.
    pub fn sysroot(&self) -> Option<&Utf8Path> {
        self.config.as_deref().or(self.base.as_deref())
    }

    /// Load `repos.conf` portage-style for this invocation: global defaults +
    /// confdir under the config root, plus the `--local`/`--prefix` overlay
    /// confdir. The single source of truth for repo discovery.
    pub fn repos_conf(&self) -> portage_repo::Result<portage_repo::ReposConf> {
        let cfg = self.config().unwrap_or_else(|| Utf8Path::new("/"));
        let extra: Vec<&Utf8Path> = self.config_overlay().into_iter().collect();
        portage_repo::ReposConf::load_rooted(cfg, &extra)
    }

    // -----------------------------------------------------------------
    // Builder methods — the fields above are private, so this is the only
    // way to construct a non-default `Roots`. Each takes the field's exact
    // storage type and consumes/returns `self`, matching the
    // `with_own_config_root_if_self_contained` shape already established.
    // -----------------------------------------------------------------

    /// Set `config` (`PORTAGE_CONFIGROOT`).
    pub fn with_config(mut self, config: Option<Utf8PathBuf>) -> Self {
        self.config = config;
        self
    }

    /// Set `base` (the planner's installed-view root).
    pub fn with_base(mut self, base: Option<Utf8PathBuf>) -> Self {
        self.base = base;
        self
    }

    /// Set `target` (the install/merge root).
    pub fn with_target(mut self, target: Option<Utf8PathBuf>) -> Self {
        self.target = target;
        self
    }

    /// Set `broot` (where BDEPEND/IDEPEND resolve).
    pub fn with_broot(mut self, broot: Option<Utf8PathBuf>) -> Self {
        self.broot = broot;
        self
    }

    /// Set `is_cross_arch` (`CHOST != CBUILD`).
    pub fn with_cross_arch(mut self, is_cross_arch: bool) -> Self {
        self.is_cross_arch = is_cross_arch;
        self
    }

    /// Set `eprefix` (`EPREFIX` for an in-place prefix build).
    pub fn with_eprefix(mut self, eprefix: Option<Utf8PathBuf>) -> Self {
        self.eprefix = eprefix;
        self
    }

    /// Set `config_overlay` (the user config-overlay dir).
    pub fn with_config_overlay(mut self, config_overlay: Option<Utf8PathBuf>) -> Self {
        self.config_overlay = config_overlay;
        self
    }

    /// Set `relocate` (whether distfiles/build trees relocate under target).
    pub fn with_relocate(mut self, relocate: bool) -> Self {
        self.relocate = relocate;
        self
    }

    /// Set `config_root_explicit` (the literal `--config-root` value).
    pub fn with_config_root_explicit(mut self, config_root_explicit: Option<Utf8PathBuf>) -> Self {
        self.config_root_explicit = config_root_explicit;
        self
    }

    // -----------------------------------------------------------------
    // Test-only constructors. `#[cfg(test)]` doesn't survive across a crate
    // boundary (it's only `true` while *this* crate is under test, not for
    // portage-cli's own tests that call these) — `#[doc(hidden)] pub`
    // instead, always compiled, just hidden from the public docs since
    // they're not meant for real callers.
    // -----------------------------------------------------------------

    /// Test-only: a `Roots` with `base`, `target`, and `broot` all set to
    /// the same path (matching a plain `--root DIR` invocation, BROOT
    /// included, so BDEPEND-satisfaction tests see the same root without a
    /// separate `host_roots` value), for exercising root-selection logic
    /// without a full CLI parse and without any VDB lookup silently falling
    /// through to the real bare host's.
    #[doc(hidden)]
    pub fn for_test(target: &str) -> Self {
        let path = Utf8PathBuf::from(target);
        Roots {
            base: Some(path.clone()),
            target: Some(path.clone()),
            broot: Some(path),
            ..Default::default()
        }
    }

    /// Test-only: a bare `--root DIR` shaped `Roots` with BROOT a genuinely
    /// separate directory from the offset (`base`/`target`) — matching real
    /// `Dual { broot: host, target: offset }`, `eprefix: None`,
    /// `is_cross_arch: false`. `for_test` collapses all three roles to one
    /// path, which can't exercise `initial_depend`'s host-vs-target weave
    /// (they're the same directory there); this can.
    #[doc(hidden)]
    pub fn for_test_root_with_broot(target: &str, broot: &str) -> Self {
        let path = Utf8PathBuf::from(target);
        Roots {
            base: Some(path.clone()),
            target: Some(path),
            broot: Some(Utf8PathBuf::from(broot)),
            ..Default::default()
        }
    }

    /// Test-only: a `Roots` shaped like `--prefix`'s overlay — `base: None`,
    /// `target`/`eprefix` the prefix, `broot` a separate host path — so
    /// `is_overlay()`/BDEPEND-weave tests can use two independent fake VDB
    /// dirs instead of the real host `/`.
    #[doc(hidden)]
    pub fn for_test_overlay(host: &str, prefix: &str) -> Self {
        let prefix = Utf8PathBuf::from(prefix);
        Roots {
            base: None,
            target: Some(prefix.clone()),
            broot: Some(Utf8PathBuf::from(host)),
            eprefix: Some(prefix.clone()),
            config_overlay: Some(prefix.join("etc/portage")),
            relocate: true,
            ..Default::default()
        }
    }
}
