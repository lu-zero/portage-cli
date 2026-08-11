//! Metadata cache operations — regeneration and (future) bulk reading.
//!
//! [`regen_cache`] sources all ebuilds and writes `md5-cache` files, sending
//! one [`RegenItem`] per finished ebuild on a caller-supplied channel.
//!
//! The sourcing concern (running bash, extracting metadata) lives in
//! [`crate::source`]; this module owns the disk I/O side.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use camino::{Utf8Path, Utf8PathBuf};
use portage_metadata::CacheEntry;

use crate::metadata_cache::{DirMetadataCache, MetadataCache};
use crate::source::{SourceContext, SourceOpts, SourcedEbuild};
use crate::{Ebuild, Repository, Result};

/// Shared eclass file → md5 cache used across all regen workers.
///
/// `papaya::HashMap` gives lock-free reads; the first-miss race where two
/// workers concurrently read and hash the same eclass is benign because
/// `insert` is atomic and the digests are identical.
type ChecksumCache = Arc<papaya::HashMap<PathBuf, md5::Digest>>;

/// Where [`regen_cache`] writes sourced metadata.
#[derive(Debug, Clone, Default)]
pub enum RegenWriteTarget {
    /// Source only; do not write cache files.
    #[default]
    None,
    /// Prefer the repository's primary (in-tree) store, else its secondary
    /// (user cache). Same policy as [`Repository::write_cache_entry`].
    Repository,
    /// Force writes into this directory (PMS entry layout).
    Dir(PathBuf),
}

/// Options for [`regen_cache`].
#[derive(Debug, Clone, Default)]
pub struct RegenOpts {
    /// Ebuild sourcing options passed to [`crate::source::source_parallel`].
    pub source: SourceOpts,
    /// Where to persist sourced metadata.
    pub write: RegenWriteTarget,
}

/// Result counters returned by [`regen_cache`].
#[derive(Debug, Clone, Default)]
pub struct RegenStats {
    /// Number of ebuilds processed.
    pub total: usize,
    /// Number of ebuilds that failed to source or write.
    pub errors: usize,
}

/// One finished regen attempt (source + optional cache write), completion order.
///
/// This library does **not** print or own a UI channel — the application
/// maps items onto its activity bus / terminal.
#[derive(Debug)]
pub struct RegenItem {
    /// The ebuild that was processed.
    pub ebuild: Ebuild,
    /// 1-based completion ordinal (not submission order).
    pub index: usize,
    /// Total ebuilds submitted for this run.
    pub total: usize,
    /// `Ok(())` on success; structured [`crate::Error`] on source or write failure
    /// (including [`crate::Error::CacheWrite`]).
    pub result: std::result::Result<(), crate::Error>,
}

/// Source all `ebuilds` and optionally write `md5-cache` files.
///
/// Finished items are sent on `out` in completion order. The worker pool is
/// driven on the **caller's** task (feed + join) — the same scheduling shape
/// as the pre-stream callback API. Spawn a concurrent consumer of a clone of
/// the paired [`flume::Receiver`] *before* awaiting if you want progressive
/// UI; this future owns `out` and drops it when the pool finishes, which
/// closes the receiver.
///
/// # Example
///
/// ```ignore
/// let (tx, rx) = flume::unbounded();
/// let ui = tokio::spawn(async move {
///     while let Ok(item) = rx.recv_async().await {
///         /* progress / errors */
///     }
/// });
/// let stats = regen_cache(repo, ebuilds, &opts, tx).await?;
/// ui.await?;
/// ```
pub async fn regen_cache(
    repo: &Repository,
    ebuilds: Vec<Ebuild>,
    opts: &RegenOpts,
    out: flume::Sender<RegenItem>,
) -> Result<RegenStats> {
    let total = ebuilds.len();
    let write = opts.write.clone();

    // A Dir target's writes go into a fresh staging directory, swapped into
    // place only once every entry is written — see `swap_dir_target`'s doc
    // comment for why. `write` gets pointed at the staging path so the
    // write closure below needs no further changes for that case;
    // `dir_swap` remembers the real target for the swap at the end.
    //
    // The default Repository target gets the exact same treatment against
    // whichever of primary/secondary is actually writable — it would
    // otherwise be the *common* case left paying the full replace-penalty
    // (an already-synced tree's in-tree cache is never empty), while only
    // the less-common explicit `-o DIR` runs got faster. `stage_dir_target`
    // against primary's directory doubles as the writability check: primary
    // being read-only (an unprivileged `em regen` against the system tree,
    // exactly the case `write_cache_entry`'s own per-entry fallback exists
    // for) fails to even create the staging dir, so this falls back to
    // staging secondary instead. No staging at all (falls through to the
    // existing per-entry `write_cache_entry`) only when secondary isn't a
    // durable on-disk store either — in-memory secondary, tests only.
    let (write, dir_swap) = match write {
        RegenWriteTarget::Dir(ref dir) => {
            let cats = ebuilds.iter().map(Ebuild::category);
            let staging = stage_dir_target(dir, cats)?;
            (
                RegenWriteTarget::Dir(staging.clone().into_std_path_buf()),
                Some((dir.clone(), staging)),
            )
        }
        RegenWriteTarget::Repository => {
            let primary_dir = repo.primary_cache_dir();
            let staged_primary = stage_dir_target(
                primary_dir.as_std_path(),
                ebuilds.iter().map(Ebuild::category),
            );
            match staged_primary {
                Ok(staging) => (
                    RegenWriteTarget::Dir(staging.clone().into_std_path_buf()),
                    Some((primary_dir.into_std_path_buf(), staging)),
                ),
                Err(_) => match repo.secondary_cache_dir() {
                    Some(secondary_dir) => {
                        let staging = stage_dir_target(
                            secondary_dir.as_std_path(),
                            ebuilds.iter().map(Ebuild::category),
                        )?;
                        (
                            RegenWriteTarget::Dir(staging.clone().into_std_path_buf()),
                            Some((secondary_dir.to_path_buf().into_std_path_buf(), staging)),
                        )
                    }
                    None => (RegenWriteTarget::Repository, None),
                },
            }
        }
        other => (other, None),
    };

    let ctx = SourceContext::new();
    let checksum_cache: ChecksumCache = Arc::new(papaya::HashMap::new());
    let done = Arc::new(AtomicUsize::new(0));
    let errors = Arc::new(AtomicUsize::new(0));
    let errors_cb = Arc::clone(&errors);
    let repo_for_write = repo.clone();

    crate::source::source_parallel_join(
        repo,
        ebuilds,
        &opts.source,
        &ctx,
        move |ebuild, result| {
            let item_result = match result {
                Err(e) => Err(e),
                Ok(sourced) => {
                    let write_err = match &write {
                        RegenWriteTarget::None => None,
                        RegenWriteTarget::Dir(dir) => {
                            write_entry_to_dir(&ebuild, sourced, dir, &checksum_cache).err()
                        }
                        RegenWriteTarget::Repository => {
                            build_entry(&ebuild, sourced, &checksum_cache)
                                .and_then(|entry| {
                                    repo_for_write
                                        .write_cache_entry(ebuild.cpv(), &entry)
                                        .map_err(|e| e.to_string())
                                })
                                .err()
                        }
                    };
                    match write_err {
                        Some(e) => Err(crate::Error::CacheWrite(e)),
                        None => Ok(()),
                    }
                }
            };
            if item_result.is_err() {
                errors_cb.fetch_add(1, Ordering::Relaxed);
            }
            let index = done.fetch_add(1, Ordering::Relaxed) + 1;
            // Sync send: never park a worker on the UI hand-off. Pair with an
            // unbounded (or large) receiver so a slow consumer cannot stall
            // the pool via channel capacity.
            let _ = out.send(RegenItem {
                ebuild,
                index,
                total,
                result: item_result,
            });
        },
    )
    .await?;

    if let Some((dir, staging)) = dir_swap {
        swap_dir_target(&dir, &staging)?;
    }

    Ok(RegenStats {
        total,
        errors: errors.load(Ordering::Relaxed),
    })
}

fn utf8_or_err(dir: &Path) -> Result<&Utf8Path> {
    Utf8Path::from_path(dir).ok_or_else(|| crate::Error::Io {
        path: dir.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "output dir is not valid UTF-8",
        ),
    })
}

fn io_err(path: &Utf8Path, source: std::io::Error) -> crate::Error {
    crate::Error::Io {
        path: path.to_path_buf().into_std_path_buf(),
        source,
    }
}

/// `<dir>` renamed with a `.regen-staging` suffix — where a `Dir` regen
/// target's writes actually land, before [`swap_dir_target`] moves them
/// into place.
fn staging_path_for(dir: &Utf8Path) -> Utf8PathBuf {
    let file_name = dir.file_name().unwrap_or("regen-output");
    dir.with_file_name(format!("{file_name}.regen-staging"))
}

/// `<dir>` renamed with a `.regen-old` suffix — where [`swap_dir_target`]
/// displaces `dir`'s previous content to during the swap, just long enough
/// to remove it.
fn displaced_path_for(dir: &Utf8Path) -> Utf8PathBuf {
    let file_name = dir.file_name().unwrap_or("regen-output");
    dir.with_file_name(format!("{file_name}.regen-old"))
}

/// Set up a fresh staging directory for a `Dir` regen target, with
/// `categories`' subdirectories pre-created, ready for the write workers to
/// populate. Every write lands here — a plain create, never a replace, no
/// matter how populated `dir` itself already is. Call [`swap_dir_target`]
/// once every write has completed to move it into place.
fn stage_dir_target<'a>(
    dir: &Path,
    categories: impl Iterator<Item = &'a str>,
) -> Result<Utf8PathBuf> {
    let staging = staging_path_for(utf8_or_err(dir)?);
    // Leftover from a previous crashed/interrupted run — never swapped
    // into `dir`, so nothing of value lives here; clear before reuse.
    if staging.is_dir() {
        fs::remove_dir_all(&staging).map_err(|e| io_err(&staging, e))?;
    }
    let cats: HashSet<&str> = categories.collect();
    for cat in cats {
        let p = staging.join(cat);
        fs::create_dir_all(&p).map_err(|e| io_err(&p, e))?;
    }
    Ok(staging)
}

/// Atomically replace `dir` with the fully-populated `staging` directory
/// [`stage_dir_target`] prepared.
///
/// `rename()` can't replace a non-empty directory directly (Linux/POSIX:
/// `ENOTEMPTY`), so this is a rename-away / rename-in / remove-old dance,
/// not one syscall — a brief window where `dir` doesn't exist between the
/// first two renames. But at every *other* instant — including for the
/// entire multi-second write phase before this ever runs — `dir` holds
/// either the complete old content or the complete new content, never a
/// partial mix. That's the actual point versus clearing `dir` in place
/// before writing (this optimization's first attempt, in the same commit
/// history): `remove_dir_all` up front left `dir` empty/partial for the
/// whole write phase, so a crash mid-run discarded the previous cache
/// entirely instead of just leaving it stale.
fn swap_dir_target(dir: &Path, staging: &Utf8Path) -> Result<()> {
    let dir = utf8_or_err(dir)?;
    let displaced = displaced_path_for(dir);
    // Leftover from a previous crashed swap (rename-in or remove-old never
    // completed) — clear before reuse.
    if displaced.is_dir() {
        fs::remove_dir_all(&displaced).map_err(|e| io_err(&displaced, e))?;
    }
    if dir.is_dir() {
        fs::rename(dir, &displaced).map_err(|e| io_err(dir, e))?;
    }
    fs::rename(staging, dir).map_err(|e| io_err(staging, e))?;
    if displaced.is_dir() {
        fs::remove_dir_all(&displaced).map_err(|e| io_err(&displaced, e))?;
    }
    Ok(())
}

fn eclass_md5(path: &Utf8Path, cache: &ChecksumCache) -> std::result::Result<md5::Digest, String> {
    let pinned = cache.pin();
    if let Some(&d) = pinned.get(path.as_std_path()) {
        return Ok(d);
    }
    let data = fs::read(path).map_err(|e| format!("read {path}: {e}"))?;
    let digest = md5::compute(&data);
    pinned.insert(path.to_path_buf().into_std_path_buf(), digest);
    Ok(digest)
}

fn build_entry(
    ebuild: &Ebuild,
    sourced: SourcedEbuild,
    checksum_cache: &ChecksumCache,
) -> std::result::Result<CacheEntry, String> {
    let ebuild_bytes = fs::read(ebuild.path()).map_err(|e| format!("read ebuild: {e}"))?;
    let ebuild_md5 = format!("{:x}", md5::compute(&ebuild_bytes));

    // Md5 every eclass that was actually sourced, using its resolved path.
    // This is path-accurate across master repos — a name-only lookup would
    // miss eclasses inherited from a master overlay's eclass/ directory.
    let SourcedEbuild { metadata, eclasses } = sourced;
    let eclasses: Vec<(String, String)> = eclasses
        .into_iter()
        .map(|(name, path)| {
            let digest =
                eclass_md5(&path, checksum_cache).map_err(|e| format!("eclass {name}: {e}"))?;
            Ok((name, format!("{digest:x}")))
        })
        .collect::<std::result::Result<_, String>>()?;

    Ok(CacheEntry {
        metadata,
        md5: Some(ebuild_md5),
        eclasses,
    })
}

fn write_entry_to_dir(
    ebuild: &Ebuild,
    sourced: SourcedEbuild,
    out_dir: &Path,
    checksum_cache: &ChecksumCache,
) -> std::result::Result<(), String> {
    let entry = build_entry(ebuild, sourced, checksum_cache)?;
    let root = Utf8Path::from_path(out_dir)
        .ok_or_else(|| format!("output dir is not valid UTF-8: {}", out_dir.display()))?;
    DirMetadataCache::new(root.to_owned())
        .put(ebuild.cpv(), &entry)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Options for [`cache_entries_parallel`].
#[derive(Debug, Clone, Default)]
pub struct CacheReadOpts {
    /// Number of parallel workers. `None` uses [`std::thread::available_parallelism`].
    pub jobs: Option<usize>,
    /// When `true`, only the highest-cpv entry per Cpn (across all repos) is
    /// parsed; older versions and duplicates from overlays are skipped before
    /// any file is read. Use this when only the latest version matters
    /// (e.g. description search) — avoids both the wasted parse work *and*
    /// the drop spike from discarding parsed-but-deduped entries.
    pub latest_per_cpn: bool,
}

/// List every `(Cpv, file path)` pair found under each repo's
/// `metadata/md5-cache/` directory.
///
/// Walks each repo's cache with [`jwalk`] (min_depth=2 / max_depth=2 —
/// exactly the category/file leaves), parses the filename into a Cpv,
/// and returns the collected pairs. No file content is read.
///
/// Useful as a name-only enumeration of every cached package across one
/// or more repos. Unlike walking `profiles/categories`, this finds
/// dynamically-created categories (e.g. crossdev's
/// `cross-<TARGET>/`).
///
/// Files whose name does not parse as a Cpv are skipped silently. A cpv
/// can appear more than once if the same package is present in multiple
/// repos — pass [`CacheReadOpts::latest_per_cpn`] to keep only the
/// highest-version entry per Cpn (across all repos).
pub fn cache_cpvs(repos: &[Repository], opts: &CacheReadOpts) -> Vec<(portage_atom::Cpv, PathBuf)> {
    let mut items: Vec<(portage_atom::Cpv, PathBuf)> = Vec::with_capacity(32_768);
    for repo in repos {
        let cache_dir = repo.cache_dir();
        let walker = jwalk::WalkDir::new(cache_dir.as_std_path())
            .skip_hidden(true)
            .min_depth(2)
            .max_depth(2);
        for entry in walker {
            let Ok(entry) = entry else { continue };
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let Some(stem) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let Some(cat) = path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str())
            else {
                continue;
            };
            let mut cpv_str = String::with_capacity(cat.len() + 1 + stem.len());
            cpv_str.push_str(cat);
            cpv_str.push('/');
            cpv_str.push_str(stem);
            let Ok(cpv) = portage_atom::Cpv::parse(&cpv_str) else {
                continue;
            };
            items.push((cpv, path));
        }
    }

    if opts.latest_per_cpn && !items.is_empty() {
        use std::collections::HashMap;
        let mut best: HashMap<portage_atom::Cpn, (portage_atom::Cpv, PathBuf)> =
            HashMap::with_capacity(items.len());
        for (cpv, path) in items.drain(..) {
            best.entry(cpv.cpn)
                .and_modify(|(prev_cpv, prev_path)| {
                    if cpv.version > prev_cpv.version {
                        *prev_cpv = cpv.clone();
                        *prev_path = path.clone();
                    }
                })
                .or_insert((cpv, path));
        }
        items = best.into_values().collect();
    }
    items
}

/// Read every `md5-cache` entry across `repos` in parallel, applying
/// `decode` to each file's text on the worker that reads it.
///
/// Two-phase: (1) a single jwalk pass collects `(Cpv, path)` for every
/// well-named cache file; (2) the slice is chunked across `jobs` blocking
/// tasks that each do `fs::read` + `decode(&text)` end-to-end, then the
/// per-task vectors are concatenated. No channel, no shared mutex.
///
/// `decode` runs on a [`tokio::task::spawn_blocking`] thread and must be
/// `Send + Sync + Clone + 'static`. Pass [`CacheEntry::parse`] (via a
/// thin closure) for the full atom-tree parse, or build a
/// [`portage_metadata::RawCacheEntry`] inside the closure to extract just
/// the fields you need (e.g. `DESCRIPTION` for a search hit) without
/// paying for atom-tree allocations.
///
/// Files whose name does not parse as a Cpv are skipped silently. I/O
/// errors and any error returned by `decode` come through as `Err`
/// items. A cpv can appear more than once if the same package is present
/// in multiple repos; the caller decides how to dedupe (or set
/// [`CacheReadOpts::latest_per_cpn`] to dedupe before any file is read).
pub async fn cache_entries_parallel<T, F>(
    repos: &[Repository],
    opts: &CacheReadOpts,
    decode: F,
) -> Vec<(portage_atom::Cpv, Result<T>)>
where
    T: Send + 'static,
    F: Fn(&str) -> Result<T> + Send + Sync + Clone + 'static,
{
    let jobs = opts.jobs.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    });

    // Phase 1 — discover (and optionally pre-dedupe) every cache file.
    // For ~30k entries that work is ~50-100ms — small enough to keep
    // serial so we can chunk evenly in phase 2.
    let items = cache_cpvs(repos, opts);
    if items.is_empty() {
        return Vec::new();
    }

    // Phase 2 — fan items out into `jobs` chunks, one blocking task each
    // does fs::read + parse for its slice end-to-end, accumulating into a
    // local Vec. Concat at the end. Avoids shared-mutex contention that
    // would otherwise dominate on many-core boxes.
    let total = items.len();
    let chunk_size = total.div_ceil(jobs);
    let mut handles = Vec::with_capacity(jobs);
    for chunk in items.chunks(chunk_size) {
        let chunk: Vec<(portage_atom::Cpv, PathBuf)> = chunk.to_vec();
        let decode = decode.clone();
        handles.push(tokio::task::spawn_blocking(move || {
            let mut out: Vec<(portage_atom::Cpv, Result<T>)> = Vec::with_capacity(chunk.len());
            for (cpv, path) in chunk {
                let result = match fs::read_to_string(&path) {
                    Ok(text) => decode(&text),
                    Err(e) => Err(crate::Error::Io {
                        path: path.clone(),
                        source: e,
                    }),
                };
                out.push((cpv, result));
            }
            out
        }));
    }

    let mut all = Vec::with_capacity(total);
    for h in handles {
        if let Ok(v) = h.await {
            all.extend(v);
        }
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf8_root(dir: &tempfile::TempDir, rel: &str) -> Utf8PathBuf {
        Utf8Path::from_path(dir.path()).unwrap().join(rel)
    }

    #[test]
    fn stage_dir_target_creates_category_dirs_in_a_sibling_staging_dir() {
        let dir = tempfile::tempdir().unwrap();
        let root = utf8_root(&dir, "out");

        let staging =
            stage_dir_target(root.as_std_path(), ["dev-libs", "sys-apps"].into_iter()).unwrap();

        assert_eq!(staging, utf8_root(&dir, "out.regen-staging"));
        assert!(staging.join("dev-libs").is_dir());
        assert!(staging.join("sys-apps").is_dir());
        assert!(
            !root.exists(),
            "swap hasn't happened yet, dir must be untouched"
        );
    }

    #[test]
    fn stage_dir_target_clears_a_leftover_staging_dir_from_a_prior_crash() {
        let dir = tempfile::tempdir().unwrap();
        let root = utf8_root(&dir, "out");
        let stale_staging = utf8_root(&dir, "out.regen-staging");
        std::fs::create_dir_all(stale_staging.join("old-cat")).unwrap();
        std::fs::write(stale_staging.join("old-cat/stale-entry"), "stale").unwrap();

        let staging = stage_dir_target(root.as_std_path(), ["dev-libs"].into_iter()).unwrap();

        assert!(!staging.join("old-cat").exists());
        assert!(staging.join("dev-libs").is_dir());
    }

    #[test]
    fn swap_dir_target_replaces_existing_content_with_the_staging_dir() {
        let dir = tempfile::tempdir().unwrap();
        let root = utf8_root(&dir, "out");
        std::fs::create_dir_all(root.join("old-cat")).unwrap();
        std::fs::write(root.join("old-cat/stale-entry"), "stale").unwrap();

        let staging = stage_dir_target(root.as_std_path(), ["dev-libs"].into_iter()).unwrap();
        std::fs::write(staging.join("dev-libs/new-entry"), "fresh").unwrap();
        swap_dir_target(root.as_std_path(), &staging).unwrap();

        assert!(!root.join("old-cat").exists(), "stale content must be gone");
        assert!(root.join("dev-libs/new-entry").exists());
        assert!(
            !staging.exists(),
            "staging dir must be consumed by the swap"
        );
    }

    #[test]
    fn swap_dir_target_works_when_the_target_never_existed() {
        let dir = tempfile::tempdir().unwrap();
        let root = utf8_root(&dir, "does-not-exist-yet");

        let staging = stage_dir_target(root.as_std_path(), ["dev-libs"].into_iter()).unwrap();
        swap_dir_target(root.as_std_path(), &staging).unwrap();

        assert!(root.join("dev-libs").is_dir());
    }

    #[test]
    fn swap_dir_target_clears_a_leftover_displaced_dir_from_a_prior_crashed_swap() {
        let dir = tempfile::tempdir().unwrap();
        let root = utf8_root(&dir, "out");
        std::fs::create_dir_all(&root).unwrap();
        let stale_displaced = utf8_root(&dir, "out.regen-old");
        std::fs::create_dir_all(stale_displaced.join("ancient-cat")).unwrap();

        let staging = stage_dir_target(root.as_std_path(), ["dev-libs"].into_iter()).unwrap();
        swap_dir_target(root.as_std_path(), &staging).unwrap();

        assert!(!stale_displaced.exists());
        assert!(root.join("dev-libs").is_dir());
    }
}
