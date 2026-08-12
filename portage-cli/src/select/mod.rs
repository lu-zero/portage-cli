//! `em select` — native workalikes of `eselect` modules.
//!
//! Currently implemented:
//! - [`profile`] — a cross-aware `eselect profile` (can set a foreign-arch
//!   profile, which `eselect profile` refuses).
//! - [`repos`] — `eselect repository` limited to **local** repositories
//!   (creating/adding/removing overlays on disk; remote syncing is a TODO).
//! - [`compiler`] — `gcc-config`/`eselect gcc` workalike for compiler profile selection.
//! - [`binutils`] — `binutils-config`/`eselect binutils` workalike for binutils profile selection.
//! - [`linker`] — linker profile selection for ld, lld, mold, etc.
//! - [`clang`] — LLVM/clang slot selection.
//! - [`mirrors`] — `mirrorselect` workalike for managing GENTOO_MIRRORS.

mod binutils;
mod clang;
pub(crate) mod compiler;
mod env_d;
mod linker;
mod mirrors;
mod pkgconf;
pub(crate) mod profile;
mod repos;

use anyhow::Result;
use camino::Utf8PathBuf;
use portage_repo::MakeConf;

use crate::cli::{Cli, SelectCommand};
use crate::style::{C_HOST, C_PREFIX};
use portage_resolve::Roots;

/// Activate the newest binutils profile built into `roots`' merge root for
/// `target` (the `binutils-config` half of `crossdev --setup`'s toolchain
/// activation). Takes `Roots` directly, not `&Cli` — the `cross-<CTARGET>/*`
/// toolchain always lives in the plain outer EROOT (see `crossdev/mod.rs`'s
/// module doc), so callers must pass `Cli::base_roots`, never `Cli::roots`
/// (which would substitute in a `--target`-active sysroot instead).
pub fn activate_binutils(roots: &Roots, target: &str) -> Result<bool> {
    binutils::activate_latest(roots, target)
}

/// Activate the newest gcc profile built into `roots`' merge root for
/// `target` (the `gcc-config` half). Run after [`activate_binutils`]. See its
/// doc comment for why this takes `Roots` rather than `&Cli`.
pub fn activate_compiler(roots: &Roots, target: &str) -> Result<bool> {
    compiler::activate_latest(roots, target)
}

/// The `SLOT` `gcc-config` currently has active for `target` in `roots`, or
/// `None` if no toolchain has been activated there yet.
pub fn current_compiler_slot(roots: &Roots, target: &str) -> Option<String> {
    compiler::current_slot(roots, target)
}

/// Create the `<target>-pkg-config` wrapper in `roots`' merge root if one
/// doesn't already exist (see [`pkgconf`]'s module doc for why this needs to
/// exist at all). Run alongside [`activate_binutils`]/[`activate_compiler`]
/// so a plain `crossdev --setup`/`toolchain --setup` leaves a real, working
/// `pkg-config` behind without an extra manual step.
///
/// `is_native` must be `true` only when `target` is the host's own native
/// CHOST (`crossdev/mod.rs`'s `activate_native_toolchain`), never for a
/// genuine foreign `CTARGET` — see [`pkgconf::activate_pkgconf`]'s doc
/// comment for why this can't be inferred from `roots` alone.
pub fn activate_pkgconf(roots: &Roots, target: &str, is_native: bool) -> Result<bool> {
    pkgconf::activate_pkgconf(roots, target, is_native)
}

/// Dispatch `em select <module> <action>`.
pub async fn run(command: &SelectCommand, globals: &Cli) -> Result<()> {
    match command {
        SelectCommand::Profile { action } => profile::run(action, globals),
        SelectCommand::Repository { action } => repos::run(action, globals),
        SelectCommand::Compiler { action } => compiler::run(action, globals),
        SelectCommand::Binutils { action } => binutils::run(action, globals),
        SelectCommand::Linker { action } => linker::run(action, globals),
        SelectCommand::Clang { action } => clang::run(action, globals),
        SelectCommand::Pkgconf { action } => pkgconf::run(action, globals),
        SelectCommand::Mirrors { action } => mirrors::run(action, globals).await,
        SelectCommand::News { command } => crate::news::run(command, globals),
        SelectCommand::Glsa { command } => crate::glsa::run(command, globals).await,
    }
}

/// The configuration root for `etc/portage` operations: `--config-root`
/// (cross sysroot / offset) when given, else `--prefix`/`--local` overlay, else `/`.
///
/// Use `outer_roots()`, not `roots()`: clap maps a select subcommand's
/// `--target` onto global `Cli::target` (shared long name), which would
/// trigger sysroot substitution. Select only means "which target's
/// config-root state", never "merge into this sysroot".
pub(crate) fn config_portage_dir(globals: &Cli) -> Utf8PathBuf {
    config_portage_dir_for(&globals.outer_roots())
}

/// [`config_portage_dir`], but from an already-computed [`Roots`] rather than
/// `&Cli` — used by [`env_d`] so its crossdev-facing entry points
/// ([`activate_binutils`]/[`activate_compiler`]) can be handed
/// `Cli::base_roots` instead of the `--target`-substituted `Cli::roots`.
///
/// Deliberately uses [`Roots::config_root_explicit`], not
/// [`Roots::config`]: the latter also follows a bare `--root` (`em`'s own
/// self-contained-bootstrap default), but real eselect never derives a
/// config root from `ROOT` alone (its `profile.eselect` module only honours
/// an explicit `PORTAGE_CONFIGROOT`/`EROOT`) — so a plain `em --root R
/// select ...` operates on the host's config unless `--config-root R` is
/// also given, matching that. `crossdev`'s own internal activation
/// (`activate_toolchain`) is unaffected: it passes `Cli::base_roots()`
/// straight to `env_d_dir`/[`config_portage_dir_for`] too, but crossdev
/// always runs under a topology it just bootstrapped itself, not through
/// this config-root guess.
pub(super) fn config_portage_dir_for(roots: &Roots) -> Utf8PathBuf {
    // If config root is explicitly set (--config-root), use it
    if let Some(config) = roots.config_root_explicit() {
        return config.join("etc/portage");
    }
    // If using --local or --prefix, use the overlay directory (already points to etc/portage)
    if let Some(overlay) = roots.config_overlay() {
        return overlay.to_path_buf();
    }
    // Fall back to system root
    Utf8PathBuf::from("/etc/portage")
}

/// Check if we're in a prefix/local context (--local or --prefix without
/// --config-root). `outer_roots()`, not `roots()` — see
/// [`config_portage_dir`]'s doc comment.
pub fn is_prefix_context(globals: &Cli) -> bool {
    is_prefix_context_for(&globals.outer_roots())
}

/// [`is_prefix_context`], but from an already-computed [`Roots`] — see
/// [`config_portage_dir_for`].
pub(super) fn is_prefix_context_for(roots: &Roots) -> bool {
    roots.config_root_explicit().is_none() && roots.config_overlay().is_some()
}

/// Format a source label for display in prefix context.
pub fn source_label(is_host: bool) -> String {
    if is_host {
        format!("{C_HOST} (host){C_HOST:#}")
    } else {
        format!("{C_PREFIX} (prefix){C_PREFIX:#}")
    }
}

/// Get CHOST from make.conf.
pub fn get_chost(globals: &Cli) -> String {
    let make_conf_path = config_portage_dir(globals).join("make.conf");

    let mut paths_to_check = vec![make_conf_path];
    // `--prefix`/`--local` deliberately don't carry their own CHOST — base
    // profile/make.conf come from the host (see `setup.rs`'s bashrc comment
    // and the generated prefix make.conf's own header, "Profile and base
    // make.conf come from the host"). Without this fallback, a prefix's own
    // (CHOST-less) make.conf overlay left `get_chost` with nothing to find,
    // so `select compiler show`/`set` with no explicit `--target` silently
    // derived a bogus target (`arm64-unknown-linux-gnu` from `Cli::arch`'s
    // Gentoo arch name, not the real `aarch64-unknown-linux-gnu` CHOST).
    if is_prefix_context(globals) {
        paths_to_check.push(Utf8PathBuf::from("/etc/portage/make.conf"));
    }

    // `MakeConf` (real brush-parser-based winnow parse, `portage-repo`) over
    // a hand-rolled `line.starts_with("CHOST=")` scan: handles quoting,
    // trailing comments, and later-assignment-wins the same way the rest of
    // this codebase's make.conf reads already do (see e.g.
    // `binpkg.rs::resolve_pkgdir_for_roots`) — a naive line scan would get
    // any of those wrong.
    for path in &paths_to_check {
        if let Ok(mc) = MakeConf::load(path)
            && let Some(chost) = mc.get("CHOST")
        {
            return chost.to_string();
        }
    }
    let arch = globals.arch.as_str();
    format!("{arch}-unknown-linux-gnu")
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// A CHOST= line, however it's quoted, read the same way `get_chost`
    /// itself parses one — used to compute this test's expected value from
    /// the real host config rather than hardcoding it.
    fn read_chost(path: &std::path::Path) -> Option<String> {
        let content = std::fs::read_to_string(path).ok()?;
        content.lines().find_map(|l| {
            let l = l.trim();
            let rest = l.strip_prefix("CHOST=")?;
            Some(rest.trim().trim_matches(['"', '\'']).to_string())
        })
    }

    /// Under `--prefix`, the prefix's own make.conf overlay never sets
    /// CHOST by design (`setup.rs`'s generated template: "Profile and base
    /// make.conf come from the host") — `get_chost` must fall back to the
    /// host's real `/etc/portage/make.conf`, not silently derive a bogus
    /// Prefix make.conf without CHOST falls back to the host's real CHOST.
    #[test]
    fn get_chost_under_prefix_falls_back_to_host_make_conf() {
        let Some(expected) = read_chost(std::path::Path::new("/etc/portage/make.conf")) else {
            eprintln!(
                "skipping: host /etc/portage/make.conf has no CHOST= line \
                 (unusual host, not testable here)"
            );
            return;
        };

        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().to_str().unwrap();
        std::fs::create_dir_all(dir.path().join("etc/portage")).unwrap();
        // Deliberately no CHOST= line -- matches the real generated prefix
        // make.conf template, which never sets one.
        std::fs::write(
            dir.path().join("etc/portage/make.conf"),
            "# no CHOST here, matches a real `em --prefix ... setup` template\n",
        )
        .unwrap();

        let cli = Cli::parse_from(["em", "--prefix", prefix]);
        assert_eq!(get_chost(&cli), expected);
    }

    /// A plain host build (no `--prefix`/`--local`) is unaffected by the
    /// new fallback -- `config_portage_dir` already points straight at the
    /// host's own make.conf for that topology, so `get_chost` must still
    /// read the same real CHOST it always did.
    #[test]
    fn get_chost_host_context_is_unaffected() {
        let Some(expected) = read_chost(std::path::Path::new("/etc/portage/make.conf")) else {
            eprintln!(
                "skipping: host /etc/portage/make.conf has no CHOST= line \
                 (unusual host, not testable here)"
            );
            return;
        };
        // `["em"]` alone (zero args) trips clap's `arg_required_else_help`
        // (prints help and exits the process) — pass `--root /` explicitly,
        // matching `binpkg.rs`'s own tests for the same reason.
        let cli = Cli::parse_from(["em", "--root", "/"]);
        assert!(!is_prefix_context(&cli));
        assert_eq!(get_chost(&cli), expected);
    }
}
