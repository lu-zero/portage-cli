//! Run-scoped memoization for VDB flat-file fields.
//!
//! The cache is shared by package handles created from the same live [`Vdb`](crate::Vdb)
//! root. A small process registry keeps only weak references to those run caches, so
//! parsed values disappear with the last handle instead of living for the process.
//! Registration and unregistration clear one package's fields; they never scan the
//! whole cache. External writers retain the existing policy: a live cache is not
//! invalidated until the owning VDB performs a write.

use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::sync::{
    Arc, Mutex, MutexGuard, OnceLock, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard, Weak,
};

use camino::{Utf8Path, Utf8PathBuf};

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(crate) enum TypedField {
    Description,
    Eapi,
    Slot,
    SlotRaw,
    Repository,
    UseFlags,
    Iuse,
    BuildTime,
    Size,
    Counter,
    Keywords,
    License,
    Homepage,
    Depend,
    Rdepend,
    Bdepend,
    Pdepend,
    Idepend,
}

type CachedValue = Arc<dyn Any + Send + Sync>;

/// Cached fields for one VDB package directory.
pub(crate) struct PackageFields {
    raw: RwLock<HashMap<String, Option<String>>>,
    typed: RwLock<HashMap<TypedField, CachedValue>>,
}

impl Default for PackageFields {
    fn default() -> Self {
        Self {
            raw: RwLock::new(HashMap::new()),
            typed: RwLock::new(HashMap::new()),
        }
    }
}

impl PackageFields {
    /// Return a raw field value, caching both present and confirmed-absent files.
    pub(crate) fn get_raw(
        &self,
        name: &str,
        fetch: impl FnOnce() -> std::io::Result<Option<String>>,
    ) -> std::io::Result<Option<String>> {
        if let Some(cached) = read_lock(&self.raw).get(name).cloned() {
            return Ok(cached);
        }
        let value = fetch()?;
        write_lock(&self.raw)
            .entry(name.to_owned())
            .or_insert_with(|| value.clone());
        Ok(value)
    }

    /// Parse a typed field once per package cache and clone the parsed value on hits.
    pub(crate) fn get_or_parse<T, E>(
        &self,
        field: TypedField,
        fetch: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E>
    where
        T: Clone + Send + Sync + 'static,
    {
        if let Some(cached) = read_lock(&self.typed)
            .get(&field)
            .and_then(|value| value.as_ref().downcast_ref::<T>())
            .cloned()
        {
            return Ok(cached);
        }
        let value = fetch()?;
        write_lock(&self.typed)
            .entry(field)
            .or_insert_with(|| Arc::new(value.clone()) as CachedValue);
        Ok(value)
    }

    fn clear(&self) {
        write_lock(&self.raw).clear();
        write_lock(&self.typed).clear();
    }
}

impl fmt::Debug for PackageFields {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PackageFields").finish_non_exhaustive()
    }
}

/// A run-scoped cache shared by all package handles from one VDB root.
#[derive(Clone)]
pub(crate) struct FieldCache {
    packages: Arc<Mutex<HashMap<Utf8PathBuf, Arc<PackageFields>>>>,
}

type Registry = Mutex<HashMap<Utf8PathBuf, Weak<FieldCache>>>;

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_registry() -> MutexGuard<'static, HashMap<Utf8PathBuf, Weak<FieldCache>>> {
    registry().lock().unwrap_or_else(PoisonError::into_inner)
}

fn read_lock<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

fn write_lock<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

impl FieldCache {
    fn new() -> Self {
        Self {
            packages: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Return the live cache for `root`, creating one when no run currently holds it.
    pub(crate) fn for_root(root: &Utf8Path) -> Arc<Self> {
        let mut entries = lock_registry();
        if let Some(cache) = entries.get(root).and_then(|weak| weak.upgrade()) {
            return cache;
        }
        let cache = Arc::new(Self::new());
        entries.insert(root.to_owned(), Arc::downgrade(&cache));
        cache
    }

    pub(crate) fn package(&self, path: &Utf8Path) -> Arc<PackageFields> {
        let mut packages = self.packages.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(
            packages
                .entry(path.to_owned())
                .or_insert_with(|| Arc::new(PackageFields::default())),
        )
    }

    pub(crate) fn invalidate_package(&self, path: &Utf8Path) {
        // Clear in place. Removing the `Arc` lets a handle that refetches
        // after the first clear keep a value the next invalidate never sees.
        let packages = self.packages.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(fields) = packages.get(path) {
            fields.clear();
        }
    }
}

impl fmt::Debug for FieldCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FieldCache").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_fields_parse_once_per_package_cache() {
        let fields = PackageFields::default();
        let mut parses = 0;
        for _ in 0..2 {
            let value = fields
                .get_or_parse(TypedField::UseFlags, || {
                    parses += 1;
                    Ok::<_, ()>(vec!["readline".to_owned()])
                })
                .unwrap();
            assert_eq!(value, ["readline"]);
        }
        assert_eq!(parses, 1);
    }

    #[test]
    fn raw_fields_cache_confirmed_absence() {
        let fields = PackageFields::default();
        let mut reads = 0;
        for _ in 0..2 {
            let value = fields
                .get_raw("OPTIONAL", || {
                    reads += 1;
                    Ok::<_, std::io::Error>(None)
                })
                .unwrap();
            assert!(value.is_none());
        }
        assert_eq!(reads, 1);
    }

    #[test]
    #[ignore = "explicit performance benchmark"]
    fn benchmark_parsed_metadata_reuse() {
        use std::hint::black_box;
        use std::time::{Duration, Instant};

        use crate::{InstalledPackage, Vdb};

        fn read_parsed(pkg: &InstalledPackage) {
            for _ in 0..2 {
                black_box(pkg.eapi().unwrap());
                black_box(pkg.slot().unwrap());
                black_box(pkg.use_flags().unwrap());
                black_box(pkg.iuse().unwrap());
                black_box(pkg.rdepend().unwrap());
            }
        }

        for count in [64usize, 128, 256] {
            let tmp = tempfile::tempdir().unwrap();
            let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
            let vdb_root = root.join("var/db/pkg");
            for i in 0..count {
                let dir = vdb_root.join("bench").join(format!("pkg{i:03}-1.0"));
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join("EAPI"), "8\n").unwrap();
                std::fs::write(dir.join("SLOT"), "0\n").unwrap();
                std::fs::write(dir.join("USE"), "foo bar\n").unwrap();
                std::fs::write(dir.join("IUSE"), "+foo -bar\n").unwrap();
                std::fs::write(dir.join("RDEPEND"), "sys-libs/foo\n").unwrap();
            }
            let vdb = Vdb::open(&vdb_root).unwrap();
            for pkg in vdb.packages() {
                read_parsed(&pkg);
            }
            let mut samples = Vec::new();
            for _ in 0..3 {
                let start = Instant::now();
                for _ in 0..2 {
                    let scan = Vdb::open(&vdb_root).unwrap();
                    for pkg in scan.packages() {
                        read_parsed(&pkg);
                    }
                }
                samples.push(start.elapsed());
            }
            samples.sort();
            let median: Duration = samples[samples.len() / 2];
            println!("parsed-metadata {count}: {median:?}");
        }
    }
}
