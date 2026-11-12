//! Metadata loading for every CPV in a repository, whether or not it ships
//! an in-tree cache.
//!
//! # The chain
//!
//! Resolve needs DEPEND/IUSE/… for every CPV, so:
//!
//! 1. Prefer bulk primary walk entries when **fresh**
//! 2. Else [`Repository::cache_entry`] (primary then secondary) when **fresh**
//! 3. Else, for **symlinks into a master**, reuse the master's primary entry
//! 4. Else **source** the ebuild and [`Repository::put_secondary`]
//!
//! Secondary cache is always configured on the [`Repository`] (builder);
//! this module does not take free-floating cache paths.
//!
//! # One entry point, evidence-gated
//!
//! [`repo_entries`] narrows the digest work to *suspects*: an ebuild with no
//! cache entry, or one newer than the specific cache file serving it (a
//! present-but-stale entry, which the bulk read would otherwise trust
//! blindly). Everything else is taken from the cache as-is. This is what
//! makes the main repo's ~32k-ebuild tree affordable to check on every
//! resolve — "almost every ebuild has a cache entry" is not "every ebuild
//! does": a version with no entry is not merely uncached, it is
//! **invisible** (`RepoData` is built from cache entries alone, so it can
//! never be selected, and its atom reports as "no ebuilds" rather than
//! masked).
//!
//! A repo with no in-tree cache at all (a hand-made local overlay,
//! `crossdev`) is the degenerate case of the same rule: the bulk read finds
//! nothing, so every ebuild is suspect and takes the digest/source chain —
//! this is `overlay_entries`' old always-digest-everything behaviour,
//! falling out of the general rule rather than needing its own code path.
//!
//! Finding suspects costs a tree walk, which is most of a resolve's
//! repo-load time on its own. Two things keep that off the common path: the
//! walk runs concurrently with the bulk read (`Ebuilds` owns its walker, so
//! it moves onto a blocking thread cleanly), and its result is memoised in a
//! sidecar keyed on [`Repository::sync_stamp`] — but only for a repo whose
//! stamp is strong enough to trust ([`Repository::has_sync_marker`]); an
//! unchanged tree with a real `timestamp.chk` (the main repo, and any
//! git/rsync-synced overlay like `guru`) skips the walk entirely, while a
//! `timestamp.chk`-less tree always re-checks.
//!
//! Validation is by **md5 of file contents** (`_md5_` plus the `_eclasses_`
//! digests), never mtime; mtime only selects *which* files to digest. That
//! matches the md5-cache format's own contract.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use portage_atom::Cpv;
use portage_metadata::CacheEntry;

use crate::cache::{
    CacheReadOpts, cache_entries_parallel, cache_entries_parallel_with_mtime,
    secondary_cache_entries_with_mtime,
};
use crate::metadata_cache::MetadataCache;
use crate::repo::Repository;

#[derive(Default)]
struct EclassMemo {
    by_name: HashMap<
        portage_atom::interner::Interned<portage_atom::interner::DefaultInterner>,
        Option<md5::Digest>,
    >,
}

/// Name of the sidecar listing the CPVs the in-tree cache does not serve
const GAP_INDEX: &str = "gap-index";

/// Every metadata entry of `repo`
///
/// The in-tree bulk read, plus the chain over ebuilds that read cannot
/// serve — suspects, symlink-into-a-master reuse, then live sourcing. One
/// function for every repo: a repo with no in-tree cache is this
/// function's degenerate case (see the module doc).
///
/// Finding suspects costs a tree walk, most of a resolve's repo-load time
/// on its own. Two things keep that off the common path: the walk runs
/// concurrently with the bulk read, and — for a repo with a trustworthy
/// sync marker ([`Repository::has_sync_marker`]) — its result is memoised
/// keyed on [`Repository::sync_stamp`], so an unchanged tree reads the
/// handful of entries straight from the secondary store.
pub async fn repo_entries(repo: &Repository) -> Vec<(Cpv, CacheEntry)> {
    let stamp = repo.sync_stamp();
    let index = repo.sidecar_path(GAP_INDEX);
    let cache_opts = CacheReadOpts {
        jobs: Some(crate::default_worker_count()),
        ..CacheReadOpts::default()
    };
    let mut eclass_digests = EclassMemo::default();

    // Unchanged tree: the suspects are known, and their entries are already in
    // the secondary store from the run that discovered them. Gated on
    // `has_sync_marker`: without a rewritten-on-sync timestamp.chk, `stamp`
    // degrades to the repo root directory's mtime, which is invariant under
    // exactly the in-place edits a hand-maintained overlay gets — trusting
    // the memo there would pin it on a stale answer forever. Without an in-tree cache every
    // ebuild is a gap entry, digested one at a time here: the walk below is faster.
    let use_index = repo.has_sync_marker() && repo.primary_cache_dir().is_dir();
    if use_index
        && let (Some(stamp), Some(index)) = (&stamp, &index)
        && let Some(cpvs) = read_gap_index(index, stamp)
    {
        let mut stale = false;
        let (primary_rows, discovery_failed) =
            match cache_entries_parallel(std::slice::from_ref(repo), &cache_opts, |text| {
                CacheEntry::parse(text).map_err(crate::Error::from)
            })
            .await
            {
                Ok(rows) => (rows, false),
                Err(e) => {
                    tracing::error!(
                        "repo '{}': metadata cache discovery failed, rescanning: {e}",
                        repo.name()
                    );
                    (Vec::new(), true)
                }
            };
        let mut out: Vec<(Cpv, CacheEntry)> = primary_rows
            .into_iter()
            .filter_map(|(cpv, e)| {
                let entry = e.ok()?;
                if repo.is_fresh_cached(&entry, &mut eclass_digests.by_name) {
                    Some((cpv, entry))
                } else {
                    stale = true;
                    None
                }
            })
            .collect();
        let mut md5_covered: HashSet<Cpv> = HashSet::new();
        let mut recovered = 0usize;
        // A linear scan over the primary rows only, not an index: the gap list is small when there
        // is an in-tree cache, and `Cpv` equality fails on an interned pointer almost immediately.
        // Without one there are no primary rows, however long the list.
        let primary_rows = out.len();
        let mut drop_primary: Vec<usize> = Vec::new();
        for cpv in &cpvs {
            if let Some(pos) = out[..primary_rows]
                .iter()
                .position(|(candidate, _)| candidate == cpv)
            {
                // Eclass freshness is not enough: a gap line exists because the
                // primary `_md5_` did not match the ebuild. Drop the line only
                // when that file now matches. Otherwise serve secondary.
                if primary_md5_matches_ebuild(repo, cpv, &out[pos].1) {
                    recovered += 1;
                    md5_covered.insert(cpv.clone());
                    continue;
                }
                drop_primary.push(pos);
            }
            if let Some(entry) = secondary_cache_entry(repo, cpv)
                && primary_md5_matches_ebuild(repo, cpv, &entry)
                && repo.is_fresh_cached(&entry, &mut eclass_digests.by_name)
            {
                recovered += 1;
                out.push((cpv.clone(), entry));
            }
        }
        // Descending, so the surviving entries keep the primary cache's order —
        // `repo_entries` output order must not depend on the gap list's.
        drop_primary.sort_unstable_by(|a, b| b.cmp(a));
        drop_primary.dedup();
        for pos in drop_primary {
            out.remove(pos);
        }
        // A pruned secondary would silently re-hide packages; fall through and
        // rebuild rather than resolve against a tree we can only partly see.
        if !discovery_failed && !stale && recovered == cpvs.len() {
            if !md5_covered.is_empty() {
                let remaining: Vec<Cpv> = cpvs
                    .iter()
                    .filter(|cpv| !md5_covered.contains(*cpv))
                    .cloned()
                    .collect();
                write_gap_index(index, stamp, remaining.iter());
            }
            return out;
        }
        if stale {
            tracing::debug!(
                "repo '{}': cached eclass metadata changed, rescanning",
                repo.name()
            );
        } else {
            tracing::debug!(
                "repo '{}': gap index stale ({recovered}/{} entries present), rescanning",
                repo.name(),
                cpvs.len()
            );
        }
        out.clear();
    }

    let scan = repo.ebuilds().ok();
    let walked = scan.is_some();
    let (bulk_res, secondary_res, ebuilds) = tokio::join!(
        cache_entries_parallel_with_mtime(std::slice::from_ref(repo), &cache_opts, |text| {
            CacheEntry::parse(text).map_err(crate::Error::from)
        }),
        // The durable secondary store's own entries — without this, a repo
        // with no in-tree cache (crossdev, pentoo-shaped trees) never has
        // anything in `covered`, so every one of its ebuilds re-enters the
        // full suspect chain on every call even once the secondary already
        // holds a fresh answer for it (previously only consulted one cpv at
        // a time, deep inside that chain).
        secondary_cache_entries_with_mtime(repo, &cache_opts, |text| {
            CacheEntry::parse(text).map_err(crate::Error::from)
        }),
        async {
            match scan {
                Some(s) => match tokio::task::spawn_blocking(move || {
                    s.into_iter()
                        .map(|e| {
                            let modified = std::fs::metadata(e.path().as_std_path())
                                .and_then(|m| m.modified())
                                .ok();
                            (e, modified)
                        })
                        .collect::<Vec<_>>()
                })
                .await
                {
                    Ok(listed) => listed,
                    Err(e) => {
                        tracing::error!(
                            "repo '{}': ebuild mtime walk failed, listing without mtimes: {e}",
                            repo.name()
                        );
                        repo.ebuilds()
                            .map(|iter| iter.into_iter().map(|e| (e, None)).collect())
                            .unwrap_or_default()
                    }
                },
                None => Vec::new(),
            }
        }
    );
    let ebuilds = without_equal_versions(repo, ebuilds);
    let bulk = bulk_res.unwrap_or_else(|e| {
        tracing::error!(
            "repo '{}': primary cache read failed, sourcing ebuilds: {e}",
            repo.name()
        );
        Vec::new()
    });
    let secondary_bulk = secondary_res.unwrap_or_else(|e| {
        tracing::error!(
            "repo '{}': secondary cache read failed, sourcing ebuilds: {e}",
            repo.name()
        );
        Vec::new()
    });

    // em's own store is never pruned: an entry whose ebuild is gone would keep a deleted version
    // installable. The in-tree cache is maintained with the tree.
    let on_disk: Option<HashSet<&Cpv>> =
        walked.then(|| ebuilds.iter().map(|(e, _)| e.cpv()).collect());
    let mut out: Vec<(Cpv, CacheEntry)> = Vec::with_capacity(bulk.len() + secondary_bulk.len());
    // The cache file's own mtime, per cpv — the "cache file serving e.cpv"
    // half of the per-entry suspect rule, gathered in the same pass as the
    // bulk read (no second directory walk).
    let mut cache_mtime: HashMap<Cpv, std::time::SystemTime> =
        HashMap::with_capacity(bulk.len() + secondary_bulk.len());
    let mut covered: std::collections::HashSet<Cpv> =
        std::collections::HashSet::with_capacity(bulk.len() + secondary_bulk.len());
    // Served by the secondary store, so the in-tree cache does not serve them: they belong in the
    // gap index, which is all the fast path reads besides the in-tree cache.
    let mut from_secondary: HashSet<Cpv> = HashSet::new();
    // Primary first, so it wins a cpv present in both — same precedence
    // `Repository::cache_entry` already gives primary over secondary.
    let rows = bulk.into_iter().map(|row| (row, false));
    let secondary_rows = secondary_bulk.into_iter().map(|row| (row, true));
    for ((cpv, mtime, entry), secondary) in rows.chain(secondary_rows) {
        let deleted = secondary && on_disk.as_ref().is_some_and(|d| !d.contains(&cpv));
        if covered.contains(&cpv) || deleted {
            continue;
        }
        if let Some(m) = mtime {
            cache_mtime.insert(cpv.clone(), m);
        }
        if let Ok(entry) = entry
            && repo.is_fresh_cached(&entry, &mut eclass_digests.by_name)
        {
            covered.insert(cpv.clone());
            if secondary {
                from_secondary.insert(cpv.clone());
            }
            out.push((cpv, entry));
        }
    }
    // Per-entry, not repo-wide: an ebuild is suspect only when it is newer
    // than the specific cache file serving it, not merely newer than some
    // repo-wide sync marker (`Repository::sync_time`) — which falls back to
    // the repo root directory's mtime when there is no `timestamp.chk`, a
    // signal that neither tracks a single edited ebuild nor stays put
    // across unrelated top-level changes (a new category directory).
    let suspects: Vec<_> = ebuilds
        .into_iter()
        .filter(|(e, modified)| {
            !covered.contains(e.cpv())
                || match (modified, cache_mtime.get(e.cpv())) {
                    (Some(m), Some(c)) => *m > *c,
                    // No mtime either side: the uncached check above is all we
                    // have, so do not treat every ebuild as suspect.
                    _ => false,
                }
        })
        .map(|(e, _)| e)
        .collect();

    // What the in-tree cache currently says about each suspect, to tell "it
    // served this correctly" from "it could not serve it".
    let suspect_set: std::collections::HashSet<&Cpv> = suspects.iter().map(|e| e.cpv()).collect();
    let before: HashMap<Cpv, Option<String>> = out
        .iter()
        .filter(|(cpv, _)| suspect_set.contains(cpv) && !from_secondary.contains(cpv))
        .map(|(cpv, e)| (cpv.clone(), e.md5.clone()))
        .collect();

    let gap = gap_entries_with_digests(
        repo,
        suspects,
        &std::collections::HashSet::new(),
        &mut eclass_digests,
    )
    .await;

    // Index only what the in-tree cache cannot serve: absent, or serving a
    // different build than the ebuild on disk. A suspect that merely looked new
    // and validated fine stays with the bulk read, so the warm path does not
    // fetch it from the secondary store for nothing. Same `has_sync_marker`
    // gate as the read above — no point writing a sidecar this repo can
    // never safely consult.
    if use_index && let (Some(stamp), Some(index)) = (&stamp, &index) {
        let resourced: HashSet<&Cpv> = gap.iter().map(|(cpv, _)| cpv).collect();
        let unserved_gap = gap.iter().filter(|(cpv, e)| match before.get(cpv) {
            None => true,
            Some(old) => old.as_deref() != e.md5.as_deref(),
        });
        let secondary_only = out
            .iter()
            .filter(|(cpv, _)| from_secondary.contains(cpv) && !resourced.contains(cpv));
        write_gap_index(
            index,
            stamp,
            unserved_gap.chain(secondary_only).map(|(cpv, _)| cpv),
        );
    }
    // A re-sourced entry supersedes whatever the bulk read produced.
    let replaced: std::collections::HashSet<&Cpv> = gap.iter().map(|(cpv, _)| cpv).collect();
    out.retain(|(cpv, _)| !replaced.contains(cpv));
    out.extend(gap);
    out
}

/// The CPVs a previous run found the cache could not serve, if `stamp`
/// still matches the tree
///
/// `None` on any mismatch or read failure — the caller then rescans, so a
/// corrupt or partial sidecar costs time, never correctness.
fn secondary_cache_entry(repo: &Repository, cpv: &Cpv) -> Option<CacheEntry> {
    let pf = format!("{}-{}", cpv.cpn.package, cpv.version);
    let path = repo
        .secondary_cache_dir()?
        .join(cpv.cpn.category.as_str())
        .join(pf);
    let text = std::fs::read_to_string(path.as_std_path()).ok()?;
    CacheEntry::parse(&text).ok()
}

fn primary_md5_matches_ebuild(repo: &Repository, cpv: &Cpv, entry: &CacheEntry) -> bool {
    let Some(recorded) = entry.md5.as_deref() else {
        return false;
    };
    let pf = format!("{}-{}", cpv.cpn.package, cpv.version);
    let path = repo
        .path()
        .join(cpv.cpn.category.as_str())
        .join(cpv.cpn.package.as_str())
        .join(format!("{pf}.ebuild"));
    let Ok(bytes) = std::fs::read(path.as_std_path()) else {
        return false;
    };
    recorded.eq_ignore_ascii_case(&format!("{:x}", md5::compute(&bytes)))
}

fn read_gap_index(path: &camino::Utf8Path, stamp: &str) -> Option<Vec<Cpv>> {
    let text = std::fs::read_to_string(path.as_std_path()).ok()?;
    let mut lines = text.lines();
    if lines.next()? != stamp {
        return None;
    }
    lines.map(|l| Cpv::parse(l).ok()).collect()
}

fn write_gap_index<'a>(path: &camino::Utf8Path, stamp: &str, cpvs: impl Iterator<Item = &'a Cpv>) {
    let mut text = String::from(stamp);
    for cpv in cpvs {
        text.push('\n');
        text.push_str(&cpv.to_string());
    }
    text.push('\n');
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent.as_std_path());
    }
    if let Err(e) = std::fs::write(path.as_std_path(), text) {
        tracing::debug!("could not write gap index {path}: {e}");
    }
}

/// Remove entries now served by the in-tree cache from a valid gap sidecar.
pub(crate) fn reconcile_gap_index(repo: &Repository, covered: &HashSet<Cpv>) {
    if !repo.has_sync_marker() {
        return;
    }
    let Some(stamp) = repo.sync_stamp() else {
        return;
    };
    let Some(path) = repo.sidecar_path(GAP_INDEX) else {
        return;
    };
    let Some(cpvs) = read_gap_index(&path, &stamp) else {
        return;
    };
    let remaining: Vec<Cpv> = cpvs
        .into_iter()
        .filter(|cpv| !covered.contains(cpv))
        .collect();
    write_gap_index(&path, &stamp, remaining.iter());
}

/// The same chain over only the CPVs `covered` does not already contain
///
/// The cache's gap. Every ebuild reaching the chain here is one with no
/// in-tree cache entry, so it would otherwise be invisible rather than
/// merely uncached.
pub async fn gap_entries(
    repo: &Repository,
    ebuilds: Vec<crate::repo::Ebuild>,
    covered: &std::collections::HashSet<Cpv>,
) -> Vec<(Cpv, CacheEntry)> {
    gap_entries_with_digests(repo, ebuilds, covered, &mut EclassMemo::default()).await
}

async fn gap_entries_with_digests(
    repo: &Repository,
    ebuilds: Vec<crate::repo::Ebuild>,
    covered: &std::collections::HashSet<Cpv>,
    digests: &mut EclassMemo,
) -> Vec<(Cpv, CacheEntry)> {
    let missing: Vec<_> = ebuilds
        .into_iter()
        .filter(|e| !covered.contains(e.cpv()))
        .collect();
    if missing.is_empty() {
        return Vec::new();
    }
    tracing::debug!(
        "repo '{}': {} ebuild(s) with no in-tree cache entry, sourcing",
        repo.name(),
        missing.len()
    );
    resolve_ebuilds(repo, Store::Repo, missing, &mut HashMap::new(), digests).await
}

/// Metadata for alias cpvs, each an [`Ebuild::with_cpv`] of the alias cpv onto
/// `source`'s real ebuild, kept in the alias's own `store`
///
/// Sourced under the alias category rather than reusing `source`'s entry:
/// ebuilds branch on `CATEGORY` at global scope (`crossdev.eclass`).
///
/// [`Ebuild::with_cpv`]: crate::repo::Ebuild::with_cpv
pub async fn alias_entries(
    source: &Repository,
    store: &dyn MetadataCache,
    ebuilds: Vec<crate::repo::Ebuild>,
) -> Vec<(Cpv, CacheEntry)> {
    resolve_ebuilds(
        source,
        Store::Only(store),
        ebuilds,
        &mut HashMap::new(),
        &mut EclassMemo::default(),
    )
    .await
}

/// Entries for `cpvs`, which the storage backend holds no valid entry for: the secondary
/// cache's when still valid, otherwise sourced through the tree and written there
#[cfg(feature = "redb")]
pub(crate) async fn backend_gaps(repo: &Repository, cpvs: Vec<Cpv>) -> Vec<(Cpv, CacheEntry)> {
    if cpvs.is_empty() {
        return Vec::new();
    }
    tracing::debug!(
        "repo '{}': {} ebuild(s) with no valid stored entry",
        repo.name(),
        cpvs.len()
    );
    let ebuilds: Vec<_> = cpvs
        .into_iter()
        .map(|cpv| repo.listed_ebuild(cpv))
        .collect();
    resolve_ebuilds(
        repo,
        Store::Only(repo.secondary()),
        ebuilds,
        &mut HashMap::new(),
        &mut EclassMemo::default(),
    )
    .await
}

/// Where [`resolve_ebuilds`] looks up and persists what it sources
#[derive(Clone, Copy)]
enum Store<'a> {
    /// The repo's own primary-then-secondary cache
    Repo,
    /// Only this cache: an alias's own, or the secondary for ebuilds the primary has no valid
    /// entry for
    Only(&'a dyn MetadataCache),
}

/// Fewest items worth a thread of their own
const ITEMS_PER_THREAD: usize = 256;

/// `f` over `items` on scoped threads, in order, each thread starting from its own `init()`
pub(crate) fn par_map<T: Send, S: Send, R: Send>(
    items: Vec<T>,
    init: impl Fn() -> S + Sync,
    f: impl Fn(&mut S, T) -> R + Sync,
) -> (Vec<R>, Vec<S>) {
    let workers = crate::default_worker_count().min(items.len().div_ceil(ITEMS_PER_THREAD));
    let chunk = items.len().div_ceil(workers.max(1)).max(1);
    let mut parts: Vec<Vec<T>> = Vec::with_capacity(workers);
    let mut items = items.into_iter();
    loop {
        let part: Vec<T> = items.by_ref().take(chunk).collect();
        if part.is_empty() {
            break;
        }
        parts.push(part);
    }
    let (init, f) = (&init, &f);
    std::thread::scope(|scope| {
        let handles: Vec<_> = parts
            .into_iter()
            .map(|part| {
                scope.spawn(move || {
                    let mut state = init();
                    let out: Vec<R> = part.into_iter().map(|item| f(&mut state, item)).collect();
                    (out, state)
                })
            })
            .collect();
        let (mut out, mut states) = (Vec::new(), Vec::new());
        for handle in handles {
            match handle.join() {
                Ok((part, state)) => {
                    out.extend(part);
                    states.push(state);
                }
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
        (out, states)
    })
}

/// The entry `result` gives for `ebuild`, written to the store; `None` when sourcing failed
fn finish(
    repo: &Repository,
    store: Store<'_>,
    ebuild: &crate::repo::Ebuild,
    result: crate::Result<crate::source::SourcedEbuild>,
) -> Option<(Cpv, CacheEntry)> {
    let cpv = ebuild.cpv().clone();
    let sourced = match result {
        Ok(sourced) => sourced,
        Err(e) => {
            tracing::error!("repo '{}': failed to source {cpv}: {e}", repo.name());
            return None;
        }
    };
    let eclasses = sourced
        .eclasses
        .iter()
        .map(|(name, digest)| (portage_atom::interner::Interned::intern(name), *digest))
        .collect();
    let entry = CacheEntry {
        metadata: sourced.metadata,
        md5: Some(sourced.ebuild_md5),
        eclasses,
    };
    // serialize() drops a field that failed to parse, so writing the entry would hide the
    // failure for good: skip the ebuild, as build_entry does.
    if entry.metadata.has_parse_failure() {
        tracing::error!(
            "repo '{}': {cpv}: {}, skipping",
            repo.name(),
            entry.metadata.parse_failure_summary()
        );
        return None;
    }
    let put = match store {
        Store::Repo => repo.put_secondary(&cpv, &entry),
        Store::Only(only) => only.put(&cpv, &entry),
    };
    if let Err(e) = put {
        tracing::warn!(
            "repo '{}': failed to write secondary cache for {cpv}: {e}",
            repo.name()
        );
    }
    Some((cpv, entry))
}

/// What [`check`] found for one ebuild
#[allow(clippy::large_enum_variant)] // `Served` is the common case; boxing it allocates per entry
enum Checked {
    /// A valid entry from a cache
    Served(Cpv, CacheEntry),
    /// Nothing valid: source it
    Source(crate::repo::Ebuild),
    /// Unreadable, or not text
    Skip,
}

/// The bulk entry, the store's or a master's, when one is valid for `ebuild`
fn check(
    repo: &Repository,
    store: Store<'_>,
    ebuild: crate::repo::Ebuild,
    bulk: Option<CacheEntry>,
    digests: &mut EclassMemo,
) -> Checked {
    let cpv = ebuild.cpv().clone();
    let Ok(bytes) = ebuild.read_bytes() else {
        return Checked::Skip;
    };
    let digest = format!("{:x}", md5::compute(&bytes));
    let mut valid = |entry: &CacheEntry| {
        entry
            .md5
            .as_deref()
            .is_some_and(|m| m.eq_ignore_ascii_case(&digest))
            && repo.is_fresh_cached(entry, &mut digests.by_name)
            && !entry.metadata.has_parse_failure()
    };

    // Primary bulk walk (in-tree md5-cache).
    if let Some(entry) = bulk
        && valid(&entry)
    {
        return Checked::Served(cpv, entry);
    }

    // Layered lookup: primary miss path already tried above for bulk;
    // this hits secondary (and any primary entry not in the bulk map).
    let stored = match store {
        Store::Repo => repo.cache_entry(&cpv),
        Store::Only(only) => only.get(&cpv),
    };
    if let Ok(Some(entry)) = stored
        && valid(&entry)
    {
        return Checked::Served(cpv, entry);
    }

    // Symlinked ebuild → master's primary cache entry.
    if let Some(entry) = master_cache_entry(&ebuild, repo.masters(), &digest) {
        return Checked::Served(cpv, entry);
    }

    if let Err(error) = std::str::from_utf8(&bytes) {
        tracing::error!(
            "repo '{}': failed to source {cpv}: ebuild is not valid UTF-8: {error}",
            repo.name()
        );
        return Checked::Skip;
    }
    Checked::Source(ebuild)
}

/// Run the four-step chain over `ebuilds`, consuming matching entries out of
/// `cached` (the primary bulk walk) as it goes; what is left is sourced in parallel.
async fn resolve_ebuilds(
    repo: &Repository,
    store: Store<'_>,
    ebuilds: impl IntoIterator<Item = crate::repo::Ebuild>,
    cached: &mut HashMap<Cpv, CacheEntry>,
    digests: &mut EclassMemo,
) -> Vec<(Cpv, CacheEntry)> {
    // The checks are independent per ebuild: spread them over threads, each with its own memo.
    let items: Vec<_> = ebuilds
        .into_iter()
        .map(|ebuild| {
            let bulk = cached.remove(ebuild.cpv());
            (ebuild, bulk)
        })
        .collect();
    let total = items.len();
    let seed = &digests.by_name;
    let (checked, memos) = par_map(
        items,
        || EclassMemo {
            by_name: seed.clone(),
        },
        |memo, (ebuild, bulk)| check(repo, store, ebuild, bulk, memo),
    );
    for memo in memos {
        digests.by_name.extend(memo.by_name);
    }

    // One slot per ebuild, so sourced entries keep their place in the output.
    let mut slots: Vec<Option<(Cpv, CacheEntry)>> = Vec::with_capacity(total);
    let mut to_source: Vec<(usize, crate::repo::Ebuild)> = Vec::new();
    for outcome in checked {
        match outcome {
            Checked::Served(cpv, entry) => slots.push(Some((cpv, entry))),
            Checked::Source(ebuild) => {
                to_source.push((slots.len(), ebuild));
                slots.push(None);
            }
            Checked::Skip => {}
        }
    }

    let sourced = source_all(repo, to_source).await;
    let (finished, _) = par_map(
        sourced,
        || (),
        |(), (slot, ebuild, result)| (slot, finish(repo, store, &ebuild, result)),
    );
    for (slot, found) in finished {
        if let Some((cpv, entry)) = found {
            for (name, digest) in &entry.eclasses {
                digests.by_name.insert(*name, Some(*digest));
            }
            slots[slot] = Some((cpv, entry));
        }
    }
    slots.into_iter().flatten().collect()
}

/// `ebuilds` with one file per version: `foo-1.0` and `foo-1.00` are the same version, a broken
/// package; the first file name is kept
fn without_equal_versions(
    repo: &Repository,
    mut ebuilds: Vec<(crate::repo::Ebuild, Option<std::time::SystemTime>)>,
) -> Vec<(crate::repo::Ebuild, Option<std::time::SystemTime>)> {
    ebuilds.sort_by(|(a, _), (b, _)| {
        (a.cpv().cmp(b.cpv())).then_with(|| a.path().as_str().cmp(b.path().as_str()))
    });
    ebuilds.dedup_by(|(dropped, _), (kept, _)| {
        let equal = dropped.cpv() == kept.cpv();
        if equal {
            tracing::warn!(
                "repo '{}': {} and {} are the same version, ignoring the second",
                repo.name(),
                kept.path_in_repo(),
                dropped.path_in_repo()
            );
        }
        equal
    });
    ebuilds
}

/// Source `ebuilds` on the shared worker pool, each returned with the slot it came with
async fn source_all(
    repo: &Repository,
    ebuilds: Vec<(usize, crate::repo::Ebuild)>,
) -> Vec<(
    usize,
    crate::repo::Ebuild,
    crate::Result<crate::source::SourcedEbuild>,
)> {
    if ebuilds.is_empty() {
        return Vec::new();
    }
    let (slots, ebuilds): (HashMap<Cpv, usize>, Vec<_>) = ebuilds
        .into_iter()
        .map(|(slot, ebuild)| ((ebuild.cpv().clone(), slot), ebuild))
        .unzip();
    let opts = crate::source::SourceOpts {
        jobs: Some(crate::default_worker_count().min(ebuilds.len())),
        dedup: false,
    };
    let results = Arc::new(Mutex::new(Vec::with_capacity(ebuilds.len())));
    let sink = Arc::clone(&results);
    let pool = crate::source::source_parallel_join(
        repo,
        ebuilds,
        &opts,
        &crate::source::SourceContext::new(),
        move |ebuild, result| {
            if let Ok(mut out) = sink.lock() {
                out.push((ebuild, result));
            }
        },
    )
    .await;
    if let Err(e) = pool {
        tracing::error!("repo '{}': source worker pool failed: {e}", repo.name());
    }
    let results = std::mem::take(&mut *results.lock().unwrap_or_else(|e| e.into_inner()));
    results
        .into_iter()
        .filter_map(|(ebuild, result)| Some((*slots.get(ebuild.cpv())?, ebuild, result)))
        .collect()
}

/// The master-repo md5-cache entry for a symlinked ebuild, when the link
/// resolves inside a master and the cache entry matches the file (`_md5_`).
fn master_cache_entry(
    ebuild: &crate::repo::Ebuild,
    masters: &[Repository],
    digest: &str,
) -> Option<CacheEntry> {
    let real = ebuild.path().canonicalize_utf8().ok()?;
    if real == ebuild.path() {
        return None;
    }
    for master in masters {
        let Ok(_) = real.strip_prefix(master.path()) else {
            continue;
        };
        let entry = master.cache_entry(ebuild.cpv()).ok().flatten()?;
        if entry
            .md5
            .as_deref()
            .is_some_and(|m| m.eq_ignore_ascii_case(digest))
        {
            return Some(entry);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;

    #[test]
    fn par_map_keeps_order_across_threads_and_returns_each_state() {
        let items: Vec<usize> = (0..ITEMS_PER_THREAD * 3 + 7).collect();
        let (out, states) = par_map(
            items.clone(),
            || 0usize,
            |seen, i| {
                *seen += 1;
                i * 2
            },
        );
        assert_eq!(out, items.iter().map(|i| i * 2).collect::<Vec<_>>());
        assert!(states.len() > 1, "enough items to spread");
        assert_eq!(states.iter().sum::<usize>(), items.len());
        let (none, states) = par_map(Vec::<usize>::new(), || (), |(), i| i);
        assert!(none.is_empty() && states.is_empty());
    }

    fn make_repo(dir: &tempfile::TempDir) -> Repository {
        std::fs::create_dir_all(dir.path().join("metadata")).unwrap();
        std::fs::write(dir.path().join("metadata").join("layout.conf"), "").unwrap();
        std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
        std::fs::write(dir.path().join("profiles").join("categories"), "sys-apps\n").unwrap();
        Repository::builder()
            .in_memory_cache()
            .open(dir.path())
            .unwrap()
    }

    fn set_mtime(path: impl AsRef<std::path::Path>, t: SystemTime) {
        std::fs::File::open(path).unwrap().set_modified(t).unwrap();
    }

    // Regression for the per-entry suspect rule (replacing a repo-wide
    // `sync_time` comparison): a hand-edited ebuild must be re-sourced even
    // though nothing touched `metadata/timestamp.chk` or the repo root
    // directory — the two things a repo-wide "since" marker actually
    // watches, and which an in-place edit three directories down never
    // moves.
    #[tokio::test]
    async fn a_hand_edited_ebuild_is_re_sourced_even_when_the_repo_root_mtime_does_not_move() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir);

        let pkg_dir = dir.path().join("sys-apps").join("foo");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let ebuild_path = pkg_dir.join("foo-1.0.ebuild");
        let old_text = "EAPI=8\nDESCRIPTION=\"old\"\nSLOT=\"0\"\n";
        std::fs::write(&ebuild_path, old_text).unwrap();
        let old_digest = format!("{:x}", md5::compute(old_text.as_bytes()));

        let cache_dir = dir
            .path()
            .join("metadata")
            .join("md5-cache")
            .join("sys-apps");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let cache_file = cache_dir.join("foo-1.0");
        std::fs::write(
            &cache_file,
            format!("EAPI=8\nDESCRIPTION=old\nSLOT=0\n_md5_={old_digest}\n"),
        )
        .unwrap();

        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        set_mtime(&cache_file, base);
        set_mtime(&ebuild_path, base);

        let entries = repo_entries(&repo).await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1.metadata.description, "old");

        // Hand-edit: content changes and the ebuild's own mtime moves past
        // its cache file's — but `metadata/timestamp.chk` doesn't exist and
        // the repo root directory's mtime never changes, so the old
        // repo-wide rule would have kept serving "old" from the cache.
        std::fs::write(&ebuild_path, "EAPI=8\nDESCRIPTION=\"new\"\nSLOT=\"0\"\n").unwrap();
        set_mtime(&ebuild_path, base + Duration::from_secs(60));

        let entries = repo_entries(&repo).await;
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].1.metadata.description, "new",
            "per-entry suspect rule must catch a hand-edit a repo-wide sync marker misses"
        );
    }

    #[tokio::test]
    async fn a_changed_eclass_invalidates_an_otherwise_covered_cache_entry() {
        let dir = tempfile::tempdir().unwrap();
        let cache_root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("metadata")).unwrap();
        std::fs::write(dir.path().join("metadata/layout.conf"), "").unwrap();
        std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
        std::fs::write(dir.path().join("profiles/categories"), "sys-apps\n").unwrap();
        std::fs::write(dir.path().join("metadata/timestamp.chk"), "1\n").unwrap();
        let repo = Repository::builder()
            .user_cache_root(
                camino::Utf8PathBuf::from_path_buf(cache_root.path().to_owned()).unwrap(),
            )
            .open(dir.path())
            .unwrap();

        let eclass_dir = dir.path().join("eclass");
        std::fs::create_dir_all(&eclass_dir).unwrap();
        let eclass_path = eclass_dir.join("test.eclass");
        let old_eclass = "IUSE=\"old\"\n";
        std::fs::write(&eclass_path, old_eclass).unwrap();
        let eclass_digest = format!("{:x}", md5::compute(old_eclass.as_bytes()));

        let pkg_dir = dir.path().join("sys-apps").join("foo");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let ebuild_path = pkg_dir.join("foo-1.0.ebuild");
        let ebuild = "EAPI=8\ninherit test\nDESCRIPTION=\"base\"\nSLOT=0\n";
        std::fs::write(&ebuild_path, ebuild).unwrap();
        let ebuild_digest = format!("{:x}", md5::compute(ebuild.as_bytes()));

        let cache_dir = dir.path().join("metadata/md5-cache/sys-apps");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let cache_path = cache_dir.join("foo-1.0");
        std::fs::write(
            &cache_path,
            format!(
                "EAPI=8\nDESCRIPTION=base\nIUSE=old\nSLOT=0\n_eclasses_=test\t{eclass_digest}\n_md5_={ebuild_digest}\n"
            ),
        )
        .unwrap();

        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        set_mtime(&ebuild_path, base);
        set_mtime(&cache_path, base + Duration::from_secs(60));

        let first = repo_entries(&repo).await;
        assert_eq!(first[0].1.metadata.description, "base");
        assert!(
            first[0]
                .1
                .metadata
                .iuse
                .iter()
                .any(|flag| flag.name() == "old")
        );

        std::fs::write(&eclass_path, "IUSE=\"new\"\n").unwrap();
        let second = repo_entries(&repo).await;
        assert_eq!(second[0].1.metadata.description, "base");
        assert!(
            second[0]
                .1
                .metadata
                .iuse
                .iter()
                .any(|flag| flag.name() == "new")
        );
    }

    #[tokio::test]
    async fn a_covered_gap_index_entry_is_not_duplicated_or_retained() {
        let dir = tempfile::tempdir().unwrap();
        let cache_root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("metadata")).unwrap();
        std::fs::write(dir.path().join("metadata/layout.conf"), "").unwrap();
        std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
        std::fs::write(dir.path().join("profiles/categories"), "sys-apps\n").unwrap();
        std::fs::write(dir.path().join("metadata/timestamp.chk"), "1\n").unwrap();
        let repo = Repository::builder()
            .user_cache_root(
                camino::Utf8PathBuf::from_path_buf(cache_root.path().to_owned()).unwrap(),
            )
            .open(dir.path())
            .unwrap();

        let pkg_dir = dir.path().join("sys-apps/foo");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let ebuild_path = pkg_dir.join("foo-1.0.ebuild");
        let ebuild = "EAPI=8\nDESCRIPTION=\"base\"\nSLOT=0\n";
        std::fs::write(&ebuild_path, ebuild).unwrap();
        let digest = format!("{:x}", md5::compute(ebuild.as_bytes()));
        let cache_file = dir.path().join("metadata/md5-cache/sys-apps/foo-1.0");
        std::fs::create_dir_all(cache_file.parent().unwrap()).unwrap();
        std::fs::write(
            &cache_file,
            format!("EAPI=8\nDESCRIPTION=base\nSLOT=0\n_md5_={digest}\n"),
        )
        .unwrap();

        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        set_mtime(&ebuild_path, base);
        set_mtime(&cache_file, base + Duration::from_secs(60));
        let stamp = repo.sync_stamp().unwrap();
        let sidecar = repo.sidecar_path(GAP_INDEX).unwrap();
        std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
        std::fs::write(&sidecar, format!("{stamp}\nsys-apps/foo-1.0\n")).unwrap();

        let entries = repo_entries(&repo).await;

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1.metadata.description, "base");
        assert_eq!(
            std::fs::read_to_string(sidecar).unwrap(),
            format!("{stamp}\n")
        );
    }

    // A synced repo with no in-tree cache: the second run used to serve everything from the
    // secondary store and write an empty gap index, so the third trusted it and saw no packages.
    // Such a repo now keeps no gap index; an ebuild deleted from it disappears.
    #[tokio::test]
    async fn a_repo_without_an_in_tree_cache_keeps_its_packages_on_every_run() {
        let dir = tempfile::tempdir().unwrap();
        let cache_root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("metadata")).unwrap();
        std::fs::write(dir.path().join("metadata/layout.conf"), "").unwrap();
        std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
        std::fs::write(dir.path().join("profiles/categories"), "sys-apps\n").unwrap();
        std::fs::write(dir.path().join("metadata/timestamp.chk"), "1\n").unwrap();
        let repo = Repository::builder()
            .user_cache_root(
                camino::Utf8PathBuf::from_path_buf(cache_root.path().to_owned()).unwrap(),
            )
            .open(dir.path())
            .unwrap();
        let pkg_dir = dir.path().join("sys-apps/foo");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(
            pkg_dir.join("foo-1.0.ebuild"),
            "EAPI=8\nDESCRIPTION=\"foo\"\nSLOT=0\n",
        )
        .unwrap();
        std::fs::write(
            pkg_dir.join("foo-2.0.ebuild"),
            "EAPI=8\nDESCRIPTION=\"foo\"\nSLOT=0\n",
        )
        .unwrap();

        for run in 1..=3 {
            assert_eq!(repo_entries(&repo).await.len(), 2, "run {run}");
        }
        assert!(!repo.sidecar_path(GAP_INDEX).unwrap().exists());
        std::fs::remove_file(pkg_dir.join("foo-2.0.ebuild")).unwrap();
        let entries = repo_entries(&repo).await;
        let versions: Vec<String> = entries.iter().map(|(cpv, _)| cpv.to_string()).collect();
        assert_eq!(versions, ["sys-apps/foo-1.0"]);
    }

    #[tokio::test]
    async fn of_ebuilds_with_equal_versions_the_first_file_name_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let cache_root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("metadata")).unwrap();
        std::fs::write(dir.path().join("metadata/layout.conf"), "").unwrap();
        std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
        std::fs::write(dir.path().join("profiles/categories"), "sys-apps\n").unwrap();
        let repo = Repository::builder()
            .user_cache_root(
                camino::Utf8PathBuf::from_path_buf(cache_root.path().to_owned()).unwrap(),
            )
            .open(dir.path())
            .unwrap();
        let pkg_dir = dir.path().join("sys-apps/foo");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        for v in ["1.00", "1.0"] {
            std::fs::write(
                pkg_dir.join(format!("foo-{v}.ebuild")),
                format!("EAPI=8\nDESCRIPTION=\"foo {v}\"\nSLOT=0\n"),
            )
            .unwrap();
        }
        for _ in 0..2 {
            let entries = repo_entries(&repo).await;
            let descriptions: Vec<_> = entries
                .iter()
                .map(|(_, e)| &e.metadata.description)
                .collect();
            assert_eq!(descriptions, ["foo 1.0"]);
        }
    }

    #[tokio::test]
    async fn a_gap_line_is_kept_when_the_primary_md5_does_not_match_the_ebuild() {
        let dir = tempfile::tempdir().unwrap();
        let cache_root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("metadata")).unwrap();
        std::fs::write(dir.path().join("metadata/layout.conf"), "").unwrap();
        std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
        std::fs::write(dir.path().join("profiles/categories"), "sys-apps\n").unwrap();
        std::fs::write(dir.path().join("metadata/timestamp.chk"), "1\n").unwrap();
        let repo = Repository::builder()
            .user_cache_root(
                camino::Utf8PathBuf::from_path_buf(cache_root.path().to_owned()).unwrap(),
            )
            .open(dir.path())
            .unwrap();

        let pkg_dir = dir.path().join("sys-apps/foo");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let ebuild_path = pkg_dir.join("foo-1.0.ebuild");
        let ebuild = "EAPI=8\nDESCRIPTION=\"from-ebuild\"\nSLOT=0\n";
        std::fs::write(&ebuild_path, ebuild).unwrap();
        let cache_file = dir.path().join("metadata/md5-cache/sys-apps/foo-1.0");
        std::fs::create_dir_all(cache_file.parent().unwrap()).unwrap();
        std::fs::write(
            &cache_file,
            "EAPI=8\nDESCRIPTION=stale-primary\nSLOT=0\n_md5_=00000000000000000000000000000000\n",
        )
        .unwrap();

        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        set_mtime(&ebuild_path, base);
        set_mtime(&cache_file, base + Duration::from_secs(60));
        let stamp = repo.sync_stamp().unwrap();
        let sidecar = repo.sidecar_path(GAP_INDEX).unwrap();
        std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
        std::fs::write(&sidecar, format!("{stamp}\nsys-apps/foo-1.0\n")).unwrap();

        let secondary_dir = repo.secondary_cache_dir().unwrap().join("sys-apps");
        std::fs::create_dir_all(&secondary_dir).unwrap();
        let digest = format!("{:x}", md5::compute(ebuild.as_bytes()));
        std::fs::write(
            secondary_dir.join("foo-1.0"),
            format!("EAPI=8\nDESCRIPTION=from-secondary\nSLOT=0\n_md5_={digest}\n"),
        )
        .unwrap();

        let entries = repo_entries(&repo).await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1.metadata.description, "from-secondary");
        let sidecar_text = std::fs::read_to_string(&sidecar).unwrap();
        assert!(
            sidecar_text.contains("sys-apps/foo-1.0"),
            "the gap line must stay while primary _md5_ does not match, got {sidecar_text}"
        );
    }

    // Regression for the `has_sync_marker` gate. Trusting the gap-index
    // memo skips the ebuild-mtime suspect walk entirely and returns
    // straight from the bulk cache read — correct only when the memo's
    // stamp can actually be trusted to track content, which is exactly
    // what a `timestamp.chk`-less repo cannot promise: a hand-edit changes
    // neither the (absent) `timestamp.chk` nor the repo root directory's
    // own mtime, so a forged-but-stamp-matching sidecar could otherwise
    // serve a stale cache entry forever.
    #[tokio::test]
    async fn a_repo_without_a_sync_marker_never_trusts_a_forged_gap_index() {
        let dir = tempfile::tempdir().unwrap();
        let cache_root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("metadata")).unwrap();
        std::fs::write(dir.path().join("metadata").join("layout.conf"), "").unwrap();
        std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
        std::fs::write(dir.path().join("profiles").join("categories"), "sys-apps\n").unwrap();

        let repo = Repository::builder()
            .user_cache_root(
                camino::Utf8PathBuf::from_path_buf(cache_root.path().to_owned()).unwrap(),
            )
            .open(dir.path())
            .unwrap();
        assert!(!repo.has_sync_marker(), "no timestamp.chk written");

        let pkg_dir = dir.path().join("sys-apps").join("foo");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let ebuild_path = pkg_dir.join("foo-1.0.ebuild");
        let old_text = "EAPI=8\nDESCRIPTION=\"old\"\nSLOT=\"0\"\n";
        std::fs::write(&ebuild_path, old_text).unwrap();
        let old_digest = format!("{:x}", md5::compute(old_text.as_bytes()));

        let cache_dir = dir
            .path()
            .join("metadata")
            .join("md5-cache")
            .join("sys-apps");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let cache_file = cache_dir.join("foo-1.0");
        std::fs::write(
            &cache_file,
            format!("EAPI=8\nDESCRIPTION=old\nSLOT=0\n_md5_={old_digest}\n"),
        )
        .unwrap();

        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        set_mtime(&cache_file, base);
        set_mtime(&ebuild_path, base);

        // Forge a "nothing is a gap" sidecar keyed on the repo's *current*
        // sync_stamp — a real one would only ever get written by
        // `has_sync_marker`-gated code, but the forgery stands in for
        // whatever stale state a prior, differently-gated run left behind.
        let stamp = repo.sync_stamp().expect("repo root dir always has a stamp");
        let sidecar = repo.sidecar_path(GAP_INDEX).expect("durable secondary");
        std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
        std::fs::write(&sidecar, format!("{stamp}\n")).unwrap();

        // Hand-edit: content changes and the ebuild's own mtime moves past
        // its cache file's — but nothing touches the (absent) timestamp.chk
        // or the repo root directory's own mtime, so the forged sidecar's
        // stamp still matches.
        std::fs::write(&ebuild_path, "EAPI=8\nDESCRIPTION=\"new\"\nSLOT=\"0\"\n").unwrap();
        set_mtime(&ebuild_path, base + Duration::from_secs(60));
        assert_eq!(repo.sync_stamp().as_deref(), Some(stamp.as_str()));

        let entries = repo_entries(&repo).await;
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].1.metadata.description, "new",
            "trusting the forged sidecar would have skipped the suspect walk and served \"old\""
        );
    }

    // Regression for folding the secondary (durable, cross-invocation)
    // cache into the bulk read: a repo with no in-tree md5-cache at all
    // previously left every ebuild uncovered, re-entering the full suspect
    // chain every call even with a fresh secondary answer. With secondary
    // folded into `covered`, an unedited entry is trusted by mtime alone —
    // the same per-entry rule the primary cache gets, deliberately not
    // re-validated by md5 when mtime alone says nothing changed.
    #[tokio::test]
    async fn a_warm_secondary_only_entry_is_trusted_without_re_validating_its_md5() {
        let dir = tempfile::tempdir().unwrap();
        let cache_root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("metadata")).unwrap();
        std::fs::write(dir.path().join("metadata").join("layout.conf"), "").unwrap();
        std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
        std::fs::write(dir.path().join("profiles").join("categories"), "sys-apps\n").unwrap();

        let repo = Repository::builder()
            .user_cache_root(
                camino::Utf8PathBuf::from_path_buf(cache_root.path().to_owned()).unwrap(),
            )
            .open(dir.path())
            .unwrap();
        assert!(!repo.has_primary_cache(), "no in-tree md5-cache at all");

        let pkg_dir = dir.path().join("sys-apps").join("foo");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let ebuild_path = pkg_dir.join("foo-1.0.ebuild");
        std::fs::write(&ebuild_path, "EAPI=8\nDESCRIPTION=\"real\"\nSLOT=\"0\"\n").unwrap();

        // A secondary entry with a deliberately *wrong* _md5_ (would fail
        // per-item validation if that path ran) but a distinct DESCRIPTION
        // to trace which path actually served it.
        let secondary_dir = repo.secondary_cache_dir().unwrap().join("sys-apps");
        std::fs::create_dir_all(&secondary_dir).unwrap();
        let secondary_file = secondary_dir.join("foo-1.0");
        std::fs::write(
            &secondary_file,
            "EAPI=8\nDESCRIPTION=cached\nSLOT=0\n_md5_=00000000000000000000000000000000\n",
        )
        .unwrap();

        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        // Ebuild mtime at the cache file's own: not suspect.
        set_mtime(&ebuild_path, base);
        set_mtime(&secondary_file, base);

        let entries = repo_entries(&repo).await;
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].1.metadata.description, "cached",
            "an unedited secondary entry must be trusted by mtime, not re-sourced or re-validated"
        );
    }

    // Regression: resolve_ebuilds's "source fresh" arm used to write
    // whatever it sourced straight to the secondary cache with no check.
    // An ebuild whose own DEPEND text is unparseable (valid shell, invalid
    // PMS dependency syntax) must not be cached as if it had no
    // dependencies — that survives a serialize()/reparse round trip
    // (290ce4c's is_empty() gate means the field is just omitted) and
    // would then look like a clean, dependency-free entry forever, with no
    // way to detect the original failure again.
    #[tokio::test]
    async fn an_unparseable_depend_is_skipped_not_cached_dependency_free() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir);

        let pkg_dir = dir.path().join("sys-apps").join("bar");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let ebuild_path = pkg_dir.join("bar-1.0.ebuild");
        std::fs::write(
            &ebuild_path,
            "EAPI=8\nDESCRIPTION=\"bar\"\nSLOT=\"0\"\nDEPEND=\"( unterminated\"\n",
        )
        .unwrap();

        let entries = repo_entries(&repo).await;
        assert!(
            entries.is_empty(),
            "an ebuild whose own DEPEND fails to parse must be skipped, not silently \
             cached as dependency-free: {entries:?}"
        );

        let cpv = portage_atom::Cpv::parse("sys-apps/bar-1.0").unwrap();
        assert!(
            repo.cache_entry(&cpv).unwrap().is_none(),
            "must not durably write a parse-failed entry to the secondary cache"
        );
    }
}
