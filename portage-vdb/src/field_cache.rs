//! Process-wide memoization for VDB flat-file reads, keyed by absolute path
//!
//! This module knows nothing about `std::fs` or any other I/O backend —
//! [`get_or_fetch`] takes a closure that performs the actual uncached read,
//! so the cache stays correct regardless of how (or from where) a field is
//! ultimately fetched. [`package`](crate::package) is the only current
//! caller, and is the one that knows it's reading real files.
//!
//! A VDB entry is immutable once written except through [`crate::Vdb::register`]
//! (overwrite in place, e.g. a same-version USE-flag rebuild) or
//! [`crate::Vdb::unregister`] (removal) — both call [`invalidate_entry`] for
//! exactly that entry, so the cache reflects "as of the last write this
//! process made" rather than a point-in-time snapshot that could go stale
//! across a real merge run. That's what makes a process-wide cache safe
//! rather than just fast: independent VDB scans of the same root (measured:
//! `em -p`'s depgraph build reads the host VDB's USE/IUSE 3-4 separate times
//! across `Avail::initial_bdepend`/`initial_depend` and
//! `load_target_installed`/`load_host_installed`) share one read instead of
//! each re-reading the same file from disk.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use camino::{Utf8Path, Utf8PathBuf};

type Cache = Mutex<HashMap<Utf8PathBuf, Option<String>>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_cache() -> MutexGuard<'static, HashMap<Utf8PathBuf, Option<String>>> {
    cache().lock().unwrap_or_else(PoisonError::into_inner)
}

/// Return the cached value for `path`, or call `fetch` on a miss and cache its result
///
/// `Ok(None)` means "confirmed absent" and is cached too, so a missing
/// optional field isn't restated on every call.
pub(crate) fn get_or_fetch(
    path: &Utf8Path,
    fetch: impl FnOnce() -> std::io::Result<Option<String>>,
) -> std::io::Result<Option<String>> {
    if let Some(cached) = lock_cache().get(path) {
        return Ok(cached.clone());
    }
    let value = fetch()?;
    lock_cache().insert(path.to_owned(), value.clone());
    Ok(value)
}

/// Drop every cached entry whose path is under `pkg_dir` — a single VDB entry's fields
///
/// This makes a write there visible on the next read. Entry-granularity:
/// every other package's cached fields are untouched.
pub(crate) fn invalidate_entry(pkg_dir: &Utf8Path) {
    lock_cache().retain(|p, _| !p.starts_with(pkg_dir));
}
