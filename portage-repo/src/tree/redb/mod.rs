//! A repository stored in redb, written by a sync that never makes a checkout
//!
//! Two files with separate lifecycles: the tree (`<location>.redb`: the git objects of one commit,
//! a path index and the md5 of every ebuild and eclass) and the md5-cache
//! (`<location>.md5-cache.redb`: `cat/PF` → entry text). Cache entries are checked against the
//! tree's digests, so the cache may lag behind or run ahead of the tree.

use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};

use super::TreeSource;

mod cache;
mod schema;
mod tree;
mod write;

pub use cache::RedbCache;
pub use schema::{ObjectKind, Oid};
pub use tree::RedbTree;
pub use write::{ObjectReader, PathChange, TreeItem, TreeUpdate, TreeWriter, UpdateSummary};

/// `<location>.redb`
pub(crate) fn tree_file(location: &Utf8Path) -> Utf8PathBuf {
    Utf8PathBuf::from(format!("{location}.redb"))
}

/// `<location>.md5-cache.redb`
pub(crate) fn cache_file(location: &Utf8Path) -> Utf8PathBuf {
    Utf8PathBuf::from(format!("{location}.md5-cache.redb"))
}

/// The tree in `<location>.redb`; only an absent database permits a checkout fallback.
fn open_tree(location: &Utf8Path) -> std::io::Result<Option<Arc<RedbTree>>> {
    let db = tree_file(location);
    match std::fs::symlink_metadata(&db) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
        Ok(_) => {}
    }
    RedbTree::open(db.clone())
        .map(|tree| Some(Arc::new(tree)))
        .map_err(|e| schema::in_file(&db, e))
}

/// [`open_tree`], as a [`TreeSource`]
pub(crate) fn tree_at(location: &Utf8Path) -> std::io::Result<Option<Arc<dyn TreeSource>>> {
    Ok(open_tree(location)?.map(|tree| tree as Arc<dyn TreeSource>))
}

/// The tree in `<location>.redb` and, when present, the entries in `<location>.md5-cache.redb`
/// (the checkout's otherwise); `None` only when there is no tree database
pub(crate) fn storage_at(location: &Utf8Path) -> std::io::Result<Option<super::Storage>> {
    let Some(tree) = open_tree(location)? else {
        return Ok(None);
    };
    let cache_db = cache_file(location);
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
    Ok(Some(storage))
}
