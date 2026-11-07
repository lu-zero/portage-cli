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

pub use cache::RedbCache;
pub use tree::RedbTree;

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
