//! Metadata loading for ebuilds the in-tree cache does not cover.
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
//! # Two entry points, because the main repo cannot afford the full walk
//!
//! [`overlay_entries`] runs the chain over **every** ebuild. Overlays
//! (crossdev, local) often ship no md5-cache at all, and they are small.
//!
//! The main repo ships a cache covering almost everything, and digesting all
//! ~32k ebuilds to check it is far too expensive per resolve. But "almost" is
//! not "all": ~60 ::gentoo ebuilds have no entry at any given time, and a
//! version with no entry is not merely uncached, it is **invisible** —
//! `RepoData` is built from cache entries alone, so it can never be selected
//! and its atom reports as "no ebuilds" rather than masked.
//!
//! [`primary_entries`] narrows the digest work to *suspects*: an ebuild with no
//! entry, or one newer than the last sync (a present-but-stale entry, which the
//! bulk read would otherwise trust). Everything else is taken from the cache
//! as-is. Finding suspects still costs a tree walk, so that runs concurrently
//! with the bulk read and its outcome is memoised in a sidecar keyed on
//! [`Repository::sync_stamp`] — an unchanged tree skips the walk entirely.
//!
//! Validation is by **md5 of file contents** (`_md5_` plus the `_eclasses_`
//! digests), never mtime; mtime only selects *which* files to digest. That
//! matches the md5-cache format's own contract.

use std::collections::HashMap;

use portage_atom::Cpv;
use portage_metadata::CacheEntry;

use crate::cache::{CacheReadOpts, cache_entries_parallel};
use crate::repo::Repository;

/// Every metadata entry of `repo`: primary bulk walk, layered cache lookup,
/// master-symlink shortcut, then live source into secondary.
pub async fn overlay_entries(repo: &Repository) -> Vec<(Cpv, CacheEntry)> {
    let mut cached: HashMap<Cpv, CacheEntry> = cache_entries_parallel(
        std::slice::from_ref(repo),
        &CacheReadOpts::default(),
        |text| CacheEntry::parse(text).map_err(crate::Error::from),
    )
    .await
    .into_iter()
    .filter_map(|(cpv, e)| e.ok().map(|e| (cpv, e)))
    .collect();

    let Ok(ebuilds) = repo.ebuilds() else {
        return cached.into_iter().collect();
    };

    resolve_ebuilds(repo, ebuilds, &mut cached).await
}

/// Name of the sidecar listing the CPVs the in-tree cache does not serve.
const GAP_INDEX: &str = "gap-index";

/// Every metadata entry of a cache-backed repo: the in-tree bulk read, plus the
/// chain over the ebuilds that read cannot serve.
///
/// An ebuild is *suspect* when the cache has no entry for it, or when it is
/// newer than the last sync — the second catches an entry that is present but
/// stale, which the bulk read would otherwise trust blindly. Only suspects are
/// digested; the other ~32k are taken from the cache as-is.
///
/// Finding suspects costs a tree walk, which is most of a resolve's repo-load
/// time on its own. Two things keep that off the common path: the walk runs
/// concurrently with the bulk read (`Ebuilds` owns its walker, so it moves onto
/// a blocking thread cleanly), and its result is memoised in a sidecar keyed on
/// [`Repository::sync_stamp`], so an unchanged tree skips the walk entirely and
/// reads the handful of entries straight from the secondary store.
pub async fn primary_entries(repo: &Repository) -> Vec<(Cpv, CacheEntry)> {
    let stamp = repo.sync_stamp();
    let index = repo.sidecar_path(GAP_INDEX);

    // Unchanged tree: the suspects are known, and their entries are already in
    // the secondary store from the run that discovered them.
    if let (Some(stamp), Some(index)) = (&stamp, &index)
        && let Some(cpvs) = read_gap_index(index, stamp)
    {
        let opts = CacheReadOpts::default();
        let mut out: Vec<(Cpv, CacheEntry)> =
            cache_entries_parallel(std::slice::from_ref(repo), &opts, |text| {
                CacheEntry::parse(text).map_err(crate::Error::from)
            })
            .await
            .into_iter()
            .filter_map(|(cpv, e)| e.ok().map(|e| (cpv, e)))
            .collect();
        let mut recovered = 0usize;
        for cpv in &cpvs {
            if let Ok(Some(entry)) = repo.cache_entry(cpv) {
                recovered += 1;
                out.push((cpv.clone(), entry));
            }
        }
        // A pruned secondary would silently re-hide packages; fall through and
        // rebuild rather than resolve against a tree we can only partly see.
        if recovered == cpvs.len() {
            return out;
        }
        tracing::debug!(
            "repo '{}': gap index stale ({recovered}/{} entries present), rescanning",
            repo.name(),
            cpvs.len()
        );
        out.clear();
    }

    let scan = repo.ebuilds().ok();
    let opts = CacheReadOpts::default();
    let (bulk, ebuilds) = tokio::join!(
        cache_entries_parallel(std::slice::from_ref(repo), &opts, |text| {
            CacheEntry::parse(text).map_err(crate::Error::from)
        }),
        async {
            match scan {
                Some(s) => tokio::task::spawn_blocking(move || {
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
                .unwrap_or_default(),
                None => Vec::new(),
            }
        }
    );

    let mut out: Vec<(Cpv, CacheEntry)> = bulk
        .into_iter()
        .filter_map(|(cpv, e)| e.ok().map(|e| (cpv, e)))
        .collect();
    let covered: std::collections::HashSet<Cpv> = out.iter().map(|(cpv, _)| cpv.clone()).collect();
    let since = repo.sync_time();
    let suspects: Vec<_> = ebuilds
        .into_iter()
        .filter(|(e, modified)| {
            !covered.contains(e.cpv())
                || match (modified, since) {
                    (Some(m), Some(s)) => *m > s,
                    // No mtime either side: the uncached check above is all we
                    // have, so do not treat every ebuild as suspect.
                    _ => false,
                }
        })
        .map(|(e, _)| e)
        .collect();

    // What the bulk read currently says about each suspect, to tell "the cache
    // served this correctly" from "the cache could not serve it".
    let suspect_set: std::collections::HashSet<&Cpv> = suspects.iter().map(|e| e.cpv()).collect();
    let before: HashMap<Cpv, Option<String>> = out
        .iter()
        .filter(|(cpv, _)| suspect_set.contains(cpv))
        .map(|(cpv, e)| (cpv.clone(), e.md5.clone()))
        .collect();

    let gap = gap_entries(repo, suspects, &std::collections::HashSet::new()).await;

    // Index only what the in-tree cache cannot serve: absent, or serving a
    // different build than the ebuild on disk. A suspect that merely looked new
    // and validated fine stays with the bulk read, so the warm path does not
    // fetch it from the secondary store for nothing.
    if let (Some(stamp), Some(index)) = (&stamp, &index) {
        let unserved = gap
            .iter()
            .filter(|(cpv, e)| match before.get(cpv) {
                None => true,
                Some(old) => old.as_deref() != e.md5.as_deref(),
            })
            .map(|(cpv, _)| cpv);
        write_gap_index(index, stamp, unserved);
    }
    // A re-sourced entry supersedes whatever the bulk read produced.
    let replaced: std::collections::HashSet<&Cpv> = gap.iter().map(|(cpv, _)| cpv).collect();
    out.retain(|(cpv, _)| !replaced.contains(cpv));
    out.extend(gap);
    out
}

/// The CPVs a previous run found the cache could not serve, when `stamp` still
/// matches the tree. `None` on any mismatch or read failure — the caller then
/// rescans, so a corrupt or partial sidecar costs time, never correctness.
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

/// The same chain over only the CPVs `covered` does not already contain — the
/// cache's gap. Every ebuild reaching the chain here is one with no in-tree
/// cache entry, so it would otherwise be invisible rather than merely uncached.
pub async fn gap_entries(
    repo: &Repository,
    ebuilds: Vec<crate::repo::Ebuild>,
    covered: &std::collections::HashSet<Cpv>,
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
    resolve_ebuilds(repo, missing, &mut HashMap::new()).await
}

/// Run the four-step chain over `ebuilds`, consuming matching entries out of
/// `cached` (the primary bulk walk) as it goes.
async fn resolve_ebuilds(
    repo: &Repository,
    ebuilds: impl IntoIterator<Item = crate::repo::Ebuild>,
    cached: &mut HashMap<Cpv, CacheEntry>,
) -> Vec<(Cpv, CacheEntry)> {
    let masters = repo.masters();
    let mut out: Vec<(Cpv, CacheEntry)> = Vec::new();
    let mut shell = None;
    // Eclass digests are shared across all entries of this repo — without
    // the memo every cached entry re-hashes its full eclass list.
    let mut digests: std::collections::HashMap<String, Option<String>> = Default::default();

    for ebuild in ebuilds {
        let cpv = ebuild.cpv().clone();
        let Ok(bytes) = std::fs::read(ebuild.path()) else {
            continue;
        };
        let digest = format!("{:x}", md5::compute(&bytes));
        let valid =
            |entry: &CacheEntry,
             digests: &mut std::collections::HashMap<String, Option<String>>| {
                entry
                    .md5
                    .as_deref()
                    .is_some_and(|m| m.eq_ignore_ascii_case(&digest))
                    && repo.is_fresh_cached(entry, digests)
            };

        // Primary bulk walk (in-tree md5-cache).
        if let Some(entry) = cached.remove(&cpv)
            && valid(&entry, &mut digests)
        {
            out.push((cpv, entry));
            continue;
        }

        // Layered lookup: primary miss path already tried above for bulk;
        // this hits secondary (and any primary entry not in the bulk map).
        if let Ok(Some(entry)) = repo.cache_entry(&cpv)
            && valid(&entry, &mut digests)
        {
            out.push((cpv, entry));
            continue;
        }

        // Symlinked ebuild → master's primary cache entry.
        if let Some(entry) = master_cache_entry(&ebuild, masters, &digest) {
            out.push((cpv, entry));
            continue;
        }

        // Source the ebuild (shell started lazily, masters' eclasses visible).
        let sh = match &mut shell {
            Some(s) => s,
            None => {
                let master_refs: Vec<&Repository> = masters.iter().collect();
                match repo.shell_with_masters(&master_refs).await {
                    Ok(s) => shell.insert(s),
                    Err(e) => {
                        tracing::error!(
                            "repo '{}': cannot start ebuild shell for metadata: {e}",
                            repo.name()
                        );
                        break;
                    }
                }
            }
        };
        match sh.source_ebuild(&ebuild).await {
            Ok(sourced) => {
                let eclasses = sourced
                    .eclasses
                    .iter()
                    .filter_map(|(name, path)| {
                        std::fs::read(path)
                            .ok()
                            .map(|b| (name.clone(), format!("{:x}", md5::compute(&b))))
                    })
                    .collect();
                let entry = CacheEntry {
                    metadata: sourced.metadata,
                    md5: Some(digest),
                    eclasses,
                };
                if let Err(e) = repo.put_secondary(&cpv, &entry) {
                    tracing::warn!(
                        "repo '{}': failed to write secondary cache for {cpv}: {e}",
                        repo.name()
                    );
                }
                out.push((cpv, entry));
            }
            Err(e) => {
                tracing::error!("repo '{}': failed to source {cpv}: {e}", repo.name());
            }
        }
    }
    out
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
