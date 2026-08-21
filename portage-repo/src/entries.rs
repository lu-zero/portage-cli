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

use std::collections::HashMap;

use portage_atom::Cpv;
use portage_metadata::CacheEntry;

use crate::cache::{
    CacheReadOpts, cache_entries_parallel, cache_entries_parallel_with_mtime,
    secondary_cache_entries_with_mtime,
};
use crate::repo::Repository;

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

    // Unchanged tree: the suspects are known, and their entries are already in
    // the secondary store from the run that discovered them. Gated on
    // `has_sync_marker`: without a rewritten-on-sync timestamp.chk, `stamp`
    // degrades to the repo root directory's mtime, which is invariant under
    // exactly the in-place edits a hand-maintained overlay gets — trusting
    // the memo there would pin it on a stale answer forever.
    if repo.has_sync_marker()
        && let (Some(stamp), Some(index)) = (&stamp, &index)
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
    let (bulk, secondary_bulk, ebuilds) = tokio::join!(
        cache_entries_parallel_with_mtime(std::slice::from_ref(repo), &opts, |text| {
            CacheEntry::parse(text).map_err(crate::Error::from)
        }),
        // The durable secondary store's own entries — without this, a repo
        // with no in-tree cache (crossdev, pentoo-shaped trees) never has
        // anything in `covered`, so every one of its ebuilds re-enters the
        // full suspect chain on every call even once the secondary already
        // holds a fresh answer for it (previously only consulted one cpv at
        // a time, deep inside that chain).
        secondary_cache_entries_with_mtime(repo, &opts, |text| {
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

    let mut out: Vec<(Cpv, CacheEntry)> = Vec::with_capacity(bulk.len() + secondary_bulk.len());
    // The cache file's own mtime, per cpv — the "cache file serving e.cpv"
    // half of the per-entry suspect rule, gathered in the same pass as the
    // bulk read (no second directory walk).
    let mut cache_mtime: HashMap<Cpv, std::time::SystemTime> =
        HashMap::with_capacity(bulk.len() + secondary_bulk.len());
    let mut covered: std::collections::HashSet<Cpv> =
        std::collections::HashSet::with_capacity(bulk.len() + secondary_bulk.len());
    // Primary first, so it wins a cpv present in both — same precedence
    // `Repository::cache_entry` already gives primary over secondary.
    for (cpv, mtime, entry) in bulk.into_iter().chain(secondary_bulk) {
        if covered.contains(&cpv) {
            continue;
        }
        if let Some(m) = mtime {
            cache_mtime.insert(cpv.clone(), m);
        }
        if let Ok(entry) = entry {
            covered.insert(cpv.clone());
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
    // fetch it from the secondary store for nothing. Same `has_sync_marker`
    // gate as the read above — no point writing a sidecar this repo can
    // never safely consult.
    if repo.has_sync_marker()
        && let (Some(stamp), Some(index)) = (&stamp, &index)
    {
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

/// The CPVs a previous run found the cache could not serve, if `stamp`
/// still matches the tree
///
/// `None` on any mismatch or read failure — the caller then rescans, so a
/// corrupt or partial sidecar costs time, never correctness.
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
    let mut digests: std::collections::HashMap<
        portage_atom::interner::Interned<portage_atom::interner::DefaultInterner>,
        Option<md5::Digest>,
    > = Default::default();

    for ebuild in ebuilds {
        let cpv = ebuild.cpv().clone();
        let Ok(bytes) = std::fs::read(ebuild.path()) else {
            continue;
        };
        let digest = format!("{:x}", md5::compute(&bytes));
        let valid = |entry: &CacheEntry,
                     digests: &mut std::collections::HashMap<
            portage_atom::interner::Interned<portage_atom::interner::DefaultInterner>,
            Option<md5::Digest>,
        >| {
            entry
                .md5
                .as_deref()
                .is_some_and(|m| m.eq_ignore_ascii_case(&digest))
                && repo.is_fresh_cached(entry, digests)
                && !entry.metadata.has_parse_failure()
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
                        std::fs::read(path).ok().map(|b| {
                            (
                                portage_atom::interner::Interned::intern(name),
                                md5::compute(&b),
                            )
                        })
                    })
                    .collect();
                let entry = CacheEntry {
                    metadata: sourced.metadata,
                    md5: Some(digest),
                    eclasses,
                };
                // Sourcing is the freshest possible source of truth - if the
                // ebuild's own DEPEND-family/SRC_URI text still fails to
                // parse here, re-sourcing again later won't fix it. Writing
                // the entry anyway would launder the failure: serialize()
                // omits a parse-failed field entirely (290ce4c), and the
                // written-back text reparses clean next time (no raw text
                // left to fail), so the failure could never be detected
                // again. Match build_entry's precedent instead: skip this
                // ebuild rather than silently caching it dependency-free.
                if entry.metadata.has_parse_failure() {
                    tracing::error!(
                        "repo '{}': {cpv}: {}, skipping",
                        repo.name(),
                        entry.metadata.parse_failure_summary()
                    );
                    continue;
                }
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

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;

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
