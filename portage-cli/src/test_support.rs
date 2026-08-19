//! Shared test-only synchronization for process-global state that would
//! otherwise race across the concurrently-running tests `cargo test`'s
//! default harness runs in one shared process.

use std::sync::Mutex;

/// Serializes any test that temporarily mutates the process-wide `PATH` env
/// var against any *other* test that reads it — directly, or indirectly by
/// spawning an external tool (`tar`/`zstd`/…) that resolves itself via a
/// `PATH` lookup. Env vars are per-process, not per-thread, so a plain
/// save/restore in one test module isn't enough; every test on either side
/// of the mutation must acquire this same lock.
///
/// Found live: `select::pkgconf`'s tests temporarily replace `PATH` with a
/// synthetic, tool-free directory list to exercise "no backend reachable".
/// While that window is open, `quickpkg::tests` spawning `tar` (and
/// `postprocess::tests` spawning `bzip2`/`strip`) failed with a bare
/// ENOENT — a real, consistently-reproducing CI failure (2026-07-20)
/// traced back to this exact PATH-clobbering window.
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
/// `HOME` env var **or** read it indirectly — real `git`/`gix` operations
/// consult `$HOME/.gitconfig` even when the calling code never touches
/// `HOME` itself, so `maint::sync::git_gix`'s and `gix_ext::reset`'s tests
/// hold this too, not just [`PATH_LOCK`].
///
/// Investigated as the cause of a 2026-08-10 Coverage-CI-only failure — it
/// wasn't (that was [`set_test_git_identity`]'s problem, a deterministic
/// missing-identity error). Kept anyway: a concurrent `set_var("HOME", ..)`
/// racing a gix call reading `$HOME` is still a real, if unobserved, hazard.
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
/// `GIT_COMMITTER_EMAIL` to a fixed test identity for the whole process, for
/// direct `gix` API calls (which read process env like real git but have no
/// per-call override point) — real `git` `Command` invocations already get
/// their identity per-call via `.env(...)`. See [the CI-only gix identity
/// gap](../../docs/design/testing.md) for why this is needed at all.
///
/// No restoration needed: every caller sets the same fixed constant.
/// Callers must still hold [`path_lock`] (and typically [`home_lock`]).
/// Only called from `sync-gix`-gated test modules; the `cfg_attr` keeps it
/// dead-code-clean in the default build instead of gating the function
/// itself, which would break the `[set_test_git_identity]` link in
/// [`HOME_LOCK`]'s doc comment.
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
