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
//! The main repo ships a cache covering almost everything, and validating it
//! means reading all ~32k ebuild files to digest them — too expensive per
//! resolve. But "almost" is not "all": ~50 ::gentoo ebuilds currently have no
//! cache entry, and a version with no entry is not merely uncached, it is
//! **invisible** — `load_repos` builds `RepoData` from cache entries alone, so
//! the version cannot be selected, and the atom is reported as "no ebuilds"
//! rather than masked. [`gap_entries`] closes exactly that hole: it runs the
//! same chain over only the CPVs the primary walk did not yield, so the cost is
//! one directory walk plus sourcing the handful that are genuinely missing.
//!
//! Not covered: a **stale** primary entry (ebuild newer than its cache) still
//! wins for the main repo, because detecting that is the expensive full-digest
//! pass. See `todo/md5-cache-blind-spot.md`.

use std::collections::HashMap;

use portage_atom::Cpv;
use portage_metadata::CacheEntry;

use crate::cache::{CacheReadOpts, cache_entries_parallel};
use crate::repo::Repository;

/// Every metadata entry of `repo`: primary bulk walk, layered cache lookup,
/// master-symlink shortcut, then live source into secondary.
pub async fn overlay_entries(repo: &Repository, masters: &[Repository]) -> Vec<(Cpv, CacheEntry)> {
    let mut cached: HashMap<Cpv, CacheEntry> = cache_entries_parallel(
        std::slice::from_ref(repo),
        &CacheReadOpts::default(),
        |text| CacheEntry::parse(text).map_err(crate::Error::from),
    )
    .await
    .into_iter()
    .filter_map(|(cpv, e)| e.ok().map(|e| (cpv, e)))
    .collect();

    let Ok(ebuilds) = repo.ebuilds_with_masters(masters) else {
        return cached.into_iter().collect();
    };

    resolve_ebuilds(repo, masters, ebuilds, &mut cached).await
}

/// Every metadata entry of a cache-backed repo: the in-tree bulk read, plus
/// the chain over whatever ebuilds that read did not cover.
///
/// The walk exists only to find the cache's gaps and costs about as much as the
/// read it would otherwise follow (~0.15s on ::gentoo against a ~1s resolve),
/// so the two run concurrently — `Ebuilds` owns its walker, so it moves onto a
/// blocking thread cleanly. Unparseable entries are dropped, as the bulk read
/// alone already did.
pub async fn primary_entries(repo: &Repository) -> Vec<(Cpv, CacheEntry)> {
    let scan = repo.ebuilds_with_masters(&[]).ok();
    let opts = CacheReadOpts::default();
    let (bulk, ebuilds) = tokio::join!(
        cache_entries_parallel(std::slice::from_ref(repo), &opts, |text| {
            CacheEntry::parse(text).map_err(crate::Error::from)
        }),
        async {
            match scan {
                Some(s) => tokio::task::spawn_blocking(move || s.into_iter().collect::<Vec<_>>())
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
    out.extend(gap_entries(repo, &[], ebuilds, &covered).await);
    out
}

/// The same chain over only the CPVs `covered` does not already contain — the
/// cache's gap. Every ebuild reaching the chain here is one with no in-tree
/// cache entry, so it would otherwise be invisible rather than merely uncached.
pub async fn gap_entries(
    repo: &Repository,
    masters: &[Repository],
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
    resolve_ebuilds(repo, masters, missing, &mut HashMap::new()).await
}

/// Run the four-step chain over `ebuilds`, consuming matching entries out of
/// `cached` (the primary bulk walk) as it goes.
async fn resolve_ebuilds(
    repo: &Repository,
    masters: &[Repository],
    ebuilds: impl IntoIterator<Item = crate::repo::Ebuild>,
    cached: &mut HashMap<Cpv, CacheEntry>,
) -> Vec<(Cpv, CacheEntry)> {
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
                    && repo.is_fresh_cached(entry, masters, digests)
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
