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
/// Found live: `select::pkgconf`'s tests temporarily replace `PATH` with a
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
