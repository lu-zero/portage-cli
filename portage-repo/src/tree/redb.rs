//! A repository stored in one redb file: the git objects of one commit plus path and md5-cache
//! indexes, written by a sync that never makes a checkout.

use std::collections::HashMap;
use std::io;

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
const MD5_CACHE: TableDefinition<&str, &[u8]> = TableDefinition::new("md5_cache");
const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");
const ECLASSES: TableDefinition<&str, &[u8]> = TableDefinition::new("eclasses");

const CODEC_RAW: u8 = 0;
const CODEC_ZSTD: u8 = 1;
const KIND_TREE: u8 = 2;
const KIND_BLOB: u8 = 3;

type Oid = [u8; 20];

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

    fn display(&self, rel: &Utf8Path) -> String {
        format!("redb://{}?{rel}", self.file)
    }
}

impl EntrySource for RedbTree {
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
}

impl RedbTree {
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

    fn read_entries(&self, repo: &Repository) -> io::Result<Vec<(Cpv, CacheEntry)>> {
        let txn = self.db.begin_read().map_err(db_err)?;
        // Inherited eclasses may come from a master on another backend: those go through the
        // master's tree, the own ones are compared with the stored digests.
        let stored = if repo.masters().is_empty() {
            Some(Self::eclass_digests(&txn)?)
        } else {
            None
        };
        let index = txn.open_table(MD5_CACHE).map_err(db_err)?;
        let mut keys: Vec<(String, Oid)> = Vec::new();
        for row in index.iter().map_err(db_err)? {
            let (k, v) = row.map_err(db_err)?;
            if let Ok(oid) = Oid::try_from(v.value()) {
                keys.push((k.value().to_string(), oid));
            }
        }
        drop(index);

        let workers = crate::default_worker_count().max(1);
        let chunk = keys.len().div_ceil(workers).max(1);
        let parsed: Vec<(Cpv, CacheEntry)> = std::thread::scope(|s| {
            let handles: Vec<_> = keys
                .chunks(chunk)
                .map(|part| {
                    let (txn, stored) = (&txn, stored.as_ref());
                    s.spawn(move || -> io::Result<Vec<(Cpv, CacheEntry)>> {
                        let objects = txn.open_table(OBJECTS).map_err(db_err)?;
                        let mut out = Vec::with_capacity(part.len());
                        for (key, oid) in part {
                            let Ok(cpv) = Cpv::parse(key) else { continue };
                            let Some((KIND_BLOB, data)) = Self::object(&objects, oid)? else {
                                continue;
                            };
                            let Ok(text) = std::str::from_utf8(&data) else {
                                continue;
                            };
                            let Ok(entry) = CacheEntry::parse(text) else {
                                continue;
                            };
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
                        Ok(out)
                    })
                })
                .collect();
            let mut all = Vec::with_capacity(keys.len());
            for h in handles {
                all.extend(h.join().map_err(|_| db_err("entry worker panicked"))??);
            }
            Ok::<_, io::Error>(all)
        })?;

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
