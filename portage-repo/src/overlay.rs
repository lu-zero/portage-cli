//! Overlay metadata loading.
//!
//! # Why this path exists (and only for overlays)
//!
//! The **main** repo is always loaded via in-tree `metadata/md5-cache`
//! (`cache_entries_parallel` in `load_repos`) — rsync/git ships that cache,
//! so resolve never sources ebuilds for `::gentoo`.
//!
//! **Overlays** (crossdev, local) often ship plain ebuilds and no md5-cache.
//! Resolve still needs DEPEND/IUSE/… for every CPV, so this module:
//!
//! 1. Prefer bulk primary walk entries when **fresh**
//! 2. Else [`Repository::cache_entry`] (primary then secondary) when **fresh**
//! 3. Else, for **symlinks into a master**, reuse the master's primary entry
//! 4. Else **source** the ebuild and [`Repository::put_secondary`]
//!
//! Secondary cache is always configured on the [`Repository`] (builder);
//! this module does not take free-floating cache paths.

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

    let mut out: Vec<(Cpv, CacheEntry)> = Vec::new();
    let mut shell = None;
    // Eclass digests are shared across all entries of this overlay — without
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
