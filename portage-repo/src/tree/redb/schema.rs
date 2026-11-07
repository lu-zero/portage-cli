//! The tables of the two databases, how values in them are encoded, and the schema versions

use std::io;

use redb::{ReadTransaction, TableDefinition};

pub(super) const OBJECTS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("objects");
pub(super) const PATHS: TableDefinition<&str, &[u8]> = TableDefinition::new("paths");
pub(super) const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");
pub(super) const ECLASSES: TableDefinition<&str, &[u8]> = TableDefinition::new("eclasses");
pub(super) const EBUILDS: TableDefinition<&str, &[u8]> = TableDefinition::new("ebuilds");
pub(super) const CACHE_ENTRIES: TableDefinition<&str, &[u8]> = TableDefinition::new("entries");
pub(super) const REFCOUNT: TableDefinition<&[u8], u32> = TableDefinition::new("refcount");
pub(super) const PACKAGES: TableDefinition<&str, &str> = TableDefinition::new("packages");
pub(super) const SYNC_LOG: TableDefinition<u64, &[u8]> = TableDefinition::new("sync_log");

/// `meta.schema` of the tree database
pub(super) const TREE_SCHEMA: &str = "2";
/// `meta.schema` of the md5-cache database
pub(super) const CACHE_SCHEMA: &str = "1";

pub(super) const CODEC_RAW: u8 = 0;
pub(super) const CODEC_ZSTD: u8 = 1;
pub(super) const KIND_TREE: u8 = 2;
pub(super) const KIND_BLOB: u8 = 3;

/// A git object id (SHA-1)
pub type Oid = [u8; 20];

/// The kind of a git object
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    /// A commit
    Commit,
    /// A tree
    Tree,
    /// A blob: a file's content, or a symlink's target
    Blob,
    /// An annotated tag
    Tag,
}

impl ObjectKind {
    pub(super) fn byte(self) -> u8 {
        match self {
            ObjectKind::Commit => 1,
            ObjectKind::Tree => KIND_TREE,
            ObjectKind::Blob => KIND_BLOB,
            ObjectKind::Tag => 4,
        }
    }

    pub(super) fn from_byte(byte: u8) -> io::Result<Self> {
        match byte {
            1 => Ok(ObjectKind::Commit),
            KIND_TREE => Ok(ObjectKind::Tree),
            KIND_BLOB => Ok(ObjectKind::Blob),
            4 => Ok(ObjectKind::Tag),
            _ => Err(db_err(format!("unknown object kind {byte}"))),
        }
    }
}

/// Bodies at least this long are compressed; below it zstd's frame costs more than it saves
const COMPRESS_FROM: usize = 64;

/// `body` as stored: kind, codec, and the body raw or zstd-compressed (trees stay raw, as diffs
/// read them often)
pub(super) fn encode(kind: ObjectKind, body: &[u8]) -> Vec<u8> {
    if kind != ObjectKind::Tree
        && body.len() >= COMPRESS_FROM
        && let Ok(len) = u32::try_from(body.len())
        && let Ok(packed) = zstd::bulk::compress(body, 3)
        && packed.len() + 4 < body.len()
    {
        return [&[kind.byte(), CODEC_ZSTD][..], &len.to_le_bytes(), &packed].concat();
    }
    [&[kind.byte(), CODEC_RAW][..], body].concat()
}

/// Where the md5-cache files are in the tree
pub(super) const MD5_CACHE_DIR: &str = "metadata/md5-cache/";

/// `cat/PF` of an md5-cache entry's path; `None` for anything else, Manifests included
pub(super) fn md5_cache_key(path: &str) -> Option<&str> {
    let rest = path.strip_prefix(MD5_CACHE_DIR)?;
    let (_, name) = rest.split_once('/')?;
    (!name.contains('/') && !name.starts_with("Manifest")).then_some(rest)
}

pub(super) fn db_err(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

/// Fail unless `meta.schema` is `expected`: a database of another layout would read as garbage
pub(super) fn check_schema(txn: &ReadTransaction, expected: &str) -> io::Result<()> {
    let meta = txn.open_table(META).map_err(db_err)?;
    let found = meta.get("schema").map_err(db_err)?;
    match found.as_ref().map(|v| v.value()) {
        Some(v) if v == expected.as_bytes() => Ok(()),
        Some(v) => Err(db_err(format!(
            "schema {}, em reads {expected}",
            String::from_utf8_lossy(v)
        ))),
        None => Err(db_err("no schema recorded")),
    }
}

/// A stored object's kind byte and decoded body
pub(super) fn decode(stored: &[u8]) -> io::Result<(u8, Vec<u8>)> {
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

pub(super) struct TreeEntry<'a> {
    pub(super) mode: &'a [u8],
    pub(super) name: &'a [u8],
    pub(super) oid: &'a Oid,
}

/// The entries of a git tree object body: `<octal mode> <name>\0<20-byte oid>`, repeated
pub(super) fn tree_entries(mut data: &[u8]) -> impl Iterator<Item = TreeEntry<'_>> {
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

#[cfg(test)]
mod tests {
    use redb::ReadableDatabase as _;

    use super::*;

    #[test]
    fn a_database_of_another_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let db = redb::Database::create(dir.path().join("t.redb")).unwrap();
        let check = |schema: Option<&str>| {
            let txn = db.begin_write().unwrap();
            {
                let mut meta = txn.open_table(META).unwrap();
                match schema {
                    Some(s) => meta.insert("schema", s.as_bytes()).map(drop).unwrap(),
                    None => meta.remove("schema").map(drop).unwrap(),
                }
            }
            txn.commit().unwrap();
            check_schema(&db.begin_read().unwrap(), TREE_SCHEMA)
        };
        assert!(check(Some(TREE_SCHEMA)).is_ok());
        let err = check(Some("1")).unwrap_err().to_string();
        assert_eq!(err, "schema 1, em reads 2");
        assert!(check(None).is_err());
    }
}
