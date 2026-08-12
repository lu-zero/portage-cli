//! Shared test-only synchronization for process-global state that would
//! otherwise race across the concurrently-running tests `cargo test`'s
//! default harness runs in one shared process.

use std::sync::Mutex;

/// Serializes any test that temporarily mutates the process-wide `PATH` env
/// var against any *other* test that reads it — directly, or indirectly by
/// spawning an external tool (`tar`/`zstd`/…) that resolves itself via a
/// `PATH` lookup. Env vars are per-process, not per-thread, so a plain
/// save/restore in one test module isn't enough once another module's test
/// touches `PATH` too; every test on either side of the mutation must
/// acquire this same lock.
///
/// synthetic, deliberately tool-free directory list to exercise "no backend
/// reachable" (replacing, not prepending, since a real `pkg-config` on the
/// host/CI machine's ambient `PATH` would otherwise still be found and
/// falsify that exact case). While that window is open,
/// `quickpkg::tests::package_one_builds_gpkg_from_vdb_and_root` spawning
/// `tar` via `portage_binpkg::write_gpkg` failed with a bare ENOENT — not a
/// flaky/theoretical race, a real, consistently-reproducing CI failure
/// (`cargo test --workspace`'s default parallel harness) traced back to
/// this exact PATH-clobbering window, 2026-07-20 — and again in
/// `postprocess::tests` (`compresses_man_pages_and_retargets_symlinks`,
/// `docompress_exclude_is_honored`, `strips_only_in_dostrip_scope`), whose
/// `post_process_image` spawns `bzip2`/`strip` the same way.
static PATH_LOCK: Mutex<()> = Mutex::new(());

/// Acquire [`PATH_LOCK`] for the duration of a test that either mutates
/// `PATH` or spawns an external tool that resolves itself via `PATH` — hold
/// the returned guard for the whole test body.
pub(crate) fn path_lock() -> std::sync::MutexGuard<'static, ()> {
    PATH_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Same rationale as [`PATH_LOCK`], for tests that mutate the process-wide
/// `HOME` env var (e.g. `--local`'s `~/.gentoo` derivation) **or** read it
/// indirectly — real `git`/`gix` operations consult `$HOME/.gitconfig` for
/// config layering even when the calling code never touches `HOME` itself,
/// so `maint::sync::git_gix`'s and `gix_ext::reset`'s tests hold this too,
/// not just [`PATH_LOCK`]. (Investigated as the cause of a 2026-08-10
/// Coverage-CI-only `git_gix`/`gix_ext::reset` failure — it wasn't; that
/// turned out to be [`set_test_git_identity`]'s problem, a deterministic
/// missing-identity error, not a race. Kept here anyway: a concurrent
/// `set_var("HOME", ..)` from `active.rs`/`cli.rs`'s tests — a *different*
/// mutex from `PATH_LOCK` — racing a gix call reading `$HOME` is still a
/// real, if not-yet-observed, hazard.)
static HOME_LOCK: Mutex<()> = Mutex::new(());

/// Acquire [`HOME_LOCK`] for the duration of a test that mutates `HOME`, or
/// that (transitively) reads it — hold the returned guard for the whole
/// test body.
pub(crate) fn home_lock() -> std::sync::MutexGuard<'static, ()> {
    HOME_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Pin `XDG_STATE_HOME` to a fresh temp dir so [`crate::active`] cannot see
/// the developer's real `~/.local/state/em/active` registration (which would
/// make bare-host topology tests flake). Holds [`HOME_LOCK`] because active
/// state resolution also reads `HOME` as a fallback.
///
/// Drop the returned guard (and keep the `TempDir` alive) for the whole test.
pub(crate) fn isolate_active_state() -> (tempfile::TempDir, ActiveStateGuard) {
    let tmp = tempfile::tempdir().expect("tempdir for XDG_STATE_HOME");
    let guard = ActiveStateGuard::new(tmp.path());
    (tmp, guard)
}

/// Restores `XDG_STATE_HOME` on drop. See [`isolate_active_state`].
pub(crate) struct ActiveStateGuard {
    _home: std::sync::MutexGuard<'static, ()>,
    saved: Option<String>,
}

impl ActiveStateGuard {
    fn new(state_parent: &std::path::Path) -> Self {
        let _home = home_lock();
        let saved = std::env::var("XDG_STATE_HOME").ok();
        // SAFETY: held under home_lock; no other test mutates XDG_STATE_HOME.
        unsafe {
            std::env::set_var("XDG_STATE_HOME", state_parent);
        }
        Self { _home, saved }
    }
}

impl Drop for ActiveStateGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.saved {
                Some(v) => std::env::set_var("XDG_STATE_HOME", v),
                None => std::env::remove_var("XDG_STATE_HOME"),
            }
        }
    }
}

/// Set `GIT_AUTHOR_NAME`/`GIT_AUTHOR_EMAIL`/`GIT_COMMITTER_NAME`/
/// `GIT_COMMITTER_EMAIL` to a fixed test identity for the whole process.
///
/// Real `git` invocations spawned via `Command` (e.g. `git_gix`'s and
/// `gix_ext::reset`'s own `git()` test helper) get their identity per-call
/// via `.env(...)` on the child process. This is for the *other* half: the
/// direct `gix` API calls those same tests' production code makes
/// (`gix::open` + fetch/reset), which read process env exactly like real
/// git does but have no per-call override point available to a caller.
///
/// Without an identity from *some* source (env or git config), gix's
/// ref/reflog-write machinery hard-errors —
/// `RefEdit("The reflog could not be created or updated")` — where real
/// git's equivalent operation (`git reset --hard` moving a ref with no new
/// commit) quietly falls back to `$(whoami)@$(hostname)` and succeeds; gix
/// does not replicate that fallback. Confirmed deterministic, not a race:
/// reproduces 100% of the time with no git identity resolvable from any
/// source (`GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
/// GIT_CONFIG_NOSYSTEM=1`, no `GIT_*` env vars), never on a dev machine
/// with a real `~/.gitconfig` — which is exactly the gap between this
/// project's CI runner (fresh, no git identity configured anywhere) and
/// every local dev machine that ever exercised these tests before
/// 2026-08-10. Also a real product gap, not just a test one: `em`'s
/// optional `--features sync-gix` backend would hit this identically on
/// any host that has never run `git config --global user.name` — tracked
/// separately from this test-only fix.
///
/// No restoration needed: the value is a fixed constant no other test
/// cares about, and every caller sets the same one. Callers must still
/// hold [`path_lock`] (and typically [`home_lock`]) for the general
/// environment-variable safety those locks exist for.
///
/// Only ever called from `sync-gix`-gated test modules (`gix_ext::reset`,
/// `maint::sync::git_gix`); the `cfg_attr` keeps it dead-code-clean in the
/// default (no `sync-gix`) build instead of gating the function itself,
/// which would break the `[set_test_git_identity]` link in [`HOME_LOCK`]'s
/// always-compiled doc comment.
#[cfg_attr(not(feature = "sync-gix"), allow(dead_code))]
pub(crate) fn set_test_git_identity() {
    // SAFETY: held under path_lock()/home_lock() by every caller; no other
    // test reads or writes these GIT_* vars.
    unsafe {
        std::env::set_var("GIT_AUTHOR_NAME", "t");
        std::env::set_var("GIT_AUTHOR_EMAIL", "t@t");
        std::env::set_var("GIT_COMMITTER_NAME", "t");
        std::env::set_var("GIT_COMMITTER_EMAIL", "t@t");
    }
}
