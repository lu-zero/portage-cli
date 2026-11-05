//! A repository stored in redb, written by a sync that never makes a checkout
//!
//! Two files with separate lifecycles: the tree (`<location>.redb`: the git objects of one commit,
//! a path index and the md5 of every ebuild and eclass) and the md5-cache
//! (`<location>.md5-cache.redb`: `cat/PF` → entry text). Cache entries are checked against the
//! tree's digests, so the cache may lag behind or run ahead of the tree.

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
use redb::{ReadOnlyDatabase, ReadableDatabase, ReadableTable, TableDefinition};

use super::{EntryKind, EntrySource, TreeSource};
use crate::repo::Repository;

const OBJECTS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("objects");
const PATHS: TableDefinition<&str, &[u8]> = TableDefinition::new("paths");
const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");
const ECLASSES: TableDefinition<&str, &[u8]> = TableDefinition::new("eclasses");
const EBUILDS: TableDefinition<&str, &[u8]> = TableDefinition::new("ebuilds");
const CACHE_ENTRIES: TableDefinition<&str, &[u8]> = TableDefinition::new("entries");

const CODEC_RAW: u8 = 0;
const CODEC_ZSTD: u8 = 1;
const KIND_TREE: u8 = 2;
const KIND_BLOB: u8 = 3;

type Oid = [u8; 20];

/// The valid entries found in one key range, with the range's index
type RangeEntries = (usize, Vec<(Cpv, CacheEntry)>);

/// A repository tree read from a redb database
pub struct RedbTree {
    db: ReadOnlyDatabase,
    file: Utf8PathBuf,
    root: Oid,
}

fn db_err(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

impl RedbTree {
    /// Open the database at `file`
    pub fn open(file: impl Into<Utf8PathBuf>) -> io::Result<Self> {
        let file = file.into();
        let db = ReadOnlyDatabase::open(&file).map_err(db_err)?;
        let txn = db.begin_read().map_err(db_err)?;
        let meta = txn.open_table(META).map_err(db_err)?;
        let hex = meta
            .get("tree")
            .map_err(db_err)?
            .ok_or_else(|| db_err("no tree recorded"))?;
        let mut root = [0u8; 20];
        hex::decode_to_slice(hex.value(), &mut root).map_err(db_err)?;
        drop(meta);
        Ok(RedbTree { db, file, root })
    }

    /// The object `oid` decoded, with its kind byte
    fn object(
        table: &redb::ReadOnlyTable<&[u8], &[u8]>,
        oid: &Oid,
    ) -> io::Result<Option<(u8, Vec<u8>)>> {
        let Some(stored) = table.get(&oid[..]).map_err(db_err)? else {
            return Ok(None);
        };
        Ok(Some(decode(stored.value())?))
    }

    /// The object at `rel`, walking tree objects from the root
    fn lookup(&self, rel: &Utf8Path) -> io::Result<Option<(u8, Vec<u8>)>> {
        let txn = self.db.begin_read().map_err(db_err)?;
        let objects = txn.open_table(OBJECTS).map_err(db_err)?;
        let mut current = Self::object(&objects, &self.root)?;
        for part in rel.components() {
            let Some((KIND_TREE, data)) = current else {
                return Ok(None);
            };
            let Some(oid) = tree_entries(&data).find(|e| e.name == part.as_str().as_bytes()) else {
                return Ok(None);
            };
            current = Self::object(&objects, oid.oid)?;
        }
        Ok(current)
    }
}

fn decode(stored: &[u8]) -> io::Result<(u8, Vec<u8>)> {
    match stored {
        [kind, CODEC_RAW, data @ ..] => Ok((*kind, data.to_vec())),
        [kind, CODEC_ZSTD, a, b, c, d, data @ ..] => {
            let len = u32::from_le_bytes([*a, *b, *c, *d]) as usize;
            thread_local! {
                static DCTX: std::cell::RefCell<Option<zstd::bulk::Decompressor<'static>>> =
                    const { std::cell::RefCell::new(None) };
            }
            let out = DCTX.with(|d| {
                let mut d = d.borrow_mut();
                if d.is_none() {
                    *d = Some(zstd::bulk::Decompressor::new()?);
                }
                d.as_mut().map_or_else(
                    || Err(db_err("no zstd context")),
                    |d| d.decompress(data, len),
                )
            })?;
            Ok((*kind, out))
        }
        _ => Err(db_err("malformed stored object")),
    }
}

struct TreeEntry<'a> {
    mode: &'a [u8],
    name: &'a [u8],
    oid: &'a Oid,
}

/// The entries of a git tree object body: `<octal mode> <name>\0<20-byte oid>`, repeated
fn tree_entries(mut data: &[u8]) -> impl Iterator<Item = TreeEntry<'_>> {
    std::iter::from_fn(move || {
        let sp = data.iter().position(|&b| b == b' ')?;
        let nul = sp + data[sp..].iter().position(|&b| b == 0)?;
        let oid: &Oid = data.get(nul + 1..nul + 21)?.try_into().ok()?;
        let entry = TreeEntry {
            mode: &data[..sp],
            name: &data[sp + 1..nul],
            oid,
        };
        data = &data[nul + 21..];
        Some(entry)
    })
}

impl TreeSource for RedbTree {
    fn read(&self, rel: &Utf8Path) -> io::Result<Option<Vec<u8>>> {
        // `paths` holds files only, as `[mode u16][oid]`: one lookup instead of a tree walk.
        let txn = self.db.begin_read().map_err(db_err)?;
        let paths = txn.open_table(PATHS).map_err(db_err)?;
        let Some(row) = paths.get(rel.as_str()).map_err(db_err)? else {
            return Ok(None);
        };
        let Some(oid) = row.value().get(2..22).and_then(|o| Oid::try_from(o).ok()) else {
            return Err(db_err("malformed path entry"));
        };
        let objects = txn.open_table(OBJECTS).map_err(db_err)?;
        Ok(match Self::object(&objects, &oid)? {
            Some((KIND_BLOB, data)) => Some(data),
            _ => None,
        })
    }

    fn list(&self, rel: &Utf8Path) -> io::Result<Option<Vec<(String, EntryKind)>>> {
        let Some((KIND_TREE, data)) = self.lookup(rel)? else {
            return Ok(None);
        };
        Ok(Some(
            tree_entries(&data)
                .filter_map(|e| {
                    let name = String::from_utf8(e.name.to_vec()).ok()?;
                    let kind = if e.mode == b"40000" {
                        EntryKind::Dir
                    } else {
                        EntryKind::File
                    };
                    Some((name, kind))
                })
                .collect(),
        ))
    }

    fn kind(&self, rel: &Utf8Path) -> io::Result<Option<EntryKind>> {
        // The parent tree's entry mode answers without decoding the object itself.
        let (Some(parent), Some(name)) = (rel.parent(), rel.file_name()) else {
            return Ok(Some(EntryKind::Dir));
        };
        let Some((KIND_TREE, data)) = self.lookup(parent)? else {
            return Ok(None);
        };
        Ok(tree_entries(&data)
            .find(|e| e.name == name.as_bytes())
            .map(|e| {
                if e.mode == b"40000" {
                    EntryKind::Dir
                } else {
                    EntryKind::File
                }
            }))
    }

    fn display(&self, rel: &Utf8Path) -> String {
        format!(
            "redb://{}?{}",
            uri_part(self.file.as_str()),
            uri_part(rel.as_str())
        )
    }

    fn ebuild_index(&self) -> Option<io::Result<Vec<Utf8PathBuf>>> {
        Some(self.ebuild_paths())
    }
}

impl RedbTree {
    /// `cat/pkg/pkg-ver.ebuild` for every key of the `ebuilds` table (`cat/pkg-ver`)
    fn ebuild_paths(&self) -> io::Result<Vec<Utf8PathBuf>> {
        let txn = self.db.begin_read().map_err(db_err)?;
        let table = txn.open_table(EBUILDS).map_err(db_err)?;
        let mut out = Vec::new();
        for row in table.iter().map_err(db_err)? {
            let (key, _) = row.map_err(db_err)?;
            let Ok(cpv) = Cpv::parse(key.value()) else {
                continue;
            };
            let (cat, pf) = key.value().split_once('/').unwrap_or_default();
            out.push(Utf8PathBuf::from(format!(
                "{cat}/{}/{pf}.ebuild",
                cpv.cpn.package
            )));
        }
        Ok(out)
    }
}

/// `text` for a URI: `%`, `#`, spaces and control characters
/// percent-encoded, everything else (`+` included) as-is
fn uri_part(text: &str) -> std::borrow::Cow<'_, str> {
    let encode = |c: char| matches!(c, '%' | '#' | ' ') || c.is_control();
    if !text.chars().any(encode) {
        return text.into();
    }
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        if encode(c) {
            let mut buf = [0u8; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        } else {
            out.push(c);
        }
    }
    out.into()
}

/// `<location>.redb`
pub(crate) fn tree_file(location: &Utf8Path) -> Utf8PathBuf {
    Utf8PathBuf::from(format!("{location}.redb"))
}

/// The tree in `<location>.redb`; `None` when there is none or it cannot be opened
fn open_tree(location: &Utf8Path) -> Option<Arc<RedbTree>> {
    let db = tree_file(location);
    if !db.is_file() {
        return None;
    }
    match RedbTree::open(db.clone()) {
        Ok(tree) => Some(Arc::new(tree)),
        Err(e) => {
            tracing::warn!("ignoring {db}: {e}");
            None
        }
    }
}

/// [`open_tree`], as a [`TreeSource`]
pub(crate) fn tree_at(location: &Utf8Path) -> Option<Arc<dyn TreeSource>> {
    Some(open_tree(location)?)
}

/// The tree in `<location>.redb` and, when present, the entries in `<location>.md5-cache.redb`
/// (the checkout's otherwise); `None` when there is no tree database or it cannot be opened
pub(crate) fn storage_at(location: &Utf8Path) -> Option<super::Storage> {
    let tree = open_tree(location)?;
    let cache_db = tree_file(location).with_extension("md5-cache.redb");
    let mut storage = super::Storage::checkout(location);
    if cache_db.is_file() {
        match RedbCache::open(cache_db.clone(), Arc::clone(&tree)) {
            Ok(cache) => {
                let cache = Arc::new(cache);
                storage.entries = cache.clone();
                storage.primary = cache;
            }
            Err(e) => {
                tracing::warn!("ignoring {cache_db}: {e}");
                return None;
            }
        }
    }
    tracing::debug!("reading {location} from {}", tree.file);
    storage.tree = tree;
    Some(storage)
}

/// The md5-cache database of a [`RedbTree`]
pub struct RedbCache {
    db: ReadOnlyDatabase,
    file: Utf8PathBuf,
    tree: Arc<RedbTree>,
}

impl RedbCache {
    /// Open the cache database at `file`, describing `tree`
    pub fn open(file: impl Into<Utf8PathBuf>, tree: Arc<RedbTree>) -> io::Result<Self> {
        let file = file.into();
        let db = ReadOnlyDatabase::open(&file).map_err(db_err)?;
        Ok(RedbCache { db, file, tree })
    }
}

impl EntrySource for RedbCache {
    fn entries<'a>(&'a self, repo: &'a Repository) -> BoxFuture<'a, Vec<(Cpv, CacheEntry)>> {
        Box::pin(async move {
            match self.read_entries(repo) {
                Ok(out) => out,
                Err(e) => {
                    tracing::error!("repo '{}': {}: {e}", repo.name(), self.file);
                    Vec::new()
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
        let txn = self.db.begin_read().map_err(db_err).map_err(io)?;
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
        true
    }
}

impl RedbCache {
    fn raw_texts(&self, latest_per_cpn: bool) -> io::Result<Vec<(Cpv, String)>> {
        let txn = self.db.begin_read().map_err(db_err)?;
        let table = txn.open_table(CACHE_ENTRIES).map_err(db_err)?;
        // One sequential pass copies; parsing names and decompressing is spread over workers,
        // like the directory read spreads file reads.
        let mut stored: Vec<(String, Vec<u8>)> = Vec::new();
        for row in table.iter().map_err(db_err)? {
            let (key, value) = row.map_err(db_err)?;
            stored.push((key.value().to_string(), value.value().to_vec()));
        }
        let workers = crate::default_worker_count().max(1);
        let chunk = stored.len().div_ceil(workers).max(1);
        let all: Vec<(Cpv, String)> = std::thread::scope(|s| {
            let handles: Vec<_> = stored
                .chunks(chunk)
                .map(|part| {
                    s.spawn(move || -> io::Result<Vec<(Cpv, String)>> {
                        let mut out = Vec::with_capacity(part.len());
                        for (key, raw) in part {
                            let Ok(cpv) = Cpv::parse(key) else { continue };
                            let (_, data) = decode(raw)?;
                            if let Ok(text) = String::from_utf8(data) {
                                out.push((cpv, text));
                            }
                        }
                        Ok(out)
                    })
                })
                .collect();
            let mut all = Vec::with_capacity(stored.len());
            for h in handles {
                all.extend(h.join().map_err(|_| db_err("cache worker panicked"))??);
            }
            Ok::<_, io::Error>(all)
        })?;
        if !latest_per_cpn {
            return Ok(all);
        }
        let mut best: HashMap<portage_atom::Cpn, (Cpv, String)> = HashMap::new();
        for (cpv, text) in all {
            match best.get(&cpv.cpn) {
                Some((prev, _)) if prev.version >= cpv.version => {}
                _ => {
                    best.insert(cpv.cpn, (cpv, text));
                }
            }
        }
        Ok(best.into_values().collect())
    }
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
        cache: &redb::ReadOnlyTable<&str, &[u8]>,
        (lo, hi): (Option<&str>, Option<&str>),
        stored: Option<&HashMap<Interned<DefaultInterner>, md5::Digest>>,
        out: &mut Vec<(Cpv, CacheEntry)>,
    ) -> io::Result<usize> {
        let bounds = (
            lo.map_or(Bound::Unbounded, Bound::Included),
            hi.map_or(Bound::Unbounded, Bound::Excluded),
        );
        let mut entries = cache.range::<&str>(bounds).map_err(db_err)?.peekable();
        let mut missing = 0;
        for row in ebuilds.range::<&str>(bounds).map_err(db_err)? {
            let (key, digest) = row.map_err(db_err)?;
            let key = key.value();
            let mut found = None;
            while let Some(next) =
                entries.next_if(|e| e.as_ref().is_ok_and(|(k, _)| k.value() <= key))
            {
                let (k, v) = next.map_err(db_err)?;
                if k.value() == key {
                    found = Some(v);
                }
            }
            let Some(stored_entry) = found else {
                missing += 1;
                continue;
            };
            let Ok(cpv) = Cpv::parse(key) else { continue };
            let (KIND_BLOB, data) = decode(stored_entry.value())? else {
                continue;
            };
            let Ok(text) = std::str::from_utf8(&data) else {
                continue;
            };
            let Ok(entry) = CacheEntry::parse(text) else {
                continue;
            };
            if !md5_matches(entry.md5.as_deref(), digest.value()) {
                continue;
            }
            let fresh = stored.is_none_or(|stored| {
                entry
                    .eclasses
                    .iter()
                    .all(|(name, d)| stored.get(name) == Some(d))
            });
            if fresh {
                out.push((cpv, entry));
            }
        }
        Ok(missing)
    }

    fn read_entries(&self, repo: &Repository) -> io::Result<Vec<(Cpv, CacheEntry)>> {
        let tree_txn = self.tree.db.begin_read().map_err(db_err)?;
        let cache_txn = self.db.begin_read().map_err(db_err)?;
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
        let missing = AtomicUsize::new(0);
        let workers = crate::default_worker_count().clamp(1, ranges.len());
        let parsed: Vec<(Cpv, CacheEntry)> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..workers)
                .map(|_| {
                    let (tree_txn, cache_txn, ranges, next, missing) =
                        (&tree_txn, &cache_txn, &ranges, &next, &missing);
                    let stored = stored.as_ref();
                    s.spawn(move || -> io::Result<Vec<RangeEntries>> {
                        let ebuilds = tree_txn.open_table(EBUILDS).map_err(db_err)?;
                        let cache = cache_txn.open_table(CACHE_ENTRIES).map_err(db_err)?;
                        let mut out = Vec::new();
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            let Some(&range) = ranges.get(i) else { break };
                            let mut part = Vec::new();
                            let n = Self::entries_in(&ebuilds, &cache, range, stored, &mut part)?;
                            missing.fetch_add(n, Ordering::Relaxed);
                            out.push((i, part));
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
            parts.sort_unstable_by_key(|(i, _)| *i);
            Ok::<_, io::Error>(parts.into_iter().flat_map(|(_, part)| part).collect())
        })?;
        let missing = missing.into_inner();
        if missing > 0 {
            tracing::debug!("{}: {missing} ebuilds have no md5-cache entry", self.file);
        }

        if stored.is_some() {
            return Ok(parsed);
        }
        let mut digests = HashMap::new();
        Ok(parsed
            .into_iter()
            .filter(|(_, entry)| repo.is_fresh_cached(entry, &mut digests))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_parts_encode_only_what_would_break_the_uri() {
        assert_eq!(uri_part("sys-apps/gtk+-3.ebuild"), "sys-apps/gtk+-3.ebuild");
        assert_eq!(uri_part("files/a b#1%.patch"), "files/a%20b%231%25.patch");
    }
}
