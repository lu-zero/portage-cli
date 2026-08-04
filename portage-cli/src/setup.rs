//! `em setup`: bootstrap an unprivileged prefix layout so a subsequent build
//! (or the next `em --local` / `em --prefix DIR` / `em --root DIR` run) has the
//! directories, the overlay search-path `bashrc`, and a `make.conf` placeholder
//! it needs.
//!
//! Three modes (docs/root-topology.md § "Lifecycle"):
//! - `--local` (standalone prefix): EPREFIX set, base == target. Full closure
//!   into `~/.gentoo`; builds its own python via `toolchain --setup`, so no
//!   host-python symlinks. The `BASHRC_LOCAL` recipe (EPREFIX-based) covers the
//!   in-place search-path needs.
//! - `--prefix DIR` (overlay): EPREFIX set, base == host. Borrows host tools,
//!   so symlinks host python into `${EPREFIX}/usr/bin` for the relocatable
//!   shebangs EPREFIX produces. The `BASHRC_PREFIX` recipe (EPREFIX-based)
//!   covers the overlay search-path needs.
//! - `--root DIR` (self-contained offset): no EPREFIX. Own everything; no
//!   CPPFLAGS injection (it actively breaks self-contained roots).
//!
//! Idempotent: directories are created if missing; files are written only when
//! absent, so re-running never clobbers a user's edits.

use anyhow::{Context, Result};
use camino::Utf8Path;

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
	# A genuine cross-*target* package (CTARGET set, no TARGET_ABI — glibc/
	# linux-headers under `crossdev --setup`'s "libc"/"kernel headers" steps;
	# TARGET_ABI is what `multilib::env_block` additionally sets for the
	# host-arch toolchain-*tool* packages, see portage-repo/src/build/
	# shell.rs's matching Rust-side check) must NOT borrow the prefix's own
	# native search paths below — they're a different architecture, and
	# stacking them ahead of the package's own in-tree `-I` flags corrupts
	# its build. Found live 2026-08-04: glibc's own
	# sysdeps/unix/sysv/linux/sysdep.h resolved `<endian.h>` to the prefix's
	# native aarch64 headers instead of its own riscv64 in-tree copy,
	# tripping its "_LIBC must not be defined by applications" guard.
	if [[ -z ${CTARGET} || -n ${TARGET_ABI} ]]; then
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

/// Directories laid out under the prefix's install root (`EROOT`).
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

/// Bootstrap the layout described by `roots`. Needs a target other than the host
/// `/` — i.e. `--local`, `--prefix DIR`, or `--root DIR` (the cross-sysroot
/// confdir case; pair with `em select profile` to set its profile).
pub fn bootstrap(roots: &Roots) -> Result<()> {
    let eroot = roots.merge_root();
    if eroot.as_str() == "/" {
        anyhow::bail!(
            "em setup needs a target: use --local, --prefix DIR, or --root DIR \
             (the host / is never bootstrapped)"
        );
    }
    // Three layout modes (docs/root-topology.md § "Lifecycle"):
    // - standalone prefix (--local): eprefix set, base == target. Full closure
    //   into ~/.gentoo; builds its own python, so NO host-python symlinks.
    // - overlay (--prefix): eprefix set, base != target (host is base). Borrows
    //   host tools, so symlinks host python into ${EPREFIX}/usr/bin to satisfy
    //   the relocatable shebangs EPREFIX produces.
    // - self-contained offset (--root): no eprefix, base == target. Own
    //   everything; no CPPFLAGS injection (actively breaks self-contained roots).
    let has_eprefix = roots.eprefix().is_some();
    let base_eq_target = roots.base() == roots.target();
    let is_standalone_prefix = has_eprefix && base_eq_target; // --local
    let is_overlay = has_eprefix && !base_eq_target; // --prefix
    let self_contained = !has_eprefix && base_eq_target; // --root

    for dir in SKELETON {
        let p = eroot.join(dir);
        std::fs::create_dir_all(p.as_std_path()).with_context(|| format!("creating {p}"))?;
    }
    // The libdir name is host-dependent; create both common ones so installs
    // into either land in an existing tree.
    for libdir in ["usr/lib", "usr/lib64"] {
        let _ = std::fs::create_dir_all(eroot.join(libdir).as_std_path());
    }

    let portage = roots
        .config_overlay()
        .map(Utf8Path::to_path_buf)
        .unwrap_or_else(|| eroot.join("etc/portage"));
    std::fs::create_dir_all(portage.as_std_path())
        .with_context(|| format!("creating {portage}"))?;

    // `BASHRC_PREFIX`'s CPPFLAGS/LDFLAGS injection (`-I<EPREFIX>/usr/include`, an
    // extra high-priority search path) is for a genuine `--prefix DIR`
    // layered *on top of* a shared host base — the host's own real headers
    // are already found by the compiler's normal default search, so the
    // prefix needs an explicit assist to also see its own. A self-contained
    // `--root DIR` (`roots.build_sysroot()` is `None`: base == target, no
    // separate host base to layer over) has no such gap — its SYSROOT/CHOST
    // toolchain wiring already resolves the whole root's own `/usr/include`
    // through the compiler's normal (or cross) search order. Injecting the
    // same CPPFLAGS there doesn't just do nothing: it *actively breaks*
    // builds — it lands ahead of a package's own project-local `-I` flags
    // (e.g. gcc's `libiberty/../include`) and can shadow a version-matched
    // local header with an incompatible one from the ROOT's own libc (found
    // 2026-07-03 doing a from-scratch native+cross toolchain bootstrap: gcc's
    // `libiberty/obstack.c` failed to compile against the ROOT's own,
    // ABI-mismatched `obstack.h`). See [[stage-build-shakeout]].
    if self_contained {
        write_if_absent(&portage.join("bashrc"), "")?;
    } else if is_overlay {
        // --prefix: EPREFIX-based overlay. Host (/) is the build sysroot; the
        // prefix is layered on top.
        write_if_absent(&portage.join("bashrc"), BASHRC_PREFIX)?;
    } else {
        // --local standalone: EPREFIX-based in-place prefix recipe.
        write_if_absent(&portage.join("bashrc"), BASHRC_LOCAL)?;
    }
    write_if_absent(
        &portage.join("make.conf"),
        &make_conf_template(is_standalone_prefix, self_contained, eroot),
    )?;

    // Host-python/host-tool symlinks: overlay only (--prefix). The overlay
    // borrows host tools (base is the host), and EPREFIX makes installed
    // scripts shebang to ${EPREFIX}/usr/bin/pythonX.Y — the symlink satisfies
    // those without building a prefix python. A standalone --local builds its
    // own python via `toolchain --setup`; a symlink there would masquerade as
    // a prefix-owned file and violate the self-contained invariant.
    // (Previously gated on `is_local` — exactly backwards.)
    if is_overlay {
        link_host_pythons(eroot)?;
        link_host_base_tools(eroot)?;
    }

    let mode = if is_standalone_prefix {
        format!("em --local            (standalone Gentoo-Prefix at {eroot})")
    } else if is_overlay {
        format!("em --prefix {eroot}   (ROOT-offset overlay)")
    } else {
        format!("em --root {eroot}     (self-contained offset)")
    };
    println!(">>> Prefix ready at {eroot}");
    println!("    config overlay: {portage}");
    println!("    use it with:    {mode}");
    if is_standalone_prefix {
        println!("    add to PATH:    {eroot}/usr/bin");
    }
    Ok(())
}

/// A commented `make.conf` placeholder documenting how the prefix is used.
///
/// For `--local`/`--prefix`, profile and base make.conf (including `MAKEOPTS`)
/// come from the host, so this file is purely commentary. A self-contained
/// `--root DIR` shares none of that — this is the *only* make.conf ever read
/// — so it needs a real `MAKEOPTS`, not just a placeholder: without one, every
/// build in the root defaults to serial (`-j1`), regardless of how many cores
/// the host has. Found 2026-07-03 doing a from-scratch toolchain bootstrap: a
/// full gcc bootstrap ran over an hour single-threaded on a 128-core box
/// because `MAKEOPTS` was silently unset. See [[stage-build-shakeout]].
fn make_conf_template(is_local: bool, self_contained: bool, eroot: &Utf8Path) -> String {
    let how = if is_local {
        format!(
            "#   em --local <pkg>        # builds in place into {eroot}\n\
             #   (add {eroot}/usr/bin to PATH to run what you install)\n"
        )
    } else {
        format!("#   em --prefix {eroot} <pkg>   # builds a ROOT-offset tree here\n")
    };
    if self_contained {
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

/// The host's own `ACCEPT_KEYWORDS`, when set.
///
/// Without this, the self-contained root's make.conf leaves `ACCEPT_KEYWORDS`
/// unset, which portage treats as stable-only. That silently starves any
/// package whose most recent versions dropped their stable keyword for the
/// host's arch (e.g. a `cross-<CTARGET>/gcc` host-side cross-compiler build
/// stuck on a years-old release because every newer one is `~arch`-only) —
/// found 2026-07-04 chasing a stalled crossdev toolchain bootstrap that
/// silently never saw newer compiler versions. See [[stage-build-shakeout]].
fn host_accept_keywords() -> Option<String> {
    portage_repo::MakeConf::load_default()
        .ok()
        .and_then(|m| m.get("ACCEPT_KEYWORDS").map(str::to_owned))
}

/// Expose the host's Python at the prefix paths the eclasses expect.
///
/// In `--local` mode the host (`/`) is the base system and provides Python, but
/// the python eclasses derive prefix-absolute paths from `EPREFIX`/`ESYSROOT`:
///
/// - `${EPREFIX}/usr/bin/pythonX.Y` is baked into installed scripts' shebangs
///   (e.g. g-ir-scanner). With no interpreter there, every such script dies with
///   `bad interpreter: No such file or directory` — surfacing as meson's opaque
///   "Unhandled python OSError" and breaking the whole gobject-introspection
///   chain (harfbuzz, pango, gdk-pixbuf, gtk+, …).
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

/// Host base-system tools that ebuilds reference by their prefix-absolute path
/// (`${EPREFIX}/usr/bin/<tool>`) rather than via `PATH`. In a real Gentoo Prefix
/// the whole userland lives under `${EPREFIX}`; in `--local` only built packages
/// do, so these must be exposed from the host. Example: the firefox ebuild sets
/// `XARGS=${EPREFIX}/usr/bin/xargs` in its mozconfig, and the build greps trees
/// with `find`. Extend as more such hard-coded references surface.
const HOST_BASE_TOOLS: &[&str] = &["xargs", "find"];

/// Symlink the host base tools in [`HOST_BASE_TOOLS`] into `${EPREFIX}/usr/bin`
/// when they are not already provided by the prefix. Idempotent, best-effort.
fn link_host_base_tools(eroot: &Utf8Path) -> Result<()> {
    let bin = eroot.join("usr/bin");
    std::fs::create_dir_all(bin.as_std_path()).with_context(|| format!("creating {bin}"))?;
    for tool in HOST_BASE_TOOLS {
        let host = format!("/usr/bin/{tool}");
        let link = bin.join(tool);
        if link.as_std_path().symlink_metadata().is_ok() || !Utf8Path::new(&host).exists() {
            continue;
        }
        let _ = std::os::unix::fs::symlink(&host, link.as_std_path());
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
        // own project-local `-I` flags (found 2026-07-03).
        // Stage build shakeout findings are in todo/stage-build-shakeout.md.
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

    /// Regression test for a guard that went silently dead: `--prefix DIR`
    /// always sets `EPREFIX`, and `em` always resolves `ROOT` to `"/"` once
    /// `EPREFIX` is set (`build/shell.rs`'s `root_var`) — a prior ROOT-keyed
    /// version of `BASHRC_PREFIX` never actually ran for any real
    /// `--prefix` build. A plain `body.contains("CPPFLAGS")` check (the test
    /// above) can't catch this: the dead guard's body still contained the
    /// string. This test actually *sources* the recipe with the real
    /// runtime env (`ROOT="/"`, `EPREFIX=<dir>`) and checks what comes out.
    ///
    /// Sources it through `MakeConf::apply_to`'s embedded `brush_core::Shell`
    /// rather than spawning a real `bash` binary: that's the same mechanism
    /// `run_phase` actually uses to source bashrc hooks
    /// (`portage-repo/src/build/shell.rs`'s `bashrc_files` handling) — `em`
    /// never shells out to a subprocess for this, so testing against a
    /// spawned `bash` was exercising a different interpreter than
    /// production. It also depended on a `bash` binary being resolvable on
    /// `PATH`, which raced against other tests that temporarily mutate the
    /// process-wide `PATH` (see `test_support::path_lock`'s doc comment).
    #[tokio::test]
    async fn overlay_bashrc_actually_exports_search_paths_at_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().to_str().unwrap();
        let body = bashrc_body("--prefix", prefix);

        let mut env = std::collections::BTreeMap::new();
        env.insert("ROOT".to_string(), "/".to_string());
        env.insert("EPREFIX".to_string(), prefix.to_string());
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

    /// Regression test for a bug found live 2026-08-04 bootstrapping riscv64
    /// glibc under `--prefix`: a genuine cross-*target* package (`CTARGET`
    /// set, no `TARGET_ABI` — matches `crossdev --setup`'s "libc"/"kernel
    /// headers" steps' own `package.env`) must NOT get the prefix's own
    /// native `CPPFLAGS`/`LDFLAGS`/`PKG_CONFIG_*`/`CMAKE_PREFIX_PATH`
    /// injection — glibc's own `sysdeps/unix/sysv/linux/sysdep.h` resolved
    /// `<endian.h>` to the prefix's native aarch64 headers instead of its
    /// own riscv64 in-tree copy, tripping its "_LIBC must not be defined by
    /// applications" guard. `PATH` must still gain the prefix's `usr/bin`
    /// though — `cross_host_tool_tuple` packages (binutils/gcc, `TARGET_ABI`
    /// also set) still need it to find `${CTARGET}-gcc`.
    #[tokio::test]
    async fn overlay_bashrc_skips_host_paths_for_a_genuine_target_package() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().to_str().unwrap();
        let body = bashrc_body("--prefix", prefix);

        let mut env = std::collections::BTreeMap::new();
        env.insert("ROOT".to_string(), "/".to_string());
        env.insert("EPREFIX".to_string(), prefix.to_string());
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

    /// The host-arch toolchain-*tool* package class (`binutils`/`gcc` —
    /// `TARGET_ABI` also set alongside `CTARGET`) must keep getting the host
    /// path injection, exactly as before this fix.
    #[tokio::test]
    async fn overlay_bashrc_keeps_host_paths_for_a_cross_host_tool_package() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().to_str().unwrap();
        let body = bashrc_body("--prefix", prefix);

        let mut env = std::collections::BTreeMap::new();
        env.insert("ROOT".to_string(), "/".to_string());
        env.insert("EPREFIX".to_string(), prefix.to_string());
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

    #[test]
    fn self_contained_root_gets_real_makeopts() {
        // Without this, every build in a self-contained --root defaults to
        // serial (no host make.conf to inherit MAKEOPTS from) — found
        // 2026-07-03 when a full gcc bootstrap ran single-threaded for over
        // an hour on a 128-core box.
        // Stage build shakeout findings are in todo/stage-build-shakeout.md.
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
        // Stage build shakeout findings are in todo/stage-build-shakeout.md.
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

    /// `--prefix` (overlay) symlinks host base tools into ${EPREFIX}/usr/bin —
    // the relocatable installed tree's shebangs reference ${EPREFIX}/usr/bin/...
    // and the overlay borrows host tools rather than building its own.
    // Previously the symlinks were gated on `--local` (exactly backwards).
    #[test]
    fn overlay_prefix_symlinks_host_base_tools() {
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from(["em", "--prefix", dir.path().to_str().unwrap()]);
        super::bootstrap(&cli.roots()).unwrap();
        let bin = cli.roots().merge_root().join("usr/bin");
        // HOST_BASE_TOOLS = [xargs, find]; the test host should have at least one.
        let has_symlink = ["find", "xargs"]
            .iter()
            .any(|t| bin.join(t).as_std_path().symlink_metadata().is_ok());
        assert!(
            has_symlink,
            "--prefix overlay must symlink host base tools into ${{EPREFIX}}/usr/bin"
        );
    }

    /// `--root` (self-contained) does NOT symlink host tools — it owns everything.
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
