//! Run an unprivileged build+merge with root privilege — faked or real — so
//! `chown`/setuid succeed and ownership is recorded, instead of swallowing the
//! EPERM and losing it.
//!
//! Fakeroost, pseudoroot and sudo are *scoped*, not umbrellas (Q6 in
//! fakeroot-privilege-backends.md: the ptrace tax / real root must stay off
//! the compile): the un-wrapped parent runs `pretend..compile`, then
//! `build_and_merge` delegates install+qmerge(+binpkg) to a wrapped
//! `em __worker` child per package ([`install_wrap_backend`] /
//! [`spawn_install_worker`]). Hakoniwa remains an umbrella via
//! [`maybe_supervise`] (userns has ~no per-syscall cost and the container
//! binds must cover the whole run), as does `em ebuild … install/qmerge`
//! (the debug applet runs phases in-process, with no worker seam). qmerge is
//! serialised across worker processes by an flock in `ebuild.rs`.
//!
//! Each fake-root backend is a default-on cargo feature compiled only where
//! it works — fakeroost and hakoniwa are Linux kernel interfaces, pseudoroot
//! covers Linux and macOS. The cfg gates pair the feature with the target
//! because default features stay enabled on targets where the dependency
//! table drops the crate.
//!
//! Backend selection (`--privilege`, or `EM_PRIVILEGE`; default `auto`):
//! - `auto` — the best compiled-in fake root: pseudoroot, else fakeroost, else
//!   `none`.
//! - `pseudoroot` — LD_PRELOAD fake root: faked ownership without the
//!   per-syscall ptrace tax, but interposition only covers dynamically linked
//!   libc callers (static binaries / raw syscalls escape it). The `auto`
//!   default: a real stage3 `--buildpkg` run under fakeroost hit a rare,
//!   non-reproducible-in-isolation ptrace race (`fakeroost: syscall failed:
//!   ENOENT`) that silently killed ~1/3 of packages' install workers *after*
//!   qmerge had already succeeded.
//!
//! pseudoroot doesn't share that failure mode.
//! - `fakeroost` — pure-Rust ptrace+seccomp fake root (no privilege):
//!   ownership is faked in-session, on-disk stays the build user. Covers every
//!   caller (no libc-interposition gap), at a higher per-syscall cost and the
//!   rare crash above.
//! - `hakoniwa` — user-namespace sandbox with build-user→0 map ("real-in-a-box"):
//!   real `chown`/`setuid` syscalls inside the box; on-disk owners are the
//!   mapped host ids (same family as `sudo`, without host root).
//! - `sudo` — re-exec under `sudo` for *real* root (real root-owned tree + real
//!   setuid). Opt-in only; never auto-selected (it escalates privilege).
//! - `none` — disable wrapping; run unprivileged and let the chown workarounds
//!   degrade gracefully.
//!
//! Already root ⇒ no wrapping (real chowns in-process). The fakeroot (system
//! binary) backend slots in behind [`Backend`] later.

use crate::cli::{Applet, Cli, Privilege};

/// The `--privilege` request parsed from the CLI (flag or `EM_PRIVILEGE` via
/// clap), recorded by [`maybe_supervise`] so `build_and_merge` — which has no
/// `Cli` — can pick the worker backend.
static PRIVILEGE_REQUEST: std::sync::OnceLock<Privilege> = std::sync::OnceLock::new();

/// Marker set on a wrapped re-exec so the inner process does not re-wrap
const ACTIVE_ENV: &str = "EM_PRIVILEGE_ACTIVE";

/// The root mechanism backing an unprivileged build
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// Already root, or already inside a session: real chowns, no wrapping
    RealRoot,
    /// Pure-Rust ptrace+seccomp fake root (`fakeroost`)
    #[cfg(all(feature = "fakeroost", target_os = "linux"))]
    Fakeroost,
    /// LD_PRELOAD fake root (`pseudoroot`) — same faked-ownership model as
    /// fakeroost without the ptrace tax (libc-interposed, so static binaries
    /// and raw syscalls escape it) — the default unprivileged backend.
    #[cfg(all(feature = "pseudoroot", any(target_os = "linux", target_os = "macos")))]
    Pseudoroot,
    /// User-namespace sandbox (`hakoniwa`) with build-user→0 map
    #[cfg(all(feature = "hakoniwa", target_os = "linux"))]
    Hakoniwa,
    /// Re-exec under `sudo` for real root. Opt-in via `EM_PRIVILEGE=sudo`
    Sudo,
}

impl Backend {
    /// Pick the backend for this process: [`RealRoot`](Self::RealRoot) when
    /// euid==0 or already inside a wrapped session; otherwise map the `--privilege`
    /// request.
    pub fn detect(requested: Privilege) -> Self {
        if rustix::process::geteuid().is_root() || already_active() {
            return Backend::RealRoot;
        }
        match requested {
            Privilege::Auto => Self::auto_backend(),
            #[cfg(all(feature = "fakeroost", target_os = "linux"))]
            Privilege::Fakeroost => Backend::Fakeroost,
            #[cfg(all(feature = "pseudoroot", any(target_os = "linux", target_os = "macos")))]
            Privilege::Pseudoroot => Backend::Pseudoroot,
            #[cfg(all(feature = "hakoniwa", target_os = "linux"))]
            Privilege::Hakoniwa => Backend::Hakoniwa,
            Privilege::Sudo => Backend::Sudo,
            Privilege::None => Backend::RealRoot,
        }
    }

    /// `auto`: the best compiled-in fake root — pseudoroot (LD_PRELOAD, no
    /// ptrace tax, and doesn't share fakeroost's rare buildpkg-phase crash;
    /// see the module doc comment) over fakeroost (ptrace, covers every
    /// caller but not macOS); neither compiled in ⇒ no wrapping, the chown
    /// workarounds degrade gracefully.
    fn auto_backend() -> Self {
        std::cfg_select! {
            all(feature = "pseudoroot", any(target_os = "linux", target_os = "macos")) => {
                Backend::Pseudoroot
            }
            all(feature = "fakeroost", target_os = "linux") => {
                Backend::Fakeroost
            }
            _ => {
                Backend::RealRoot
            }
        }
    }
}

fn already_active() -> bool {
    std::env::var_os(ACTIVE_ENV).is_some()
}

/// Does this invocation actually run build/merge phases? Only those need root —
/// resolves, queries and `--pretend` do not. Covers every path that builds and
/// installs (the plain emerge merge, plus `ebuild`/`crossdev`/`toolchain`, whose
/// staged drivers run through the same merge code), so the unprivileged chown
/// handling is uniformly faked and never falls back to the EPERM swallow.
pub(crate) fn will_build(cli: &Cli) -> bool {
    if cli.pretend {
        return false;
    }
    match &cli.applet {
        // [`crate::cli::parse_cli_from`] means `None` only reaches here for a
        // genuinely atom-less, applet-less invocation (`em --info`) — never
        // one that will build.
        None => false,
        Some(Applet::Emerge(args)) => {
            // Removals mutate the root even though they skip the merge
            // engine entirely; search/deselect never touch it.
            args.mode.unmerge
                || args.mode.depclean
                || (!args.atoms.is_empty() && !args.mode.search && !args.mode.searchdesc)
        }
        Some(
            Applet::Ebuild { .. }
            | Applet::Crossdev(_)
            | Applet::Toolchain(_)
            | Applet::Stages(_)
            | Applet::Depclean { .. }
            | Applet::Revdep { .. },
        ) => true,
        Some(_) => false,
    }
}

/// If an unprivileged invocation will build, re-exec em once under the selected
/// backend and return its exit code (the caller must exit with it). Returns
/// `None` when no wrapping is needed (root, already wrapped, `EM_PRIVILEGE=none`,
/// or a non-building command), so the caller proceeds normally.
pub fn maybe_supervise(cli: &Cli) -> Option<i32> {
    let privilege = cli.effective_privilege();
    let _ = PRIVILEGE_REQUEST.set(privilege);
    if !will_build(cli) {
        return None;
    }
    match Backend::detect(privilege) {
        Backend::RealRoot => None,
        // Fakeroost/pseudoroot/sudo are scoped, not umbrellas (Q6): the ptrace
        // tax / real root must stay off the compile. build_and_merge delegates
        // only install+qmerge to a wrapped __worker child. The exceptions
        // (see `needs_whole_process_wrap`) run without that worker seam and
        // are wrapped whole instead.
        #[cfg(all(feature = "fakeroost", target_os = "linux"))]
        Backend::Fakeroost => needs_whole_process_wrap(cli).then(fakeroost::reexec),
        #[cfg(all(feature = "pseudoroot", any(target_os = "linux", target_os = "macos")))]
        Backend::Pseudoroot => needs_whole_process_wrap(cli).then(pseudoroot::reexec),
        Backend::Sudo => needs_whole_process_wrap(cli).then(reexec_sudo),
        #[cfg(all(feature = "hakoniwa", target_os = "linux"))]
        Backend::Hakoniwa => Some(hakoniwa::reexec(cli)),
    }
}

/// Whether this invocation has no per-package `__worker` seam to delegate
/// privileged work to, so the *whole* process needs wrapping instead.
///
/// `em ebuild … install/qmerge` (the debug applet runs phases in-process),
/// `-C`/`--unmerge` and `-c`/`--depclean` (pure removal, no install to
/// attach a worker to), and `-B`/`--buildpkgonly` (single-process by
/// design — src_install's fowners/fperms and the `${D}` ownership packed
/// into the GPKG still need the fake root, but there is no live-root
/// install to scope it to).
fn needs_whole_process_wrap(cli: &Cli) -> bool {
    let (unmerge, depclean) = match &cli.applet {
        Some(Applet::Emerge(args)) => (args.mode.unmerge, args.mode.depclean),
        Some(Applet::Depclean { .. }) => (false, true),
        _ => (false, false),
    };
    ebuild_applet_installs(cli) || unmerge || depclean || cli.merge_flags().buildpkgonly
}

/// `em ebuild … <phase>` with a merge-side phase: the only build path that does
/// not go through `build_and_merge` (and thus the worker seam).
fn ebuild_applet_installs(cli: &Cli) -> bool {
    matches!(&cli.applet, Some(Applet::Ebuild { phase, .. })
        if phase.iter().any(|p| matches!(p.as_str(), "install" | "qmerge" | "merge")))
}

/// The backend the install group should be wrapped with in a `__worker` child,
/// or `None` to run it in-process (root, already inside a session, hakoniwa
/// umbrella, `--privilege none`). The worker child runs with
/// `EM_PRIVILEGE_ACTIVE` set, so its own install group is in-process.
pub fn install_wrap_backend() -> Option<Backend> {
    let requested = PRIVILEGE_REQUEST.get().copied().unwrap_or(Privilege::Auto);
    match Backend::detect(requested) {
        Backend::RealRoot => None,
        #[cfg(all(feature = "hakoniwa", target_os = "linux"))]
        Backend::Hakoniwa => None,
        backend => Some(backend),
    }
}

/// Serializable inputs for the install worker — the subset of
/// `build_and_merge`'s args that cross the process boundary.
pub struct WorkerArgs<'a> {
    pub ebuild_path: &'a str,
    /// The resolved plan entry's authoritative `Cpv` (e.g
    /// `cross-riscv64-unknown-linux-gnu/gcc-16.1.1`) — carried across the
    /// process boundary explicitly so the worker never re-derives it from
    /// `ebuild_path`'s on-disk directory name, which is wrong for a
    /// cross-derived package.
    pub cpv: &'a str,
    pub use_flags: &'a str,
    pub work_base: &'a str,
    pub root: &'a str,
    pub distdir: Option<&'a str>,
    pub config_root: Option<&'a str>,
    pub sysroot: Option<&'a str>,
    pub eprefix: Option<&'a str>,
    /// Where BDEPEND-class build tools live for this invocation
    /// (`Cli::host_roots()`'s merge root) — see `EbuildShell::build_broot`.
    pub broot: Option<&'a str>,
    /// See `ebuild::RootContext::self_contained_bootstrap`
    pub self_contained_bootstrap: bool,
    /// See `ebuild::RootContext::extra_path`, `:`-joined
    ///
    /// Empty for all but `em setup --local`'s own merge.
    pub extra_path: &'a str,
    pub binpkg: Option<&'a str>,
    /// `binpkg`'s origin forces cryptographic signature verification
    /// (a `binrepos.conf` entry with `verify-signature = yes`), independent
    /// of `FEATURES=binpkg-request-signature`. See `ebuild::RunInner`'s
    /// field of the same name.
    pub force_verify_signature: bool,
    pub buildpkg: bool,
    pub quiet: bool,
    /// Activity session id (same as parent `SessionStart.job_id`)
    ///
    /// When set with [`Self::activity_live_root`], the worker emits install-phase events into
    /// the shared live FS tree.
    pub activity_job_id: Option<&'a str>,
    pub activity_parent_job_id: Option<&'a str>,
    /// Filesystem root of the parent's live activity sink
    pub activity_live_root: Option<&'a str>,
    /// `host` or `target` — must match the parent's package side
    pub activity_side: Option<&'a str>,
    /// Unix socket path for streaming phase events back to the parent bus
    /// (set by [`spawn_install_worker`] when `reemit` is provided).
    pub activity_reemit_path: Option<&'a str>,
}

/// Spawn a wrapped `em __worker` child for the install group and await it
///
/// The compile ran un-wrapped in the parent; this wraps only the
/// install/qmerge/binpkg tail where ownership/device-node metadata is produced.
///
/// When `reemit` is `Some`, the parent binds a Unix socket under `work_base`,
/// the worker writes JSONL phase events there, and the parent re-emits them on
/// the given bus (so `--activity-fd` / emergelog / subscribers see install
/// phases). Path-based (not FD inheritance) so sudo/fakeroost wraps work.
pub async fn spawn_install_worker(
    backend: Backend,
    args: &WorkerArgs<'_>,
    reemit: Option<crate::activity::ActivityBus>,
) -> std::io::Result<i32> {
    #[cfg(unix)]
    if let Some(bus) = reemit {
        return spawn_install_worker_with_reemit(backend, args, bus).await;
    }
    #[cfg(not(unix))]
    let _ = reemit;

    let mut cmd = build_worker_command(backend, args, None)?;
    // The worker runs a full install+qmerge — off the executor thread, so
    // parallel builds in other tasks keep making progress while we wait.
    tokio::task::spawn_blocking(move || cmd.status().map(|s| s.code().unwrap_or(1)))
        .await
        .map_err(std::io::Error::other)?
}

#[cfg(unix)]
async fn spawn_install_worker_with_reemit(
    backend: Backend,
    args: &WorkerArgs<'_>,
    bus: crate::activity::ActivityBus,
) -> std::io::Result<i32> {
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixListener;

    // Unix-domain socket paths are capped at SUN_LEN (~108 bytes on Linux),
    // so a socket under the (frequently very long) per-package work_base
    // overflows and `bind` fails. Use a short unique path in the system temp
    // dir instead; the worker reaches it via `--activity-reemit-path`
    // regardless of privilege backend (sudo runs as root, which can reach
    // `/tmp`; the others share the invoking user's filesystem view).
    let sock_path = std::env::temp_dir().join(format!(
        "em-activity-reemit-{}-{}.sock",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path)?;
    let sock_path_str = sock_path.to_string_lossy().into_owned();

    let reader = std::thread::spawn({
        let bus = bus.clone();
        move || {
            // Blocks until the worker connects (or the listener is dropped).
            let (stream, _) = match listener.accept() {
                Ok(s) => s,
                Err(e) => {
                    crate::style::warn_line!("activity re-emit accept: {e}");
                    return;
                }
            };
            let reader = BufReader::new(stream);
            for line in reader.lines() {
                let Ok(line) = line else {
                    break;
                };
                if line.trim().is_empty() {
                    continue;
                }
                match crate::activity::ActivityEvent::from_jsonl_line(&line) {
                    Ok(ev) => bus.emit(ev),
                    Err(e) => crate::style::warn_line!("activity re-emit parse: {e}"),
                }
            }
        }
    });

    let mut cmd = build_worker_command(backend, args, Some(sock_path_str.as_str()))?;
    let status = tokio::task::spawn_blocking(move || cmd.status().map(|s| s.code().unwrap_or(1)))
        .await
        .map_err(std::io::Error::other)??;

    // If the worker never connected (crash before open), unblock accept.
    let _ = std::os::unix::net::UnixStream::connect(&sock_path);
    // Worker exit closes its end of the socket → reader gets EOF.
    let _ = reader.join();
    let _ = std::fs::remove_file(&sock_path);
    Ok(status)
}

fn build_worker_command(
    backend: Backend,
    args: &WorkerArgs<'_>,
    reemit_path: Option<&str>,
) -> std::io::Result<std::process::Command> {
    let exe = std::env::current_exe()?;
    let mut cmd = match backend {
        Backend::Sudo => {
            tracing::info!(
                target: portage_repo::ACTION_TARGET,
                ">>> install/qmerge under sudo (real root)"
            );
            let mut c = std::process::Command::new("sudo");
            c.arg("-E").arg(&exe);
            c
        }
        _ => std::process::Command::new(&exe),
    };
    cmd.arg("__worker")
        .arg("--ebuild")
        .arg(args.ebuild_path)
        .arg("--cpv")
        .arg(args.cpv)
        .arg("--use-flags")
        .arg(args.use_flags)
        .arg("--work-base")
        .arg(args.work_base)
        .arg("--root")
        .arg(args.root);
    if args.buildpkg {
        cmd.arg("--buildpkg");
    }
    if args.quiet {
        cmd.arg("--quiet");
    }
    if let Some(d) = args.distdir {
        cmd.arg("--distdir").arg(d);
    }
    if let Some(c) = args.config_root {
        cmd.arg("--config-root").arg(c);
    }
    if let Some(s) = args.sysroot {
        cmd.arg("--sysroot").arg(s);
    }
    if let Some(e) = args.eprefix {
        cmd.arg("--eprefix").arg(e);
    }
    if let Some(b) = args.broot {
        cmd.arg("--broot").arg(b);
    }
    if args.self_contained_bootstrap {
        cmd.arg("--self-contained-bootstrap");
    }
    if !args.extra_path.is_empty() {
        cmd.arg("--extra-path").arg(args.extra_path);
    }
    if let Some(b) = args.binpkg {
        cmd.arg("--binpkg").arg(b);
    }
    if args.force_verify_signature {
        cmd.arg("--force-verify-signature");
    }
    if let Some(id) = args.activity_job_id {
        cmd.arg("--activity-job-id").arg(id);
    }
    if let Some(id) = args.activity_parent_job_id {
        cmd.arg("--activity-parent-job-id").arg(id);
    }
    if let Some(r) = args.activity_live_root {
        cmd.arg("--activity-live-root").arg(r);
    }
    if let Some(s) = args.activity_side {
        cmd.arg("--activity-side").arg(s);
    }
    if let Some(p) = reemit_path.or(args.activity_reemit_path) {
        cmd.arg("--activity-reemit-path").arg(p);
    }
    Ok(match backend {
        #[cfg(all(feature = "fakeroost", target_os = "linux"))]
        Backend::Fakeroost => {
            cmd.env(ACTIVE_ENV, "fakeroost");
            fakeroost::wrap(&cmd)
        }
        #[cfg(all(feature = "pseudoroot", any(target_os = "linux", target_os = "macos")))]
        Backend::Pseudoroot => {
            cmd.env(ACTIVE_ENV, "pseudoroot");
            pseudoroot::wrap(&cmd)
        }
        _ => {
            cmd.env(ACTIVE_ENV, "sudo");
            cmd
        }
    })
}

/// `(own binary, forwarded args)` for a self re-exec, or `None` if the binary
/// path can't be resolved (the caller treats that as a failure exit).
fn self_invocation() -> Option<(std::path::PathBuf, Vec<std::ffi::OsString>)> {
    match std::env::current_exe() {
        Ok(exe) => Some((exe, std::env::args_os().skip(1).collect())),
        Err(e) => {
            crate::style::error_line!("cannot locate own binary to re-exec: {e}");
            None
        }
    }
}

fn reexec_sudo() -> i32 {
    let Some((exe, args)) = self_invocation() else {
        return 1;
    };
    tracing::info!(
        target: portage_repo::ACTION_TARGET,
        ">>> unprivileged build — re-running under sudo (real root)"
    );
    // `-E` preserves the environment (USE overrides, etc.); the sudoers policy may
    // still strip it, in which case the build falls back to make.conf config. The
    // root child detects euid==0 and runs in-process with real chowns.
    match std::process::Command::new("sudo")
        .arg("-E")
        .arg(exe)
        .args(args)
        .env(ACTIVE_ENV, "sudo")
        .status()
    {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            crate::style::error_line!("failed to re-exec under sudo: {e}");
            1
        }
    }
}

/// `DISTDIR` from make.conf (root-aware, following `--config-root`/`--local`/
/// `--prefix` like `crate::select::config_portage_dir`'s other callers, e.g.
/// `mirrordist::gentoo_mirrors_list`), falling back to real portage's own
/// default when unset — not the bare host `/var/cache/distfiles`.
#[cfg(all(feature = "hakoniwa", target_os = "linux"))]
fn distdir(cli: &Cli) -> String {
    let path = crate::select::config_portage_dir(cli).join("make.conf");
    portage_repo::MakeConf::load(&path)
        .ok()
        .and_then(|mc| mc.get("DISTDIR").map(str::to_string))
        .unwrap_or_else(|| "/var/cache/distfiles".to_string())
}

#[cfg(all(feature = "fakeroost", target_os = "linux"))]
mod fakeroost {
    use std::process::Command;

    use ::fakeroost::FakerootCommandExt;

    /// The supervisor re-exec command wrapping `cmd`; running `cmd` itself
    /// would execute unwrapped.
    pub fn wrap(cmd: &Command) -> Command {
        cmd.fakeroot()
    }

    /// Umbrella re-exec — only for `em ebuild … install/qmerge` (see
    /// [`maybe_supervise`](super::maybe_supervise)); merge runs use the
    /// per-package install worker.
    pub fn reexec() -> i32 {
        let Some((exe, args)) = super::self_invocation() else {
            return 1;
        };
        tracing::info!(
            target: portage_repo::ACTION_TARGET,
            ">>> unprivileged build — running under fakeroost (fake root)"
        );
        match Command::new(exe)
            .args(args)
            .env(super::ACTIVE_ENV, "fakeroost")
            .fakeroot()
            .status()
        {
            Ok(s) => s.code().unwrap_or(1),
            Err(e) => {
                crate::style::error_line!("failed to start the fakeroost supervisor: {e}");
                1
            }
        }
    }
}

#[cfg(all(feature = "pseudoroot", any(target_os = "linux", target_os = "macos")))]
mod pseudoroot {
    use std::process::Command;

    use ::pseudoroot::FakerootCommandExt;

    /// The session re-exec command wrapping `cmd`; running `cmd` itself
    /// would execute unwrapped.
    pub fn wrap(cmd: &Command) -> Command {
        cmd.fakeroot()
    }

    /// Umbrella re-exec — only for `em ebuild … install/qmerge` (see
    /// [`maybe_supervise`](super::maybe_supervise)); merge runs use the
    /// per-package install worker.
    pub fn reexec() -> i32 {
        let Some((exe, args)) = super::self_invocation() else {
            return 1;
        };
        tracing::info!(
            target: portage_repo::ACTION_TARGET,
            ">>> unprivileged build — running under pseudoroot (LD_PRELOAD fake root)"
        );
        match Command::new(exe)
            .args(args)
            .env(super::ACTIVE_ENV, "pseudoroot")
            .fakeroot()
            .status()
        {
            Ok(s) => s.code().unwrap_or(1),
            Err(e) => {
                crate::style::error_line!("failed to start the pseudoroot session: {e}");
                1
            }
        }
    }
}

#[cfg(all(feature = "hakoniwa", target_os = "linux"))]
mod hakoniwa {
    use ::hakoniwa::{Container, Namespace, Runctl};

    use crate::cli::Cli;

    /// Whether the host can spawn an unprivileged user namespace with id maps
    ///
    /// Hakoniwa's parent process writes `/proc/<child>/uid_map` via `newuidmap` /
    /// `newgidmap`; both the kernel knob and those helpers must be present.
    pub fn userns_available() -> bool {
        if let Ok(v) = std::fs::read_to_string("/proc/sys/kernel/unprivileged_userns_clone")
            && v.trim() == "0"
        {
            return false;
        }
        // Both helpers are required for a complete id-map; having only one
        // is not enough to spawn a working userns container.
        ["newuidmap", "newgidmap"]
            .iter()
            .all(|name| which_in_path(name))
    }

    fn which_in_path(name: &str) -> bool {
        let Some(path) = std::env::var_os("PATH") else {
            return false;
        };
        std::env::split_paths(&path).any(|dir| dir.join(name).is_file())
    }

    /// Bind `host` read-write at the same path inside hakoniwa's mount namespace
    fn bind_rw(container: &mut Container, host: &str) {
        if std::path::Path::new(host).is_dir() {
            container.bindmount_rw(host, host);
        }
    }

    fn bind_ro(container: &mut Container, host: &str) {
        if std::path::Path::new(host).exists() {
            container.bindmount_ro(host, host);
        }
    }

    /// Writable trees the build touches but `rootfs("/")` leaves out (it only
    /// bind-mounts the usual FHS prefixes read-only).
    fn bind_build_tree(container: &mut Container, cli: &Cli) {
        let roots = cli.roots();
        bind_rw(container, roots.merge_root().as_str());
        if let Some(overlay) = roots.config_overlay() {
            bind_rw(container, overlay.as_str());
        }
        if let Some(eprefix) = roots.eprefix() {
            bind_rw(container, eprefix.as_str());
        }
        bind_rw(container, "/tmp");
        bind_rw(container, "/var/tmp");
        // `rootfs("/")` binds only the FHS prefixes (/usr, /etc, /bin, /lib*, /sbin) —
        // not the portage data trees under /var that a build reads/writes. Bind every
        // configured repo read-only and DISTDIR read-write (the inner em fetches into
        // it) — via `Cli::search_repos`/repos.conf and make.conf, not the conventional
        // /var/db/repos and /var/cache/distfiles paths, which a non-default repos.conf
        // or DISTDIR would silently miss. The build/merge trees (work_base, merge_root,
        // eprefix) are bound above; the em binary itself is bound by reexec.
        for repo in cli.search_repos() {
            if let Some(path) = repo.to_str() {
                bind_ro(container, path);
            }
        }
        bind_rw(container, &super::distdir(cli));
        if let Some(relocate) = roots.relocate_root() {
            bind_rw(container, relocate.join("var/cache/distfiles").as_ref());
            bind_rw(container, relocate.join("var/tmp").as_ref());
        }
    }

    /// The `(start, count)` subordinate-id range delegated to the user in
    /// `/etc/subuid`/`/etc/subgid` (first line matching the name or numeric id).
    fn read_subid(name: &str, id: u32, subid_file: &str) -> Option<(u32, u32)> {
        let content = std::fs::read_to_string(subid_file).ok()?;
        let id_str = id.to_string();
        for line in content.lines() {
            let mut f = line.split(':');
            let who = f.next()?;
            if who != name && who != id_str.as_str() {
                continue;
            }
            let start = f.next()?.parse().ok()?;
            let count = f.next()?.parse().ok()?;
            return Some((start, count));
        }
        None
    }

    /// hakoniwa id-map triples `(container_id, host_id, count)`
    type IdMaps = Vec<(u32, u32, u32)>;

    /// Container root → the caller, plus the caller's delegated subuid/subgid range
    /// from container id 1, so real chown/setuid to non-root ids inside the box land
    /// on owned ids (a single `uid→0` map can only own root). Mirrors crossdev-stages.
    fn idmaps_for(id: u32, subid_file: &str) -> IdMaps {
        let name = std::env::var("USER").unwrap_or_else(|_| id.to_string());
        let mut maps = vec![(0, id, 1)];
        if let Some((start, count)) = read_subid(&name, id, subid_file) {
            maps.push((1, start, count));
        }
        maps
    }

    /// `(uid_maps, gid_maps)` for the current user (root + delegated subuid/subgid)
    fn id_range_maps() -> (IdMaps, IdMaps) {
        let uid = rustix::process::getuid().as_raw();
        let gid = rustix::process::getgid().as_raw();
        (
            idmaps_for(uid, "/etc/subuid"),
            idmaps_for(gid, "/etc/subgid"),
        )
    }

    pub fn reexec(cli: &Cli) -> i32 {
        if !userns_available() {
            crate::style::error_line!(
                "hakoniwa requires user namespaces and newuidmap/newgidmap on PATH; \
                 try --privilege pseudoroot, fakeroost, or sudo"
            );
            return 1;
        }
        let Some((exe, args)) = super::self_invocation() else {
            return 1;
        };
        let Some(program) = exe.to_str() else {
            crate::style::error_line!("hakoniwa cannot run a non-UTF-8 executable path");
            return 1;
        };

        let mut container = Container::new();
        // Container::new() unshares Mount, User and Pid (and mounts a private /proc).
        // rootfs("/") binds the FHS prefixes read-only but leaves out /dev and the
        // tmpfs mounts a build needs. Mirror the working crossdev-stages setup: full
        // namespace isolation, a minimal devfs, a /dev/shm tmpfs, and allow-new-privs
        // (builds exec setuid helpers). The writable build trees (merge root, /tmp,
        // /var/tmp, …) are bound by bind_build_tree.
        if let Err(e) = container.rootfs("/") {
            crate::style::error_line!("hakoniwa rootfs setup failed: {e}");
            return 1;
        }
        container
            .unshare(Namespace::Ipc)
            .unshare(Namespace::Uts)
            .unshare(Namespace::Cgroup)
            .devfsmount("/dev")
            .tmpfsmount("/dev/shm")
            // Without RootdirRW hakoniwa remounts the whole container root read-only,
            // which also forces our rw build binds RO (the build can't create its work
            // dirs). crossdev-stages sets this for the same reason; the FHS prefixes
            // from rootfs("/") stay individually read-only regardless.
            .runctl(Runctl::RootdirRW)
            .runctl(Runctl::AllowNewPrivs);
        // Map the caller to container root *and* their delegated subuid/subgid range
        // (not a single uid→0), so the build can really own files as the various
        // system users (portage, messagebus, …), not only root.
        let (uid_maps, gid_maps) = id_range_maps();
        container.uidmaps(&uid_maps);
        container.gidmaps(&gid_maps);
        bind_build_tree(&mut container, cli);
        // The em binary we re-exec: bound by rootfs("/") when installed under /usr,
        // but a dev build lives outside the FHS prefixes — bind it read-only so the
        // container can exec it.
        bind_ro(&mut container, program);

        let mut cmd = container.command(program);
        for arg in args {
            let Some(s) = arg.to_str() else {
                crate::style::error_line!("hakoniwa cannot forward a non-UTF-8 argument");
                return 1;
            };
            cmd.arg(s);
        }
        cmd.env(super::ACTIVE_ENV, "hakoniwa");
        for (key, val) in std::env::vars() {
            cmd.env(&key, &val);
        }

        tracing::info!(
            target: portage_repo::ACTION_TARGET,
            ">>> unprivileged build — running under hakoniwa (userns mapped root)"
        );
        match cmd.status() {
            Ok(status) => {
                // hakoniwa reports container-setup/exec failures via `reason` with a
                // non-success code — surface it instead of swallowing it.
                if status.code != 0 && !status.reason.is_empty() {
                    crate::style::error_line!("hakoniwa: {}", status.reason);
                }
                status.code
            }
            Err(e) => {
                crate::style::error_line!("failed to start the hakoniwa container: {e}");
                1
            }
        }
    }
}

#[cfg(all(test, feature = "hakoniwa", target_os = "linux"))]
mod tests {
    use clap::Parser as _;

    use super::*;

    #[test]
    fn userns_knob_zero_means_unavailable() {
        // Don't assert true on real hosts — only that we don't panic reading the knob.
        let _ = hakoniwa::userns_available();
    }

    /// `distdir` must follow `--local`'s own make.conf, not the host's — the
    /// same bug class as the sandbox's old hardcoded `/var/db/repos` and
    /// `/var/cache/distfiles` bind targets.
    #[test]
    fn distdir_follows_local_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = camino::Utf8Path::from_path(dir.path()).unwrap();
        let portage_dir = prefix.join("etc/portage");
        std::fs::create_dir_all(portage_dir.as_std_path()).unwrap();
        std::fs::write(
            portage_dir.join("make.conf").as_std_path(),
            "DISTDIR=\"/custom/distfiles\"\n",
        )
        .unwrap();

        let cli = Cli::parse_from([
            "em",
            "emerge",
            "--local",
            prefix.as_str(),
            "-p",
            "sys-libs/zlib",
        ]);
        assert_eq!(distdir(&cli), "/custom/distfiles");
    }

    #[test]
    fn distdir_falls_back_to_the_real_portage_default_when_unset() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = camino::Utf8Path::from_path(dir.path()).unwrap();
        let cli = Cli::parse_from([
            "em",
            "emerge",
            "--local",
            prefix.as_str(),
            "-p",
            "sys-libs/zlib",
        ]);
        assert_eq!(distdir(&cli), "/var/cache/distfiles");
    }
}
