//! Abstract md5-cache storage for a [`crate::Repository`]
//!
//! The PMS tree layout stays file-backed; **metadata cache** is a separate
//! store that may be a directory (primary in-tree or user-side secondary) or
//! an in-memory map (tests). Entry path layout
//! (`<category>/<PN>-<PVR>`) lives only in [`DirMetadataCache`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicI8, Ordering};
use std::sync::{Arc, Mutex};

use camino::{Utf8Path, Utf8PathBuf};
use portage_atom::Cpv;
use portage_metadata::CacheEntry;

use crate::error::{Error, Result};
use crate::repo::util;

/// Per-CPV md5-cache store
///
/// Directory backends use the PMS on-disk layout under a root; memory
/// backends key by [`Cpv`] directly.
pub trait MetadataCache: Send + Sync {
    /// Read a cache entry, or `Ok(None)` if missing
    fn get(&self, cpv: &Cpv) -> Result<Option<CacheEntry>>;

    /// Write (or replace) a cache entry
    fn put(&self, cpv: &Cpv, entry: &CacheEntry) -> Result<()>;

    /// Whether this store might contain entries (dir exists / map non-empty)
    fn is_populated(&self) -> bool;
}

/// Directory-backed cache: `<root>/<category>/<PN>-<PVR>`
///
/// [`MetadataCache::is_populated`] is sticky: an empty secondary is probed
/// once (one `read_dir`), then skipped for the rest of the process so
/// primary-miss paths do not pay a per-CPV `openat` ENOENT into an unused
/// user cache. [`MetadataCache::put`] marks the store non-empty so later
/// reads hit the new files.
#[derive(Debug)]
pub struct DirMetadataCache {
    root: Utf8PathBuf,
    /// `-1` unknown, `0` empty/missing, `1` has at least one entry (or a put)
    populated: AtomicI8,
}

impl Clone for DirMetadataCache {
    fn clone(&self) -> Self {
        Self {
            root: self.root.clone(),
            // Fresh probe state — clones are rare; do not share sticky false
            // across independent roots by accident if root were remapped.
            populated: AtomicI8::new(self.populated.load(Ordering::Relaxed)),
        }
    }
}

impl DirMetadataCache {
    /// Create a cache rooted at `root` (created on first `put` if needed)
    pub fn new(root: impl Into<Utf8PathBuf>) -> Self {
        Self {
            root: root.into(),
            populated: AtomicI8::new(-1),
        }
    }

    /// Filesystem root of this cache
    pub fn root(&self) -> &Utf8Path {
        &self.root
    }

    /// On-disk path for `cpv` under this root
    pub fn entry_path(&self, cpv: &Cpv) -> Utf8PathBuf {
        entry_under(&self.root, cpv)
    }

    fn probe_nonempty(root: &Utf8Path) -> bool {
        let Ok(mut rd) = std::fs::read_dir(root.as_std_path()) else {
            return false;
        };
        rd.next().is_some()
    }
}

impl MetadataCache for DirMetadataCache {
    fn get(&self, cpv: &Cpv) -> Result<Option<CacheEntry>> {
        let path = self.entry_path(cpv);
        match std::fs::read_to_string(path.as_std_path()) {
            Ok(contents) => Ok(Some(CacheEntry::parse(&contents)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(util::io_err(&path, e)),
        }
    }

    fn put(&self, cpv: &Cpv, entry: &CacheEntry) -> Result<()> {
        let path = self.entry_path(cpv);
        let parent = path
            .parent()
            .ok_or_else(|| Error::InvalidRepository(path.clone().into_std_path_buf()))?;
        std::fs::create_dir_all(parent.as_std_path()).map_err(|e| util::io_err(parent, e))?;
        // Atomic replace: write temp then rename (same as regen_cache).
        let file_name = path
            .file_name()
            .ok_or_else(|| Error::InvalidRepository(path.clone().into_std_path_buf()))?;
        let tmp = parent.join(format!("{file_name}.tmp"));
        std::fs::write(tmp.as_std_path(), entry.serialize()).map_err(|e| util::io_err(&tmp, e))?;
        std::fs::rename(tmp.as_std_path(), path.as_std_path())
            .map_err(|e| util::io_err(&path, e))?;
        self.populated.store(1, Ordering::Relaxed);
        Ok(())
    }

    fn is_populated(&self) -> bool {
        match self.populated.load(Ordering::Relaxed) {
            1 => true,
            0 => false,
            _ => {
                let nonempty = Self::probe_nonempty(&self.root);
                self.populated
                    .store(if nonempty { 1 } else { 0 }, Ordering::Relaxed);
                nonempty
            }
        }
    }
}

/// In-memory cache for tests and ephemeral secondary stores
#[derive(Debug, Default)]
pub struct MemoryMetadataCache {
    map: Mutex<HashMap<Cpv, CacheEntry>>,
}

impl MemoryMetadataCache {
    /// Empty in-memory cache
    pub fn new() -> Self {
        Self::default()
    }

    /// Wrap as an [`Arc`] for builder / `Repository` fields
    pub fn shared(self) -> Arc<Self> {
        Arc::new(self)
    }
}

impl MetadataCache for MemoryMetadataCache {
    fn get(&self, cpv: &Cpv) -> Result<Option<CacheEntry>> {
        let guard = self
            .map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(guard.get(cpv).cloned())
    }

    fn put(&self, cpv: &Cpv, entry: &CacheEntry) -> Result<()> {
        let mut guard = self
            .map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.insert(cpv.clone(), entry.clone());
        Ok(())
    }

    fn is_populated(&self) -> bool {
        let guard = self
            .map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        !guard.is_empty()
    }
}

/// PMS md5-cache relative path: `<category>/<PN>-<PVR>`
fn entry_under(root: &Utf8Path, cpv: &Cpv) -> Utf8PathBuf {
    root.join(cpv.cpn.category.as_str())
        .join(format!("{}-{}", cpv.cpn.package, cpv.version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use portage_atom::Cpv;

    fn sample_entry() -> CacheEntry {
        CacheEntry::parse(
            "\
EAPI=8
DESCRIPTION=test
SLOT=0
",
        )
        .unwrap()
    }

    #[test]
    fn empty_dir_is_not_populated() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        // Root exists but has no entries — must not thrash secondary lookups.
        let cache = DirMetadataCache::new(&root);
        assert!(!cache.is_populated());
        // Sticky: still false on second call without a put.
        assert!(!cache.is_populated());
    }

    #[test]
    fn dir_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cache = DirMetadataCache::new(root);
        let cpv = Cpv::parse("cat/pkg-1.0").unwrap();
        assert!(cache.get(&cpv).unwrap().is_none());
        assert!(!cache.is_populated());
        cache.put(&cpv, &sample_entry()).unwrap();
        let got = cache.get(&cpv).unwrap().unwrap();
        assert_eq!(got.metadata.description, "test");
        assert!(cache.is_populated());
    }

    #[test]
    fn memory_round_trip() {
        let cache = MemoryMetadataCache::new();
        let cpv = Cpv::parse("cat/pkg-1.0").unwrap();
        cache.put(&cpv, &sample_entry()).unwrap();
        assert_eq!(
            cache.get(&cpv).unwrap().unwrap().metadata.description,
            "test"
        );
        assert!(cache.is_populated());
    }

    #[test]
    fn entry_under_layout() {
        let p = entry_under(
            Utf8Path::new("/c"),
            &Cpv::parse("sys-devel/gcc-15.2.1").unwrap(),
        );
        assert_eq!(p.as_str(), "/c/sys-devel/gcc-15.2.1");
    }
}
