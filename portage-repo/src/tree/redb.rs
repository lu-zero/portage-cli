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

/// Valid entries, and the ebuilds without one
type Split = (Vec<(Cpv, CacheEntry)>, Vec<Cpv>);

/// [`Split`] of one key range, with the range's index
type RangeEntries = (usize, Vec<(Cpv, CacheEntry)>, Vec<Cpv>);

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

    fn checkout(&self, rels: &[&Utf8Path], copies: Option<&Utf8Path>) -> io::Result<Utf8PathBuf> {
        let copies =
            copies.ok_or_else(|| db_err(format!("{} has no checkout to build from", self.file)))?;
        let root = copies.join(hex::encode(self.root));
        let txn = self.db.begin_read().map_err(db_err)?;
        let objects = txn.open_table(OBJECTS).map_err(db_err)?;
        for rel in rels {
            let dest = root.join(rel);
            if dest.symlink_metadata().is_ok() {
                continue;
            }
            let Some((mode, oid)) = self.entry(&objects, rel)? else {
                continue;
            };
            // Each path appears whole or not at all, so a concurrent em never sees half of it.
            std::fs::create_dir_all(copies)?;
            let staging = tempfile::Builder::new()
                .prefix(".staging-")
                .tempdir_in(copies)?;
            let tmp = Utf8Path::from_path(staging.path())
                .ok_or_else(|| db_err("non-UTF-8 staging directory"))?
                .join("x");
            write_object(&objects, &mode, &oid, &tmp)?;
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if let Err(e) = std::fs::rename(&tmp, &dest)
                && dest.symlink_metadata().is_err()
            {
                return Err(e);
            }
        }
        std::fs::create_dir_all(&root)?;
        // A copy in use stays recent, so another em building from it never sees it pruned.
        std::fs::File::open(&root)?.set_modified(std::time::SystemTime::now())?;
        prune_copies(copies, &root);
        Ok(root)
    }
}

/// How long a copy of another tree, or a staging directory left by a killed em, is kept unused
const COPY_KEEP: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);

/// Copies of other trees under `copies`, and leftover staging directories, unused for [`COPY_KEEP`]
fn prune_copies(copies: &Utf8Path, keep: &Utf8Path) {
    let Ok(dir) = copies.read_dir_utf8() else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in dir.flatten() {
        let name = entry.file_name();
        let ours = (name.len() == 40 && hex::decode(name).is_ok()) || name.starts_with(".staging-");
        let unused = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| now.duration_since(t).unwrap_or_default() > COPY_KEEP);
        if ours && unused && entry.path() != keep {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Write the object `oid` with git mode `mode` to `dest`
fn write_object(
    objects: &redb::ReadOnlyTable<&[u8], &[u8]>,
    mode: &[u8],
    oid: &Oid,
    dest: &Utf8Path,
) -> io::Result<()> {
    let missing = || db_err(format!("object {} missing for {dest}", hex::encode(oid)));
    if mode == b"160000" {
        return std::fs::create_dir(dest);
    }
    let (kind, data) = RedbTree::object(objects, oid)?.ok_or_else(missing)?;
    match (mode, kind) {
        (b"40000", KIND_TREE) => {
            std::fs::create_dir(dest)?;
            for e in tree_entries(&data) {
                let Ok(name) = std::str::from_utf8(e.name) else {
                    continue;
                };
                write_object(objects, e.mode, e.oid, &dest.join(name))?;
            }
            Ok(())
        }
        (b"120000", KIND_BLOB) => {
            let target = String::from_utf8(data).map_err(db_err)?;
            std::os::unix::fs::symlink(target, dest)
        }
        (_, KIND_BLOB) => {
            std::fs::write(dest, data)?;
            if mode == b"100755" {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))?;
            }
            Ok(())
        }
        _ => Err(db_err(format!("unexpected object for {dest}"))),
    }
}

impl RedbTree {
    /// The git mode and oid of the tree entry at `rel`
    fn entry(
        &self,
        objects: &redb::ReadOnlyTable<&[u8], &[u8]>,
        rel: &Utf8Path,
    ) -> io::Result<Option<(Vec<u8>, Oid)>> {
        let mut found = (b"40000".to_vec(), self.root);
        for part in rel.components() {
            let Some((KIND_TREE, data)) = Self::object(objects, &found.1)? else {
                return Ok(None);
            };
            let Some(e) = tree_entries(&data).find(|e| e.name == part.as_str().as_bytes()) else {
                return Ok(None);
            };
            found = (e.mode.to_vec(), *e.oid);
        }
        Ok(Some(found))
    }

    /// `cat/pkg/pkg-ver.ebuild` for every key of the `ebuilds` table (`cat/pkg-ver`)
    fn ebuild_paths(&self) -> io::Result<Vec<Utf8PathBuf>> {
        Ok(self
            .cpvs()?
            .into_iter()
            .map(|cpv| {
                let pf = format!("{}-{}", cpv.cpn.package, cpv.version);
                Utf8PathBuf::from(format!(
                    "{}/{}/{pf}.ebuild",
                    cpv.cpn.category, cpv.cpn.package
                ))
            })
            .collect())
    }

    /// Every cpv in the `ebuilds` table
    fn cpvs(&self) -> io::Result<Vec<Cpv>> {
        let txn = self.db.begin_read().map_err(db_err)?;
        let table = txn.open_table(EBUILDS).map_err(db_err)?;
        let mut out = Vec::new();
        for row in table.iter().map_err(db_err)? {
            let (key, _) = row.map_err(db_err)?;
            if let Ok(cpv) = Cpv::parse(key.value()) {
                out.push(cpv);
            }
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
    let cache = if cache_db.is_file() {
        match RedbCache::open(cache_db.clone(), Arc::clone(&tree)) {
            Ok(cache) => cache,
            Err(e) => {
                tracing::warn!("ignoring {cache_db}, sourcing what it would hold: {e}");
                RedbCache::missing(cache_db, Arc::clone(&tree))
            }
        }
    } else {
        RedbCache::missing(cache_db, Arc::clone(&tree))
    };
    let cache = Arc::new(cache);
    storage.entries = cache.clone();
    storage.primary = cache;
    tracing::debug!("reading {location} from {}", tree.file);
    storage.tree = tree;
    Some(storage)
}

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
    use super::*;

    #[test]
    fn uri_parts_encode_only_what_would_break_the_uri() {
        assert_eq!(uri_part("sys-apps/gtk+-3.ebuild"), "sys-apps/gtk+-3.ebuild");
        assert_eq!(uri_part("files/a b#1%.patch"), "files/a%20b%231%25.patch");
    }

    /// A database holding `objects` (oid byte, kind, body) with `root` as its tree
    fn database(file: &Utf8Path, root: u8, objects: &[(u8, u8, Vec<u8>)]) -> RedbTree {
        let db = redb::Database::create(file).unwrap();
        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(OBJECTS).unwrap();
            for (oid, kind, body) in objects {
                let stored = [&[*kind, CODEC_RAW][..], body].concat();
                table.insert(&[*oid; 20][..], &stored[..]).unwrap();
            }
            let mut meta = txn.open_table(META).unwrap();
            meta.insert("tree", hex::encode([root; 20]).as_bytes())
                .unwrap();
        }
        txn.commit().unwrap();
        drop(db);
        RedbTree::open(file).unwrap()
    }

    fn tree_body(entries: &[(&str, &str, u8)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (mode, name, oid) in entries {
            out.extend_from_slice(format!("{mode} {name}\0").as_bytes());
            out.extend_from_slice(&[*oid; 20]);
        }
        out
    }

    #[test]
    fn list_and_kind_walk_the_tree_objects() {
        let dir = tempfile::tempdir().unwrap();
        let dir = Utf8Path::from_path(dir.path()).unwrap();
        let tree = database(
            &dir.join("t.redb"),
            1,
            &[
                (1, KIND_TREE, tree_body(&[("40000", "eclass", 2)])),
                (
                    2,
                    KIND_TREE,
                    tree_body(&[("100644", "a.eclass", 3), ("40000", "tests", 4)]),
                ),
                (3, KIND_BLOB, b"text".to_vec()),
                (4, KIND_TREE, Vec::new()),
            ],
        );
        let mut names = tree.list(Utf8Path::new("eclass")).unwrap().unwrap();
        names.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            names,
            [
                ("a.eclass".into(), EntryKind::File),
                ("tests".into(), EntryKind::Dir)
            ]
        );
        assert_eq!(
            tree.kind(Utf8Path::new("eclass/a.eclass")).unwrap(),
            Some(EntryKind::File)
        );
        assert_eq!(
            tree.kind(Utf8Path::new("eclass/tests")).unwrap(),
            Some(EntryKind::Dir)
        );
        assert_eq!(tree.kind(Utf8Path::new("eclass/none")).unwrap(), None);
        assert_eq!(tree.list(Utf8Path::new("eclass/a.eclass")).unwrap(), None);
        assert_eq!(tree.list(Utf8Path::new("profiles")).unwrap(), None);
    }

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

    #[test]
    fn checkout_copies_modes_and_links_and_prunes_only_unused_copies() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let dir = Utf8Path::from_path(dir.path()).unwrap();
        let copies = dir.join("copies");
        let age = |path: &Utf8Path, days: u64| {
            std::fs::create_dir_all(path).unwrap();
            let when = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86400);
            std::fs::File::open(path)
                .unwrap()
                .set_modified(when)
                .unwrap();
        };
        let stale = copies.join(hex::encode([9u8; 20]));
        let in_use = copies.join(hex::encode([8u8; 20]));
        let left_behind = copies.join(".staging-x");
        age(&stale, 8);
        age(&left_behind, 8);
        age(&in_use, 1);

        let tree = database(
            &dir.join("t.redb"),
            1,
            &[
                (
                    1,
                    KIND_TREE,
                    tree_body(&[("40000", "eclass", 2), ("40000", "skipped", 2)]),
                ),
                (
                    2,
                    KIND_TREE,
                    tree_body(&[
                        ("100644", "a.eclass", 3),
                        ("100755", "run", 4),
                        ("120000", "b.eclass", 5),
                    ]),
                ),
                (3, KIND_BLOB, b"inherited".to_vec()),
                (4, KIND_BLOB, b"#!/bin/sh".to_vec()),
                (5, KIND_BLOB, b"a.eclass".to_vec()),
            ],
        );
        let rels = [Utf8Path::new("eclass"), Utf8Path::new("missing")];
        let root = tree.checkout(&rels, Some(&copies)).unwrap();
        assert_eq!(root, copies.join(hex::encode([1u8; 20])));
        let eclass = root.join("eclass");
        assert_eq!(
            std::fs::read_to_string(eclass.join("b.eclass")).unwrap(),
            "inherited"
        );
        assert!(
            eclass
                .join("b.eclass")
                .symlink_metadata()
                .unwrap()
                .is_symlink()
        );
        let mode = |p: &Utf8Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&eclass.join("run")), 0o755);
        assert!(!root.join("skipped").exists() && !root.join("missing").exists());
        assert!(
            !stale.exists() && !left_behind.exists(),
            "unused for a week: pruned"
        );
        assert!(in_use.exists(), "another em may still be building from it");

        std::fs::write(eclass.join("a.eclass"), "kept").unwrap();
        tree.checkout(&rels, Some(&copies)).unwrap();
        assert_eq!(
            std::fs::read_to_string(eclass.join("a.eclass")).unwrap(),
            "kept"
        );
        assert!(tree.checkout(&rels, None).is_err());
    }
}
