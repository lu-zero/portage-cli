//! The md5-cache database as the repository's entries and primary cache

use std::collections::HashMap;
use std::io;
use std::ops::Bound;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use camino::{Utf8Path, Utf8PathBuf};
use futures_util::future::BoxFuture;
use portage_atom::Cpv;
use portage_atom::interner::{DefaultInterner, Interned};
use portage_metadata::CacheEntry;
use redb::{ReadOnlyDatabase, ReadableDatabase, ReadableTable};

use super::RedbTree;
use super::schema::{
    CACHE_ENTRIES, CACHE_SCHEMA, EBUILDS, ECLASSES, KIND_BLOB, check_schema, db_err, decode,
};
use crate::repo::Repository;
use crate::tree::{EntrySource, TreeSource};

/// Valid entries, and the ebuilds without one
type Split = (Vec<(Cpv, CacheEntry)>, Vec<Cpv>);

/// [`Split`] of one key range, with the range's index
type RangeEntries = (usize, Vec<(Cpv, CacheEntry)>, Vec<Cpv>);

/// The md5-cache database of a [`RedbTree`]
pub struct RedbCache {
    /// `None` when there is no cache database: every ebuild is then left to be sourced
    db: Option<ReadOnlyDatabase>,
    file: Utf8PathBuf,
    tree: Arc<RedbTree>,
}

impl RedbCache {
    /// Open the cache database at `file`, describing `tree`
    pub fn open(file: impl Into<Utf8PathBuf>, tree: Arc<RedbTree>) -> io::Result<Self> {
        let file = file.into();
        let db = ReadOnlyDatabase::open(&file).map_err(db_err)?;
        check_schema(&db.begin_read().map_err(db_err)?, CACHE_SCHEMA)?;
        Ok(RedbCache {
            db: Some(db),
            file,
            tree,
        })
    }

    /// No cache database at `file` (yet) for `tree`
    pub fn missing(file: impl Into<Utf8PathBuf>, tree: Arc<RedbTree>) -> Self {
        RedbCache {
            db: None,
            file: file.into(),
            tree,
        }
    }
}

impl EntrySource for RedbCache {
    fn entries<'a>(&'a self, repo: &'a Repository) -> BoxFuture<'a, Vec<(Cpv, CacheEntry)>> {
        Box::pin(async move {
            match self.read_entries(repo) {
                Ok((mut out, gaps)) => {
                    out.extend(crate::entries::backend_gaps(repo, gaps).await);
                    out
                }
                Err(e) => {
                    // Every ebuild still has its text in the tree: source them all.
                    tracing::error!("repo '{}': {}: {e}", repo.name(), self.file);
                    match self.tree.cpvs() {
                        Ok(cpvs) => crate::entries::backend_gaps(repo, cpvs).await,
                        Err(e) => {
                            tracing::error!("repo '{}': {}: {e}", repo.name(), self.tree.file);
                            Vec::new()
                        }
                    }
                }
            }
        })
    }

    fn raw_entries(&self, latest_per_cpn: bool) -> Option<io::Result<Vec<(Cpv, String)>>> {
        Some(self.raw_texts(latest_per_cpn))
    }
}

impl crate::metadata_cache::MetadataCache for RedbCache {
    fn get(&self, cpv: &Cpv) -> crate::Result<Option<CacheEntry>> {
        let key = format!("{}/{}-{}", cpv.cpn.category, cpv.cpn.package, cpv.version);
        let shown = || format!("{}?{key}", self.file);
        let io = |e: io::Error| crate::repo::util::io_err(shown(), e);
        let Some(db) = &self.db else {
            return Ok(None);
        };
        let txn = db.begin_read().map_err(db_err).map_err(io)?;
        let table = txn.open_table(CACHE_ENTRIES).map_err(db_err).map_err(io)?;
        let Some(stored) = table.get(key.as_str()).map_err(db_err).map_err(io)? else {
            return Ok(None);
        };
        let (_, data) = decode(stored.value()).map_err(io)?;
        let text = String::from_utf8(data).map_err(|e| io(db_err(e)))?;
        Ok(Some(CacheEntry::parse(&text)?))
    }

    fn put(&self, _cpv: &Cpv, _entry: &CacheEntry) -> crate::Result<()> {
        Err(crate::repo::util::io_err(
            self.file.as_str(),
            io::Error::new(io::ErrorKind::ReadOnlyFilesystem, "written only by sync"),
        ))
    }

    fn is_populated(&self) -> bool {
        self.db.is_some()
    }
}

impl RedbCache {
    fn raw_texts(&self, latest_per_cpn: bool) -> io::Result<Vec<(Cpv, String)>> {
        let Some(db) = &self.db else {
            return Ok(Vec::new());
        };
        let txn = db.begin_read().map_err(db_err)?;
        let table = txn.open_table(CACHE_ENTRIES).map_err(db_err)?;
        // One sequential pass copies; parsing names and decompressing is spread over workers,
        // like the directory read spreads file reads.
        let mut stored: Vec<(String, Vec<u8>)> = Vec::new();
        for row in table.iter().map_err(db_err)? {
            let (key, value) = row.map_err(db_err)?;
            stored.push((key.value().to_string(), value.value().to_vec()));
        }
        let named: Vec<(Cpv, &[u8])> = par_filter_map(&stored, |(key, raw)| {
            Ok(Cpv::parse(key).ok().map(|cpv| (cpv, raw.as_slice())))
        })?;
        // The newest version is chosen from the names, so only those entries are decompressed.
        let chosen: Vec<(Cpv, &[u8])> = if latest_per_cpn {
            let mut best: HashMap<portage_atom::Cpn, (Cpv, &[u8])> = HashMap::new();
            for (cpv, raw) in named {
                match best.get(&cpv.cpn) {
                    Some((prev, _)) if prev.version >= cpv.version => {}
                    _ => {
                        best.insert(cpv.cpn, (cpv, raw));
                    }
                }
            }
            best.into_values().collect()
        } else {
            named
        };
        par_filter_map(&chosen, |(cpv, raw)| {
            let (_, data) = decode(raw)?;
            Ok(String::from_utf8(data).ok().map(|text| (cpv.clone(), text)))
        })
    }
}

/// `f` over `items` on the worker threads, in order, dropping the `None`s
fn par_filter_map<'a, T: Sync, R: Send>(
    items: &'a [T],
    f: impl Fn(&'a T) -> io::Result<Option<R>> + Sync,
) -> io::Result<Vec<R>> {
    let workers = crate::default_worker_count().max(1);
    let chunk = items.len().div_ceil(workers).max(1);
    let f = &f;
    std::thread::scope(|s| {
        let handles: Vec<_> = items
            .chunks(chunk)
            .map(|part| {
                s.spawn(move || -> io::Result<Vec<R>> {
                    let mut out = Vec::with_capacity(part.len());
                    for item in part {
                        out.extend(f(item)?);
                    }
                    Ok(out)
                })
            })
            .collect();
        let mut all = Vec::with_capacity(items.len());
        for h in handles {
            all.extend(h.join().map_err(|_| db_err("cache worker panicked"))??);
        }
        Ok(all)
    })
}

/// Whether the `_md5_` hex text records `digest`
fn md5_matches(recorded: Option<&str>, digest: &[u8]) -> bool {
    let mut bytes = [0u8; 16];
    recorded
        .is_some_and(|hex| hex::decode_to_slice(hex, &mut bytes).is_ok() && bytes[..] == *digest)
}

impl RedbCache {
    /// The md5 of every eclass, recorded when the tree was written
    fn eclass_digests(
        txn: &redb::ReadTransaction,
    ) -> io::Result<HashMap<Interned<DefaultInterner>, md5::Digest>> {
        let table = txn.open_table(ECLASSES).map_err(db_err)?;
        let mut out = HashMap::new();
        for row in table.iter().map_err(db_err)? {
            let (k, v) = row.map_err(db_err)?;
            let mut digest = [0u8; 16];
            hex::decode_to_slice(v.value(), &mut digest).map_err(db_err)?;
            out.insert(Interned::intern(k.value()), md5::Digest(digest));
        }
        Ok(out)
    }

    /// Valid entries for the keys in `lo..hi`, merging the ordered `ebuilds` and `entries` tables
    fn entries_in(
        ebuilds: &redb::ReadOnlyTable<&str, &[u8]>,
        cache: Option<&redb::ReadOnlyTable<&str, &[u8]>>,
        (lo, hi): (Option<&str>, Option<&str>),
        stored: Option<&HashMap<Interned<DefaultInterner>, md5::Digest>>,
        out: &mut Vec<(Cpv, CacheEntry)>,
        gaps: &mut Vec<Cpv>,
    ) -> io::Result<()> {
        let bounds = (
            lo.map_or(Bound::Unbounded, Bound::Included),
            hi.map_or(Bound::Unbounded, Bound::Excluded),
        );
        let mut entries = cache
            .map(|c| c.range::<&str>(bounds))
            .transpose()
            .map_err(db_err)?
            .into_iter()
            .flatten()
            .peekable();
        for row in ebuilds.range::<&str>(bounds).map_err(db_err)? {
            let (key, digest) = row.map_err(db_err)?;
            let key = key.value();
            let mut found = None;
            // An error row is taken and returned, not left peeked to turn every later key into a gap.
            while let Some(next) =
                entries.next_if(|e| e.as_ref().map_or(true, |(k, _)| k.value() <= key))
            {
                let (k, v) = next.map_err(db_err)?;
                if k.value() == key {
                    found = Some(v);
                }
            }
            let Ok(cpv) = Cpv::parse(key) else { continue };
            let entry = match found.map(|v| decode(v.value())).transpose()? {
                Some((KIND_BLOB, data)) => std::str::from_utf8(&data)
                    .ok()
                    .and_then(|text| CacheEntry::parse(text).ok()),
                _ => None,
            };
            let valid = entry.filter(|entry| {
                md5_matches(entry.md5.as_deref(), digest.value())
                    && stored.is_none_or(|stored| {
                        entry
                            .eclasses
                            .iter()
                            .all(|(name, d)| stored.get(name) == Some(d))
                    })
            });
            match valid {
                Some(entry) => out.push((cpv, entry)),
                None => gaps.push(cpv),
            }
        }
        Ok(())
    }

    /// The valid entries, and the ebuilds that have none
    fn read_entries(&self, repo: &Repository) -> io::Result<Split> {
        let tree_txn = self.tree.db.begin_read().map_err(db_err)?;
        let cache_txn = self
            .db
            .as_ref()
            .map(|db| db.begin_read())
            .transpose()
            .map_err(db_err)?;
        // Inherited eclasses may come from a master on another backend: those go through the
        // master's tree, the own ones are compared with the stored digests.
        let stored = if repo.masters().is_empty() {
            Some(Self::eclass_digests(&tree_txn)?)
        } else {
            None
        };
        // Split the key space at category names; the ranges between them cover every key,
        // listed category or not. Workers take ranges in turn.
        let mut cuts: Vec<String> = match self.tree.read(Utf8Path::new("profiles/categories"))? {
            Some(bytes) => String::from_utf8_lossy(&bytes)
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(|c| format!("{c}/"))
                .collect(),
            None => Vec::new(),
        };
        cuts.sort();
        let ranges: Vec<(Option<&str>, Option<&str>)> = std::iter::once(None)
            .chain(cuts.iter().map(|c| Some(c.as_str())))
            .zip(
                cuts.iter()
                    .map(|c| Some(c.as_str()))
                    .chain(std::iter::once(None)),
            )
            .collect();
        let next = AtomicUsize::new(0);
        let workers = crate::default_worker_count().clamp(1, ranges.len());
        let (parsed, mut gaps) = std::thread::scope(|s| {
            let handles: Vec<_> = (0..workers)
                .map(|_| {
                    let (tree_txn, cache_txn, ranges, next) =
                        (&tree_txn, &cache_txn, &ranges, &next);
                    let stored = stored.as_ref();
                    s.spawn(move || -> io::Result<Vec<RangeEntries>> {
                        let ebuilds = tree_txn.open_table(EBUILDS).map_err(db_err)?;
                        let cache = cache_txn
                            .as_ref()
                            .map(|txn| txn.open_table(CACHE_ENTRIES))
                            .transpose()
                            .map_err(db_err)?;
                        let mut out = Vec::new();
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            let Some(&range) = ranges.get(i) else { break };
                            let (mut part, mut gaps) = (Vec::new(), Vec::new());
                            Self::entries_in(
                                &ebuilds,
                                cache.as_ref(),
                                range,
                                stored,
                                &mut part,
                                &mut gaps,
                            )?;
                            out.push((i, part, gaps));
                        }
                        Ok(out)
                    })
                })
                .collect();
            // Key order, whichever worker took which range.
            let mut parts = Vec::new();
            for h in handles {
                parts.extend(h.join().map_err(|_| db_err("entry worker panicked"))??);
            }
            parts.sort_unstable_by_key(|(i, _, _)| *i);
            let (mut parsed, mut gaps) = (Vec::new(), Vec::new());
            for (_, part, part_gaps) in parts {
                parsed.extend(part);
                gaps.extend(part_gaps);
            }
            Ok::<_, io::Error>((parsed, gaps))
        })?;

        if stored.is_some() {
            return Ok((parsed, gaps));
        }
        let mut digests = HashMap::new();
        let (fresh, stale): (Vec<_>, Vec<_>) = parsed
            .into_iter()
            .partition(|(_, entry)| repo.is_fresh_cached(entry, &mut digests));
        gaps.extend(stale.into_iter().map(|(cpv, _)| cpv));
        Ok((fresh, gaps))
    }
}

#[cfg(test)]
mod tests {
    use super::super::schema::CODEC_RAW;
    use super::*;

    #[test]
    fn entries_without_a_valid_stored_entry_are_gaps() {
        let dir = tempfile::tempdir().unwrap();
        let file = Utf8Path::from_path(dir.path()).unwrap().join("t.redb");
        let db = redb::Database::create(&file).unwrap();
        let eclass = md5::compute("eclass");
        let entry = |ebuild: &str, eclass: md5::Digest| {
            let text = format!(
                "EAPI=8\nDESCRIPTION=t\nSLOT=0\n_eclasses_=e\t{eclass:x}\n_md5_={:x}\n",
                md5::compute(ebuild)
            );
            [&[KIND_BLOB, CODEC_RAW][..], text.as_bytes()].concat()
        };
        let txn = db.begin_write().unwrap();
        {
            let mut ebuilds = txn.open_table(EBUILDS).unwrap();
            let mut cache = txn.open_table(CACHE_ENTRIES).unwrap();
            for (key, text) in [
                ("a/fresh-1", "fresh"),
                ("a/missing-1", "missing"),
                ("b/edited-1", "edited now"),
                ("b/stale-1", "stale"),
            ] {
                ebuilds.insert(key, &md5::compute(text).0[..]).unwrap();
            }
            cache
                .insert("a/fresh-1", &entry("fresh", eclass)[..])
                .unwrap();
            cache
                .insert("b/edited-1", &entry("edited", eclass)[..])
                .unwrap();
            cache
                .insert("b/stale-1", &entry("stale", md5::compute("old"))[..])
                .unwrap();
            cache
                .insert("c/removed-1", &entry("removed", eclass)[..])
                .unwrap();
        }
        txn.commit().unwrap();

        let txn = db.begin_read().unwrap();
        let (ebuilds, cache) = (
            txn.open_table(EBUILDS).unwrap(),
            txn.open_table(CACHE_ENTRIES).unwrap(),
        );
        let stored = HashMap::from([(Interned::intern("e"), eclass)]);
        let (mut valid, mut gaps) = (Vec::new(), Vec::new());
        for range in [(None, Some("b/")), (Some("b/"), None)] {
            RedbCache::entries_in(
                &ebuilds,
                Some(&cache),
                range,
                Some(&stored),
                &mut valid,
                &mut gaps,
            )
            .unwrap();
        }
        let names = |cpvs: Vec<&Cpv>| cpvs.iter().map(|c| c.to_string()).collect::<Vec<_>>();
        assert_eq!(names(valid.iter().map(|(c, _)| c).collect()), ["a/fresh-1"]);
        assert_eq!(
            names(gaps.iter().collect()),
            ["a/missing-1", "b/edited-1", "b/stale-1"]
        );

        let (mut valid, mut gaps) = (Vec::new(), Vec::new());
        RedbCache::entries_in(
            &ebuilds,
            None,
            (None, None),
            Some(&stored),
            &mut valid,
            &mut gaps,
        )
        .unwrap();
        assert!(
            valid.is_empty(),
            "with no cache database every ebuild is a gap"
        );
        assert_eq!(gaps.len(), 4);
    }
}
