//! `em setup`: bootstrap an unprivileged prefix layout so a subsequent build
//! (or the next `em --local` / `em --prefix DIR` / `em --root DIR` run) has the
//! directories, the overlay search-path `bashrc`, and a `make.conf` placeholder
//! it needs.
//!
//! Three modes (docs/design/root-topology.md § "Lifecycle"):
//! - `--local` (standalone prefix): EPREFIX set, base == target. Full closure
//!   into `~/.gentoo`; builds its own python via `toolchain --setup`, so no
//!   host-python symlinks. The `BASHRC_LOCAL` recipe (EPREFIX-based) covers the
//!   in-place search-path needs.
//! - `--prefix DIR` (overlay): EPREFIX set, base == host. Symlinks host
//!   python/base tools into `${EPREFIX}/usr/bin` for relocatable shebangs,
//!   then oneshot-merges `sys-apps/baselayout` (USE=build) so layout matches
//!   the profile — same as `em -1 baselayout`, not a hand-rolled mkdir tree.
//!   The `BASHRC_PREFIX` recipe covers overlay search-path needs.
//! - `--root DIR` (self-contained offset): no EPREFIX. Own everything; no
//!   CPPFLAGS injection (it actively breaks self-contained roots).
//!
//! Idempotent: directories are created if missing; files are written only when
//! absent, so re-running never clobbers a user's edits.

mod host_tools;
mod local_profile;
mod provided;
mod repo;

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};

use crate::util::write_if_absent;
use portage_resolve::Roots;

/// The `bashrc` recipe for an in-place (`--local`) prefix: paths are already
/// correct in the installed `.pc`, so only the search path is added.
const BASHRC_LOCAL: &str = r#"# Overlay search paths for `em --local` (created by `em setup`).
# EPREFIX makes the installed .pc record correct ${EPREFIX}/usr paths, so the
# build only needs them on the search path — no sysroot/CPPFLAGS rewriting.
if [[ -n ${EPREFIX} ]]; then
	_ov="${EPREFIX%/}"
	_libdir="$(get_libdir 2>/dev/null || echo lib)"
	export PKG_CONFIG_PATH="${_ov}/usr/${_libdir}/pkgconfig:${_ov}/usr/share/pkgconfig${PKG_CONFIG_PATH:+:${PKG_CONFIG_PATH}}"
	# meson.eclass pins PKG_CONFIG_LIBDIR to the prefix, which *replaces*
	# pkg-config's built-in default — so host base packages (zlib, …) become
	# invisible and prefix .pc with `Requires: zlib` fail to resolve. In an
	# in-place prefix the host (/) is the base system, so search the prefix
	# first, then the host. Without this, the meson font/cairo/harfbuzz chain
	# can't find host deps.
	export PKG_CONFIG_LIBDIR="${_ov}/usr/${_libdir}/pkgconfig:${_ov}/usr/share/pkgconfig:/usr/${_libdir}/pkgconfig:/usr/share/pkgconfig${PKG_CONFIG_LIBDIR:+:${PKG_CONFIG_LIBDIR}}"
	# The prefix .pc record correct -L${EPREFIX}/usr/lib for *direct* deps, but
	# the host toolchain's default link search does not include the prefix, so a
	# lib's transitive NEEDED (e.g. libxcb → libXau/libXdmcp) can't be resolved
	# at link time — every meson link probe then fails and configure misdetects
	# functions (cairo's xrender gradient fallback clashes with the new header).
	# -rpath (not just -rpath-link) so in-place prefix binaries also resolve
	# their prefix deps at runtime.
	# Most prefix headers are found via pkg-config -I, but some sources include
	# a prefix-only header transitively without their target declaring the dep
	# (e.g. mesa's gbm-dri backend pulls <xcb/xcb.h>). On the host that header
	# lives in the default search path; in the prefix it does not, so put the
	# prefix include dir on the global search path — the -I counterpart of the
	# LDFLAGS -L below, matching what --prefix mode already does.
	export CPPFLAGS="-I${_ov}/usr/include${CPPFLAGS:+ ${CPPFLAGS}}"
	export LDFLAGS="-L${_ov}/usr/${_libdir} -Wl,-rpath,${_ov}/usr/${_libdir}${LDFLAGS:+ ${LDFLAGS}}"
	# Prefix tools invoked *during* a build (g-ir-compiler, g-ir-scanner, vala,
	# …) are dynamically linked against prefix libs. The -rpath above covers
	# tools built after it landed, but anything installed earlier — and tools
	# whose rpath the host loader still doesn't search — needs the prefix libdir
	# on the runtime search path. This is build-time only (portage bashrc), so it
	# does not leak into the installed packages' runtime.
	export LD_LIBRARY_PATH="${_ov}/usr/${_libdir}${LD_LIBRARY_PATH:+:${LD_LIBRARY_PATH}}"
	export CMAKE_PREFIX_PATH="${_ov}/usr${CMAKE_PREFIX_PATH:+:${CMAKE_PREFIX_PATH}}"
	# Build tools merged into the prefix (vala, cbindgen, …) must be on PATH,
	# and their python modules (xcb-proto's xcbgen, gobject-introspection, …)
	# on PYTHONPATH, so dependent builds find them.
	export PATH="${_ov}/usr/bin${PATH:+:${PATH}}"
	for _pd in "${_ov}"/usr/lib*/python*/site-packages; do
		[[ -d ${_pd} ]] && export PYTHONPATH="${_pd}${PYTHONPATH:+:${PYTHONPATH}}"
	done
	unset _ov _libdir _pd
fi
"#;

/// The `bashrc` recipe for the relocatable overlay (`--prefix DIR`): host (/)
/// is the build sysroot, the prefix is layered on top. Since `--prefix` sets
/// `EPREFIX` (`b3f20c1`), `econf` passes `--prefix=${EPREFIX}/usr`, so a
/// package's own `.pc`/headers/libs installed *into* the prefix already
/// record prefix-relative paths, same as `--local` — this recipe only needs
/// to put them (and the host's own) on the search path, no sysroot rewriting.
const BASHRC_PREFIX: &str = r#"# Overlay search paths for `em --prefix DIR` (created by `em setup`).
# Host (/) is the build sysroot; the prefix is layered on top. Do NOT set
# PKG_CONFIG_SYSROOT_DIR — nothing here needs path rewriting, only search-path
# additions, and rewriting would corrupt the host .pc files' own real paths
# once they're found via PKG_CONFIG_LIBDIR below.
#
# Keyed on EPREFIX, not ROOT: every `--prefix DIR` build sets EPREFIX, and
# `em` always resolves ROOT to "/" once EPREFIX is set (`build/shell.rs`'s
# `root_var`) — a prior ROOT-keyed version of this recipe (written before
# `b3f20c1` flipped that) silently went dead for every `--prefix` build; only
# caught when a meson-based host-arch build under `--prefix` couldn't see a
# host-satisfied BDEPEND (`dev-vcs/git`'s build, missing `libpcre2-8`) —
# it wasn't just PKG_CONFIG_LIBDIR that was missing, the *entire* previous
# block (PKG_CONFIG_PATH, CPPFLAGS, LDFLAGS, CMAKE_PREFIX_PATH) had stopped
# running. This also covers the cross toolchain wrappers case: without
# ${EPREFIX}/usr/bin on PATH, tc-getCC can't find ${CTARGET}-gcc and falls
# back to the host ${CHOST}-gcc, breaking cross glibc/gcc builds with
# target-flag-on-host-gcc errors like "-mabi=lp64d: unrecognized argument".
if [[ -n ${EPREFIX} && ${EPREFIX%/} != "" && ${EPREFIX%/} != "/" ]]; then
	_ov="${EPREFIX%/}"
	_libdir="$(get_libdir 2>/dev/null || echo lib)"
	# PATH stays unconditional: cross_host_tool_tuple packages (binutils/gcc,
	# CTARGET set + TARGET_ABI also set — see the CTARGET check below) still
	# need ${EPREFIX}/usr/bin on PATH to find ${CTARGET}-gcc, or tc-getCC
	# falls back to the host ${CHOST}-gcc (see this recipe's own doc comment).
	export PATH="${_ov}/usr/bin${PATH:+:${PATH}}"
	# Only inject below for a genuinely host-side package. Checked against
	# real crossdev's own two reference tools: /usr/bin/crossdev's
	# `doemerge` (bootstraps binutils/gcc/glibc/linux-headers via plain
	# `emerge`, host CHOST throughout for every one of them) and
	# /usr/bin/cross-emerge (installs an ordinary package for the target,
	# CHOST=<target>). cross-emerge never touches plain CPPFLAGS/CFLAGS/
	# LDFLAGS at all — PORTAGE_CONFIGROOT pointing at the sysroot already
	# gets them right from the sysroot's own make.conf — it only pulls the
	# host's own values into the *scoped* BUILD_CFLAGS/BUILD_CPPFLAGS/
	# BUILD_LDFLAGS, exactly what toolchain-funcs.eclass/meson.eclass/
	# cargo.eclass expect. But cross-emerge only ever handles the
	# target-package case; `em`'s single `--prefix` merge mixes host-side
	# and target-side packages in the same run, so unlike cross-emerge we
	# still need this injection for two host-side cases: a plain native
	# package (CBUILD == CHOST, no CTARGET at all), and `crossdev --setup`'s
	# own host-arch toolchain-*tool* packages (binutils/gcc — CBUILD ==
	# CHOST too, since `use_outer_eroot` routes them through the outer/
	# host config same as a native package, but package.env additionally
	# marks them with TARGET_ABI, set by `multilib::env_block` — see
	# portage-repo/src/build/shell.rs's matching Rust-side check).
	#
	# Everything else must NOT borrow the prefix's own native search
	# paths: a genuine `--target` package (CBUILD != CHOST, since its
	# CHOST correctly comes from the sysroot's own make.conf) or
	# `crossdev --setup`'s own glibc/linux-headers steps (CBUILD == CHOST
	# via use_outer_eroot's routing, but CTARGET set with no TARGET_ABI
	# marks them target-class regardless). Stacking the prefix's paths
	# ahead of either's own in-tree `-I` flags corrupts the build — found
	# live 2026-08-04, two ways: glibc's own
	# sysdeps/unix/sysv/linux/sysdep.h resolved `<endian.h>` to the
	# prefix's native aarch64 headers instead of its own riscv64 in-tree
	# copy (tripping its "_LIBC must not be defined by applications"
	# guard), and plain sys-libs/zlib / sys-apps/install-xattr (ordinary
	# dependencies of a `-T riscv64-unknown-linux-gnu -b llvm-core/clang`
	# build, not under `crossdev --setup` at all) failed to link
	# ("cannot find Scrt1.o") for the identical reason.
	#
	# Host-class inject via package.env marker (bash-crossdev set_env):
	# CBUILD==CHOST && (CTARGET empty || TARGET_ABI set). No EM_BUILD_CLASS
	# (todo/drop-buildclass.md).
	if [[ ${CBUILD:-${CHOST}} == ${CHOST} && ( -z ${CTARGET} || -n ${TARGET_ABI} ) ]]; then
		export PKG_CONFIG_PATH="${_ov}/usr/${_libdir}/pkgconfig:${_ov}/usr/share/pkgconfig${PKG_CONFIG_PATH:+:${PKG_CONFIG_PATH}}"
		# meson.eclass pins PKG_CONFIG_LIBDIR to the prefix alone when the env var
		# isn't already set (it *replaces* pkg-config's built-in default search,
		# unlike PKG_CONFIG_PATH, which is additive) — so a host-satisfied BDEPEND
		# (e.g. dev-libs/libpcre2 for dev-vcs/git's meson build) becomes invisible
		# to a meson-based build even though PKG_CONFIG_PATH alone would have
		# found it. Search the prefix first, then the host, matching BASHRC_LOCAL.
		export PKG_CONFIG_LIBDIR="${_ov}/usr/${_libdir}/pkgconfig:${_ov}/usr/share/pkgconfig:/usr/${_libdir}/pkgconfig:/usr/share/pkgconfig${PKG_CONFIG_LIBDIR:+:${PKG_CONFIG_LIBDIR}}"
		export CPPFLAGS="-I${_ov}/usr/include${CPPFLAGS:+ ${CPPFLAGS}}"
		export LDFLAGS="-L${_ov}/usr/${_libdir} -Wl,-rpath-link,${_ov}/usr/${_libdir}${LDFLAGS:+ ${LDFLAGS}}"
		export CMAKE_PREFIX_PATH="${_ov}/usr${CMAKE_PREFIX_PATH:+:${CMAKE_PREFIX_PATH}}"
	fi
	unset _ov _libdir
fi
"#;

/// Directories laid out under the prefix's install root (`EROOT`)
const SKELETON: &[&str] = &[
    "etc/portage",
    "var/db/pkg",
    "var/cache/distfiles",
    "var/tmp/portage",
    "var/lib",
    "usr/bin",
    "usr/include",
    "usr/share",
];

/// Which of the three layouts (see this module's doc comment) `setup` is
/// building, derived once from the resolved roots.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// `--local`: EPREFIX set, base == target
    Local,
    /// `--prefix DIR`: EPREFIX set, host is the base
    Overlay,
    /// `--root DIR`: no EPREFIX, base == target
    SelfContained,
}

impl Mode {
    fn resolve(roots: &Roots) -> Result<Self> {
        if roots.merge_root().as_str() == "/" {
            anyhow::bail!(
                "em setup needs a target: use --local, --prefix DIR, or --root DIR \
                 (the host / is never bootstrapped)"
            );
        }
        Ok(
            match (roots.eprefix().is_some(), roots.base() == roots.target()) {
                (true, true) => Self::Local,
                (true, false) => Self::Overlay,
                (false, _) => Self::SelfContained,
            },
        )
    }

    fn label(self) -> &'static str {
        match self {
            Self::Local => "--local (standalone prefix)",
            Self::Overlay => "--prefix (overlay)",
            Self::SelfContained => "--root (self-contained offset)",
        }
    }

    fn usage(self, eroot: &Utf8Path) -> String {
        match self {
            Self::Local => format!("em --local            (standalone Gentoo-Prefix at {eroot})"),
            Self::Overlay => format!("em --prefix {eroot}   (ROOT-offset overlay)"),
            Self::SelfContained => format!("em --root {eroot}     (self-contained offset)"),
        }
    }

    /// A self-contained `--root` is not a topology `em active` can select: it
    /// offsets config too, so it is driven by `--root`, never by a registered
    /// default.
    fn registrable(self) -> Option<crate::active::ActiveKind> {
        match self {
            Self::Local => Some(crate::active::ActiveKind::Local),
            Self::Overlay => Some(crate::active::ActiveKind::Prefix),
            Self::SelfContained => None,
        }
    }
}

/// `em setup` — bootstrap a layout and register it as available
///
/// Registration lives here rather than in [`bootstrap`] because it is a
/// property of *the command*, not of building a layout: `crossdev` bootstraps
/// prefixes as an internal step, and those are not topologies a user chooses
/// between. Keeping [`bootstrap`] free of it also keeps it free of side effects
/// outside the tree it is given — its callers include tests.
///
/// After the skeleton + config, runs **`sys-apps/baselayout` oneshot** with
/// `USE=build` (same idea as `em -1 baselayout`) so merged-usr / split-usr
/// matches the active profile — do not reimplement baselayout in mkdir form.
///
/// When `pretend` is true (global `-p`/`--pretend`), print what would be
/// created and write nothing — same contract as crossdev's config plan under `-p`.
pub async fn run(cli: &crate::cli::Cli, args: &crate::cli::SetupArgs) -> Result<()> {
    let roots = cli.roots();
    let mode = Mode::resolve(&roots)?;
    if mode != Mode::Local && !args.extra_path.is_empty() {
        anyhow::bail!(
            "--extra-path applies to `em setup --local` only: {} borrows the host's tools \
             through its own layout, not through the build PATH",
            mode.label()
        );
    }
    if cli.pretend {
        return preview(&roots, mode);
    }
    // The host has to supply the tools a still-empty prefix builds with, so
    // this runs before anything is written: a host that cannot bootstrap one
    // should not be left holding a half-built prefix.
    let extra_path = match mode {
        Mode::Local => host_tools::check(&args.extra_path)?,
        _ => Vec::new(),
    };
    bootstrap_mode(&roots, mode)?;
    // `--local` needs its own resolvable repo + profile + bootstrap
    // `package.provided` *before* the baselayout merge below — without a
    // repo, that merge can't resolve `sys-apps/baselayout` at all on a host
    // with no Gentoo tree of its own (see todo/local-bootstrap-provided.md).
    if mode == Mode::Local {
        ensure_config_root(cli, &roots, &extra_path).await?;
    }
    merge_baselayout(cli, &extra_path).await?;
    // Available only — the active pointer is the user's to move, with
    // `em active set`.
    if let Some(kind) = mode.registrable()
        && let Some(name) = crate::active::register_available(kind, roots.merge_root())
    {
        println!("    registered as:  {name}   (em active list / em active set {name})");
    }
    Ok(())
}

/// The `--local` config-root ladder: repo, then profile, then bootstrap
/// `package.provided` — must land in that order, since profile resolution
/// needs the repo's `profiles/` and `package.provided` needs both the
/// profile directory and the synced tree's available versions.
async fn ensure_config_root(
    cli: &crate::cli::Cli,
    roots: &Roots,
    extra_path: &[Utf8PathBuf],
) -> Result<()> {
    let eroot = roots.merge_root();
    let repo_path = repo::ensure_repo(cli, eroot).await?;
    let repo = crate::repo_open::open(repo_path.as_std_path())
        .context("opening the prefix's ::gentoo repo")?;
    local_profile::ensure_profile(eroot, &repo)?;
    provided::ensure_provided(eroot, &repo, extra_path)?;
    Ok(())
}

/// Oneshot-merge `sys-apps/baselayout` with `USE=build` into the outer EROOT
/// (`use_outer_eroot`), not the world file. Public so `crossdev --setup` can
/// seed the same layout under `--prefix` before toolchain steps.
pub async fn merge_baselayout(cli: &crate::cli::Cli, extra_path: &[Utf8PathBuf]) -> Result<()> {
    let outer = cli.outer_roots();
    let eroot = outer.merge_root();
    if eroot.as_str() == "/" {
        return Ok(());
    }
    println!(">>> merging sys-apps/baselayout (USE=build, oneshot) into {eroot}");
    let use_override = ["build".to_string()];
    let mut merge_flags = cli.merge_flags.clone();
    merge_flags.oneshot = true;
    crate::emerge_atoms(
        cli,
        &["sys-apps/baselayout".to_string()],
        crate::EmergeOpts {
            use_override: &use_override,
            // Same as stage1 baselayout: install the skeleton without pulling
            // the world; baselayout's own deps are minimal under USE=build.
            nodeps: true,
            depgraph_flags: None,
            merge_flags: Some(merge_flags),
            use_outer_eroot: true,
            target_only_installed_view: false,
            update_world: false,
            is_resume: false,
            activity: None,
            activity_session: Default::default(),
            extra_aliases: &[],
            extra_path,
            autounmask_widen: false,
        },
    )
    .await
}

/// `-p` / `--pretend` path for [`run`]: describe the layout without writing
fn preview(roots: &Roots, mode: Mode) -> Result<()> {
    let eroot = roots.merge_root();
    println!(">>> would bootstrap layout at {eroot} ({})", mode.label());
    println!(">>> would create skeleton dirs under {eroot} (etc/portage, var/db/pkg, …)");
    let portage = roots
        .config_overlay()
        .map(Utf8Path::to_path_buf)
        .unwrap_or_else(|| eroot.join("etc/portage"));
    println!(">>> would ensure config files under {portage} (bashrc, make.conf placeholders)");
    if mode == Mode::Local {
        println!(">>> would check the host tools this prefix borrows until it builds its own");
        tracing::info!(
            "would resolve a ::gentoo repo (piggy-back the host's, else write an own-tree \
             entry and sync it), link make.profile, and write the Tier-1 bootstrap \
             package.provided block"
        );
    }
    println!(">>> would oneshot-merge sys-apps/baselayout (USE=build) into {eroot}");
    println!(">>> would register available entry for em active (not the active pointer)");
    println!(">>> (pretend — no files written)");
    Ok(())
}

/// Bootstrap the layout described by `roots`
///
/// Needs a target other than the host `/` — i.e. `--local`, `--prefix DIR`, or `--root DIR`
/// (the cross-sysroot confdir case; pair with `em select profile` to set its profile).
pub fn bootstrap(roots: &Roots) -> Result<()> {
    bootstrap_mode(roots, Mode::resolve(roots)?)
}

fn bootstrap_mode(roots: &Roots, mode: Mode) -> Result<()> {
    let eroot = roots.merge_root();
    for dir in SKELETON {
        let p = eroot.join(dir);
        std::fs::create_dir_all(p.as_std_path()).with_context(|| format!("creating {p}"))?;
    }
    // Libdir name is host-dependent; both so early installs have a landing
    // place before baselayout (or packages) own the real layout.
    for libdir in ["usr/lib", "usr/lib64"] {
        let _ = std::fs::create_dir_all(eroot.join(libdir).as_std_path());
    }

    let portage = roots
        .config_overlay()
        .map(Utf8Path::to_path_buf)
        .unwrap_or_else(|| eroot.join("etc/portage"));
    std::fs::create_dir_all(portage.as_std_path())
        .with_context(|| format!("creating {portage}"))?;

    // `BASHRC_PREFIX` injects `-I<EPREFIX>/usr/include` for a host-layered
    // `--prefix`. Self-contained `--root` has no host layer — the same
    // injection shadows package-local `-I` (e.g. gcc libiberty) with ROOT
    // headers. Standalone `--local` uses `BASHRC_LOCAL`.
    let bashrc = match mode {
        Mode::Local => BASHRC_LOCAL,
        Mode::Overlay => BASHRC_PREFIX,
        Mode::SelfContained => "",
    };
    write_if_absent(&portage.join("bashrc"), bashrc)?;
    write_if_absent(&portage.join("make.conf"), &make_conf_template(mode, eroot))?;

    // Host-python/host-tool symlinks: overlay only (--prefix). The overlay
    // borrows host tools (base is the host), and EPREFIX makes installed
    // scripts shebang to ${EPREFIX}/usr/bin/pythonX.Y — the symlink satisfies
    // those without building a prefix python. A standalone --local builds its
    // own python via `toolchain --setup`; a symlink there would masquerade as
    // a prefix-owned file and violate the self-contained invariant.
    if mode == Mode::Overlay {
        link_host_pythons(eroot)?;
        link_host_base_tools(eroot)?;
    }

    println!(">>> Prefix ready at {eroot}");
    println!("    config overlay: {portage}");
    println!("    use it with:    {}", mode.usage(eroot));
    if mode == Mode::Local {
        println!("    add to PATH:    {eroot}/usr/bin");
    }
    Ok(())
}

/// `make.conf` for a new prefix/root
///
/// Overlay/local: commentary only (host supplies profile + MAKEOPTS). Self-contained
/// `--root`: the only make.conf read — seed real `MAKEOPTS` / `ACCEPT_KEYWORDS` from the
/// host so builds are not serial stable-only by default.
fn make_conf_template(mode: Mode, eroot: &Utf8Path) -> String {
    let how = if mode == Mode::Local {
        format!(
            "#   em --local <pkg>        # builds in place into {eroot}\n\
             #   (add {eroot}/usr/bin to PATH to run what you install)\n"
        )
    } else {
        format!("#   em --prefix {eroot} <pkg>   # builds a ROOT-offset tree here\n")
    };
    if mode == Mode::SelfContained {
        let accept_keywords = match host_accept_keywords() {
            Some(k) => format!("ACCEPT_KEYWORDS=\"{k}\"\n"),
            None => String::new(),
        };
        return format!(
            "# Portage config for this self-contained em --root (created by `em setup`).\n\
             #\n\
             # Use this root with:\n\
             #   em --root {eroot} <pkg>\n\
             #\n\
             # Unlike --local/--prefix, this root shares NO config with the host — this\n\
             # is the only make.conf it ever reads. MAKEOPTS mirrors the host's build\n\
             # parallelism (or falls back to nproc) since nothing else would set it.\n\
             # ACCEPT_KEYWORDS mirrors the host's too — without it, portage defaults to\n\
             # stable-only, silently starving any package whose newest versions dropped\n\
             # their stable keyword for this arch.\n\
             MAKEOPTS=\"{}\"\n\
             {accept_keywords}",
            host_makeopts()
        );
    }
    format!(
        "# Portage config overlay for this em prefix (created by `em setup`).\n\
         #\n\
         # Use this prefix with:\n\
         {how}\
         #\n\
         # Profile and base make.conf come from the host (/etc/portage). The\n\
         # `package.use` and `bashrc` files in this directory overlay the host\n\
         # config so you can tune the prefix without root. Put per-package USE\n\
         # in `package.use`, e.g.:\n\
         #   media-libs/freetype harfbuzz\n"
    )
}

/// The host's own `MAKEOPTS` (real build parallelism the user already tuned),
/// falling back to `-j<nproc>` when the host has none set or is unreadable.
/// `pub(crate)`: also used by `crossdev::make_conf_body` for the cross
/// sysroot's make.conf, which needs the exact same default (see its call site
/// for why the sysroot's own make.conf needs this at all).
pub(crate) fn host_makeopts() -> String {
    portage_repo::MakeConf::load_default()
        .ok()
        .and_then(|m| m.get("MAKEOPTS").map(str::to_owned))
        .unwrap_or_else(|| {
            let n = std::thread::available_parallelism().map_or(1, |n| n.get());
            format!("-j{n}")
        })
}

/// Host `ACCEPT_KEYWORDS`, when set
///
/// Self-contained roots mirror it so packages are not stuck on stable-only (newer toolchain
/// versions are often `~arch` only).
fn host_accept_keywords() -> Option<String> {
    portage_repo::MakeConf::load_default()
        .ok()
        .and_then(|m| m.get("ACCEPT_KEYWORDS").map(str::to_owned))
}

/// Expose the host's Python at the prefix paths the eclasses expect
///
/// In `--local` mode the host (`/`) is the base system and provides Python, but
/// the python eclasses derive prefix-absolute paths from `EPREFIX`/`ESYSROOT`:
///
/// - `${EPREFIX}/usr/bin/pythonX.Y` is baked into installed scripts'
///   shebangs. With no interpreter there, every such script dies with `bad
///   interpreter: No such file or directory`, breaking the whole
///   gobject-introspection chain (harfbuzz, pango, gdk-pixbuf, gtk+, …).
///
/// - `PYTHON_INCLUDEDIR=${ESYSROOT}/usr/include/pythonX.Y` is checked for
///   existence by python-utils-r1 (`does not install any header files!`),
///   breaking C-extension packages like dev-python/pillow.
///
/// Symlink the host `/usr/bin/python*` entries and `/usr/include/python*` dirs
/// into the prefix so those paths resolve. Idempotent and best-effort.
fn link_host_pythons(eroot: &Utf8Path) -> Result<()> {
    link_host_entries(&eroot.join("usr/bin"), "/usr/bin", "python")?;
    link_host_entries(&eroot.join("usr/include"), "/usr/include", "python")?;
    Ok(())
}

/// Host tools linked into `${EPREFIX}/usr/bin` for **overlay** (`--prefix`) only
///
/// Ebuilds often hardcode prefix-absolute paths; without a full Prefix userland those must
/// resolve somehow. Layout (`bin`→`usr/bin`) comes from merging baselayout — this list is
/// host binary content only. See `docs/design/em-prefix-experiment.md`.
const HOST_BASE_TOOLS: &[&str] = &[
    "bash", "sh", "xargs", "find", "perl", "install", "true", "grep", "env", "ed",
];

/// Symlink the host base tools in [`HOST_BASE_TOOLS`] into `${EPREFIX}/usr/bin`
/// when they are not already provided by the prefix. Tries `/usr/bin` then
/// `/bin`. Idempotent, best-effort.
fn link_host_base_tools(eroot: &Utf8Path) -> Result<()> {
    let bin = eroot.join("usr/bin");
    std::fs::create_dir_all(bin.as_std_path()).with_context(|| format!("creating {bin}"))?;
    for tool in HOST_BASE_TOOLS {
        let link = bin.join(tool);
        if link.as_std_path().symlink_metadata().is_ok() {
            continue;
        }
        for host in [format!("/usr/bin/{tool}"), format!("/bin/{tool}")] {
            if !Utf8Path::new(&host).exists() {
                continue;
            }
            let _ = std::os::unix::fs::symlink(&host, link.as_std_path());
            break;
        }
    }
    Ok(())
}

/// Symlink every entry of `host_dir` whose name starts with `prefix` into
/// `dst_dir`, pointing back at the host path. Skips entries already present.
fn link_host_entries(dst_dir: &Utf8Path, host_dir: &str, prefix: &str) -> Result<()> {
    std::fs::create_dir_all(dst_dir.as_std_path())
        .with_context(|| format!("creating {dst_dir}"))?;
    let Ok(entries) = std::fs::read_dir(host_dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(prefix) {
            continue;
        }
        let link = dst_dir.join(name);
        // Skip if anything is already there (including a broken symlink).
        if link.as_std_path().symlink_metadata().is_ok() {
            continue;
        }
        let target = format!("{host_dir}/{name}");
        let _ = std::os::unix::fs::symlink(&target, link.as_std_path());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::Cli;

    #[tokio::test]
    async fn pretend_run_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().to_str().unwrap();
        let cli =
            crate::cli::Cli::try_parse_from(["em", "-p", "--prefix", prefix, "setup"]).unwrap();
        super::run(&cli, &crate::cli::SetupArgs::default())
            .await
            .unwrap();
        assert!(
            !std::path::Path::new(prefix).join("etc/portage").exists(),
            "pretend must not create etc/portage"
        );
        assert!(
            !std::path::Path::new(prefix).join("var/db/pkg").exists(),
            "pretend must not create var/db/pkg"
        );
    }

    // Relaxing the build `PATH` is a `--local` bootstrap concern only: an
    // overlay layers on a host that already supplies the tools, and reaches
    // them through its own layout rather than the phase `PATH`.
    #[tokio::test]
    async fn extra_path_is_refused_outside_local() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().to_str().unwrap();
        let cli =
            Cli::try_parse_from(["em", "--prefix", prefix, "setup", "--extra-path", "/opt/b"])
                .unwrap();
        let Some(crate::cli::Applet::Setup(args)) = cli.applet.as_ref() else {
            unreachable!("parsed as `setup`")
        };
        let err = super::run(&cli, args).await.unwrap_err();
        assert!(err.to_string().contains("--extra-path"), "{err}");
    }

    // Each flag maps to the topology `em setup` builds, and only two of the
    // three are ones `em active` can select — a self-contained `--root` is
    // driven by the flag, never by a registered default. Registration itself
    // deliberately lives in `run`, not in `bootstrap`: `crossdev` bootstraps
    // prefixes internally and tests bootstrap into tempdirs, and neither
    // should touch the user's state.
    #[test]
    fn mode_maps_flags_to_selectable_topologies() {
        use super::Mode;
        use crate::active::ActiveKind;

        let mode = |args: &[&str]| {
            let cli = Cli::parse_from([&["em"], args].concat());
            Mode::resolve(&cli.roots()).unwrap()
        };

        assert_eq!(
            mode(&["--prefix", "/pfx"]).registrable(),
            Some(ActiveKind::Prefix)
        );
        assert_eq!(
            mode(&["--local", "/loc"]).registrable(),
            Some(ActiveKind::Local)
        );
        assert_eq!(mode(&["--root", "/root"]).registrable(), None);
        assert!(
            Mode::resolve(&Cli::parse_from(["em", "setup"]).roots()).is_err(),
            "the host / is never bootstrapped"
        );
    }

    fn bashrc_body(flag: &str, dir: &str) -> String {
        let cli = Cli::parse_from(["em", flag, dir]);
        super::bootstrap(&cli.roots()).unwrap();
        std::fs::read_to_string(cli.roots().merge_root().join("etc/portage/bashrc")).unwrap()
    }

    #[test]
    fn self_contained_root_gets_no_cppflags_injection() {
        // A genuinely self-contained `--root DIR` (base == target, no host
        // base to layer over) must NOT get BASHRC_PREFIX's CPPFLAGS/LDFLAGS
        // injection — it actively breaks builds by out-ranking a package's
        // own project-local `-I` flags.
        let dir = tempfile::tempdir().unwrap();
        let body = bashrc_body("--root", dir.path().to_str().unwrap());
        assert_eq!(body, "", "self-contained --root must get an empty bashrc");
    }

    #[test]
    fn layered_prefix_keeps_cppflags_injection() {
        // A `--prefix DIR` layered on the shared host base still needs it —
        // unaffected by the self-contained fix above.
        let dir = tempfile::tempdir().unwrap();
        let body = bashrc_body("--prefix", dir.path().to_str().unwrap());
        assert!(body.contains("CPPFLAGS"));
    }

    // Regression test for a guard that went silently dead: `--prefix DIR`
    // always sets `EPREFIX`, and `em` always resolves `ROOT` to `"/"` once
    // `EPREFIX` is set (`build/shell.rs`'s `root_var`) — a prior ROOT-keyed
    // version of `BASHRC_PREFIX` never actually ran for any real
    // `--prefix` build. A plain `body.contains("CPPFLAGS")` check (the test
    // above) can't catch this: the dead guard's body still contained the
    // string. This test actually *sources* the recipe with the real
    // runtime env (`ROOT="/"`, `EPREFIX=<dir>`) and checks what comes out.
    //
    // Sources it through `MakeConf::apply_to`'s embedded `brush_core::Shell`
    // rather than spawning a real `bash` binary: that's the same mechanism
    // `run_phase` actually uses to source bashrc hooks
    // (`portage-repo/src/build/shell.rs`'s `bashrc_files` handling) — `em`
    // never shells out to a subprocess for this, so testing against a
    // spawned `bash` was exercising a different interpreter than
    // production. It also depended on a `bash` binary being resolvable on
    // `PATH`, which raced against other tests that temporarily mutate the
    // process-wide `PATH` (see `test_support::path_lock`'s doc comment).
    #[tokio::test]
    async fn overlay_bashrc_actually_exports_search_paths_at_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().to_str().unwrap();
        let body = bashrc_body("--prefix", prefix);

        let mut env = std::collections::BTreeMap::new();
        env.insert("ROOT".to_string(), "/".to_string());
        env.insert("EPREFIX".to_string(), prefix.to_string());
        // Plain native --prefix package: no CTARGET, CBUILD==CHOST default.
        env.insert("CHOST".to_string(), "aarch64-pc-linux-gnu".to_string());
        env.insert("CBUILD".to_string(), "aarch64-pc-linux-gnu".to_string());
        portage_repo::MakeConf::parse(body)
            .unwrap()
            .apply_to(&mut env)
            .await
            .unwrap();

        let get = |name: &str| env.get(name).cloned().unwrap_or_default();
        let path = get("PATH");
        let pkg_config_path = get("PKG_CONFIG_PATH");
        let pkg_config_libdir = get("PKG_CONFIG_LIBDIR");
        let cppflags = get("CPPFLAGS");
        let ldflags = get("LDFLAGS");
        let cmake_prefix_path = get("CMAKE_PREFIX_PATH");

        assert!(path.contains(&format!("{prefix}/usr/bin")), "PATH: {path}");
        assert!(
            pkg_config_path.contains(&format!("{prefix}/usr/lib/pkgconfig")),
            "PKG_CONFIG_PATH: {pkg_config_path}"
        );
        // The host-visibility fix: PKG_CONFIG_LIBDIR must list the prefix
        // *and* the host's own pkgconfig dirs, or a meson-based build can't
        // see a host-satisfied BDEPEND at all (meson.eclass pins
        // PKG_CONFIG_LIBDIR to the prefix alone whenever the env var isn't
        // already set, replacing pkg-config's own built-in default).
        assert!(
            pkg_config_libdir.contains(&format!("{prefix}/usr/lib/pkgconfig")),
            "PKG_CONFIG_LIBDIR missing prefix: {pkg_config_libdir}"
        );
        assert!(
            pkg_config_libdir.contains("/usr/lib/pkgconfig")
                && pkg_config_libdir.matches("/usr/lib/pkgconfig").count() >= 2,
            "PKG_CONFIG_LIBDIR missing host dir: {pkg_config_libdir}"
        );
        assert!(
            cppflags.contains(&format!("{prefix}/usr/include")),
            "CPPFLAGS: {cppflags}"
        );
        assert!(
            ldflags.contains(&format!("{prefix}/usr/lib")),
            "LDFLAGS: {ldflags}"
        );
        assert!(
            cmake_prefix_path.contains(&format!("{prefix}/usr")),
            "CMAKE_PREFIX_PATH: {cmake_prefix_path}"
        );
    }

    // Target packages (CTARGET set, no TARGET_ABI) must not get overlay
    // host-path injection (CPPFLAGS/LDFLAGS/…); PATH still gains prefix
    // usr/bin for cross tools.
    #[tokio::test]
    async fn overlay_bashrc_skips_host_paths_for_a_genuine_target_package() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().to_str().unwrap();
        let body = bashrc_body("--prefix", prefix);

        let mut env = std::collections::BTreeMap::new();
        env.insert("ROOT".to_string(), "/".to_string());
        env.insert("EPREFIX".to_string(), prefix.to_string());
        // K|L target package: CTARGET set, no TARGET_ABI.
        env.insert("CHOST".to_string(), "aarch64-pc-linux-gnu".to_string());
        env.insert("CBUILD".to_string(), "aarch64-pc-linux-gnu".to_string());
        env.insert(
            "CTARGET".to_string(),
            "riscv64-unknown-linux-gnu".to_string(),
        );
        portage_repo::MakeConf::parse(body)
            .unwrap()
            .apply_to(&mut env)
            .await
            .unwrap();

        let get = |name: &str| env.get(name).cloned().unwrap_or_default();
        assert!(
            get("PATH").contains(&format!("{prefix}/usr/bin")),
            "PATH: {}",
            get("PATH")
        );
        assert!(get("CPPFLAGS").is_empty(), "CPPFLAGS: {}", get("CPPFLAGS"));
        assert!(get("LDFLAGS").is_empty(), "LDFLAGS: {}", get("LDFLAGS"));
        assert!(
            get("PKG_CONFIG_PATH").is_empty(),
            "PKG_CONFIG_PATH: {}",
            get("PKG_CONFIG_PATH")
        );
        assert!(
            get("PKG_CONFIG_LIBDIR").is_empty(),
            "PKG_CONFIG_LIBDIR: {}",
            get("PKG_CONFIG_LIBDIR")
        );
        assert!(
            get("CMAKE_PREFIX_PATH").is_empty(),
            "CMAKE_PREFIX_PATH: {}",
            get("CMAKE_PREFIX_PATH")
        );
    }

    // The host-arch toolchain-*tool* package class (`binutils`/`gcc` —
    // `TARGET_ABI` also set alongside `CTARGET`, `CBUILD == CHOST` via
    // `use_outer_eroot`'s routing) must keep getting the host path
    // injection, exactly as before this fix.
    #[tokio::test]
    async fn overlay_bashrc_keeps_host_paths_for_a_cross_host_tool_package() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().to_str().unwrap();
        let body = bashrc_body("--prefix", prefix);

        let mut env = std::collections::BTreeMap::new();
        env.insert("ROOT".to_string(), "/".to_string());
        env.insert("EPREFIX".to_string(), prefix.to_string());
        // Host tool package.env: TARGET_ABI set alongside CTARGET.
        env.insert("CHOST".to_string(), "aarch64-pc-linux-gnu".to_string());
        env.insert("CBUILD".to_string(), "aarch64-pc-linux-gnu".to_string());
        env.insert(
            "CTARGET".to_string(),
            "riscv64-unknown-linux-gnu".to_string(),
        );
        env.insert("TARGET_ABI".to_string(), "lp64d".to_string());
        portage_repo::MakeConf::parse(body)
            .unwrap()
            .apply_to(&mut env)
            .await
            .unwrap();

        let get = |name: &str| env.get(name).cloned().unwrap_or_default();
        assert!(
            get("CPPFLAGS").contains(&format!("{prefix}/usr/include")),
            "CPPFLAGS: {}",
            get("CPPFLAGS")
        );
    }

    // package.env CBUILD/CTARGET/TARGET_ABI sniff is the only host-class
    // gate (no EM_BUILD_CLASS).
    #[tokio::test]
    async fn overlay_bashrc_uses_package_env_sniff_for_host_class() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().to_str().unwrap();
        let body = bashrc_body("--prefix", prefix);

        let sniff = |chost: &str, cbuild: &str, ctarget: &str| {
            let (body, prefix) = (body.to_string(), prefix.to_string());
            let (chost, cbuild, ctarget) =
                (chost.to_string(), cbuild.to_string(), ctarget.to_string());
            async move {
                let mut env = std::collections::BTreeMap::new();
                env.insert("ROOT".to_string(), "/".to_string());
                env.insert("EPREFIX".to_string(), prefix);
                env.insert("CHOST".to_string(), chost);
                env.insert("CBUILD".to_string(), cbuild);
                env.insert("CTARGET".to_string(), ctarget);
                portage_repo::MakeConf::parse(body)
                    .unwrap()
                    .apply_to(&mut env)
                    .await
                    .unwrap();
                env.get("CPPFLAGS").cloned().unwrap_or_default()
            }
        };

        let inc = format!("{prefix}/usr/include");
        // Plain native package: host-class, injected.
        assert!(
            sniff("aarch64-pc-linux-gnu", "aarch64-pc-linux-gnu", "")
                .await
                .contains(&inc)
        );
        // Genuine foreign-arch target package (CBUILD != CHOST): skipped.
        assert!(
            !sniff("riscv64-unknown-linux-gnu", "aarch64-pc-linux-gnu", "")
                .await
                .contains(&inc)
        );
        // Target-class cross step (CTARGET set, no TARGET_ABI): skipped.
        assert!(
            !sniff(
                "aarch64-pc-linux-gnu",
                "aarch64-pc-linux-gnu",
                "riscv64-unknown-linux-gnu"
            )
            .await
            .contains(&inc)
        );
    }

    // Regression: an ordinary
    // `--target riscv64-unknown-linux-gnu` package (`sys-libs/zlib`,
    // `sys-apps/install-xattr`, ...) has `CBUILD != CHOST` (its `CHOST`
    // correctly resolves from the sysroot's own `make.conf`) and no
    // `CTARGET` set at all — the pre-existing `CTARGET`-without-`TARGET_ABI`
    // check alone didn't catch this case, only the newly-added
    // `CBUILD == CHOST` guard does. Confirmed against real crossdev's own
    // `/usr/bin/cross-emerge`, which never injects host paths into plain
    // `CPPFLAGS`/`LDFLAGS` for a target-package build (see this recipe's own
    // doc comment).
    #[tokio::test]
    async fn overlay_bashrc_skips_host_paths_for_an_ordinary_target_package() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().to_str().unwrap();
        let body = bashrc_body("--prefix", prefix);

        let mut env = std::collections::BTreeMap::new();
        env.insert("ROOT".to_string(), "/".to_string());
        env.insert("EPREFIX".to_string(), prefix.to_string());
        // Ordinary --target package: CBUILD != CHOST.
        env.insert("CHOST".to_string(), "riscv64-unknown-linux-gnu".to_string());
        env.insert("CBUILD".to_string(), "aarch64-pc-linux-gnu".to_string());
        portage_repo::MakeConf::parse(body)
            .unwrap()
            .apply_to(&mut env)
            .await
            .unwrap();

        let get = |name: &str| env.get(name).cloned().unwrap_or_default();
        assert!(
            get("PATH").contains(&format!("{prefix}/usr/bin")),
            "PATH: {}",
            get("PATH")
        );
        assert!(get("CPPFLAGS").is_empty(), "CPPFLAGS: {}", get("CPPFLAGS"));
        assert!(get("LDFLAGS").is_empty(), "LDFLAGS: {}", get("LDFLAGS"));
        assert!(
            get("PKG_CONFIG_PATH").is_empty(),
            "PKG_CONFIG_PATH: {}",
            get("PKG_CONFIG_PATH")
        );
        assert!(
            get("PKG_CONFIG_LIBDIR").is_empty(),
            "PKG_CONFIG_LIBDIR: {}",
            get("PKG_CONFIG_LIBDIR")
        );
        assert!(
            get("CMAKE_PREFIX_PATH").is_empty(),
            "CMAKE_PREFIX_PATH: {}",
            get("CMAKE_PREFIX_PATH")
        );
    }

    #[test]
    fn self_contained_root_gets_real_makeopts() {
        // Without this, every build in a self-contained --root defaults to
        // serial (no host make.conf to inherit MAKEOPTS from) — found
        // an hour on a 128-core box.
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from(["em", "--root", dir.path().to_str().unwrap()]);
        super::bootstrap(&cli.roots()).unwrap();
        let make_conf =
            std::fs::read_to_string(cli.roots().merge_root().join("etc/portage/make.conf"))
                .unwrap();
        assert!(make_conf.contains("MAKEOPTS="));
        assert!(!make_conf.contains("MAKEOPTS=\"\""), "must be non-empty");
    }

    #[test]
    fn self_contained_root_gets_host_accept_keywords() {
        // Without this, ACCEPT_KEYWORDS is unset in the self-contained root's
        // make.conf, which portage treats as stable-only — silently starving
        // any package whose newest versions dropped their stable keyword for
        // the host arch (e.g. a cross-toolchain build stuck on a years-old
        // compiler release).
        let Some(host_kw) = super::host_accept_keywords() else {
            return; // nothing to assert if the test host itself has none set
        };
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from(["em", "--root", dir.path().to_str().unwrap()]);
        super::bootstrap(&cli.roots()).unwrap();
        let make_conf =
            std::fs::read_to_string(cli.roots().merge_root().join("etc/portage/make.conf"))
                .unwrap();
        assert!(make_conf.contains(&format!("ACCEPT_KEYWORDS=\"{host_kw}\"")));
    }

    #[test]
    fn layered_prefix_make_conf_has_no_makeopts() {
        // Unaffected by the self-contained fix — --prefix already inherits
        // the host's real MAKEOPTS via config sharing.
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from(["em", "--prefix", dir.path().to_str().unwrap()]);
        super::bootstrap(&cli.roots()).unwrap();
        let make_conf =
            std::fs::read_to_string(cli.roots().merge_root().join("etc/portage/make.conf"))
                .unwrap();
        assert!(!make_conf.contains("MAKEOPTS="));
    }

    // `--prefix` (overlay) symlinks host base tools into ${EPREFIX}/usr/bin —
    // the relocatable installed tree's shebangs reference ${EPREFIX}/usr/bin/...
    // and the overlay borrows host tools rather than building its own.
    // Previously the symlinks were gated on `--local` (exactly backwards).
    #[test]
    fn overlay_prefix_symlinks_host_base_tools() {
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from(["em", "--prefix", dir.path().to_str().unwrap()]);
        super::bootstrap(&cli.roots()).unwrap();
        let bin = cli.roots().merge_root().join("usr/bin");
        // HOST_BASE_TOOLS includes find/xargs; the test host should have at least one.
        let has_symlink = ["find", "xargs"]
            .iter()
            .any(|t| bin.join(t).as_std_path().symlink_metadata().is_ok());
        assert!(
            has_symlink,
            "--prefix overlay must symlink host base tools into ${{EPREFIX}}/usr/bin"
        );
    }

    // `--root` (self-contained) does NOT symlink host tools — it owns everything
    // (Layout comes from oneshot baselayout in [`super::run`], not bootstrap.)
    #[test]
    fn self_contained_root_does_not_symlink_host_tools() {
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from(["em", "--root", dir.path().to_str().unwrap()]);
        super::bootstrap(&cli.roots()).unwrap();
        let bin = cli.roots().merge_root().join("usr/bin");
        let has_symlink = ["find", "xargs"]
            .iter()
            .any(|t| bin.join(t).as_std_path().symlink_metadata().is_ok());
        assert!(
            !has_symlink,
            "--root self-contained must NOT symlink host tools"
        );
    }
}
