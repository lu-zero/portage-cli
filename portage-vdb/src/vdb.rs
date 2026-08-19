//! Top-level VDB reader

use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};

use crate::Result;
use crate::category::{Categories, CategoriesIter, Category, PackageFilter, PackagesIter};
use crate::error::Error;
use crate::package::InstalledPackage;

/// The default VDB path
pub const DEFAULT_VDB_PATH: &str = "/var/db/pkg";

/// Reader for the Portage installed package database (VDB)
///
/// The VDB lives at `/var/db/pkg` and contains one subdirectory per
/// category, each containing one subdirectory per installed package.
///
/// # Example
///
/// ```no_run
/// use portage_vdb::Vdb;
///
/// let vdb = Vdb::open("/var/db/pkg").unwrap();
/// for cat in vdb.categories() {
///     for pkg in cat.packages() {
///         println!("{}", pkg);
///     }
/// }
/// ```
#[derive(Debug, Clone)]
pub struct Vdb {
    root: Utf8PathBuf,
}

/// Cap on [`Vdb::scan`]'s default worker count: past this, parallel opens
/// spend system time without cutting wall clock.
const DEFAULT_SCAN_JOBS_CAP: usize = 16;

fn default_scan_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, DEFAULT_SCAN_JOBS_CAP)
}

impl Vdb {
    /// Open the VDB at the given root path (typically `/var/db/pkg`)
    pub fn open(path: impl AsRef<Utf8Path>) -> Result<Self> {
        let path = path.as_ref();
        if path.is_dir() {
            Ok(Self {
                root: path.to_path_buf(),
            })
        } else {
            Err(Error::RootNotFound(path.to_path_buf()))
        }
    }

    /// Open the VDB at the default path (`/var/db/pkg`)
    pub fn open_default() -> Result<Self> {
        Self::open(DEFAULT_VDB_PATH)
    }

    /// The root path of this VDB
    pub fn root(&self) -> &Utf8Path {
        &self.root
    }

    /// Lazy iterator over all categories in the VDB
    pub fn categories(&self) -> Categories {
        Categories::new(self.root.clone())
    }

    /// Look up a single category by name
    pub fn category(&self, name: &str) -> Option<Category> {
        let path = self.root.join(name);
        if path.is_dir() {
            Some(Category::new(name.to_string(), path))
        } else {
            None
        }
    }

    /// Flat lazy iterator over every installed package across all categories
    pub fn packages(&self) -> AllPackages {
        AllPackages::new(self.categories())
    }

    /// Find which installed package owns a given file path
    ///
    /// Scans all packages' CONTENTS files. This is O(n) over installed packages.
    pub fn owner(&self, file_path: &Utf8Path) -> Option<InstalledPackage> {
        self.packages()
            .into_iter()
            .find(|pkg| pkg.owns(file_path).unwrap_or(false))
    }

    /// Run `f` over every installed package in parallel; return a stream of
    /// the `Some` results in completion order.
    ///
    /// Reading a file per package — CONTENTS for an ownership question, a
    /// metadata field for a property one — is the part of any VDB query that
    /// grows with the system, from a few hundred packages on a server to five
    /// figures on a large desktop. The per-package work is independent, so it
    /// splits across cores.
    ///
    /// Same shape as `portage_repo::source_parallel`: a bounded work channel
    /// feeding a fixed worker pool, results handed to the caller over an
    /// unbounded one so a worker never parks on the hand-off. Workers take
    /// the next package rather than a fixed slice each — CONTENTS sizes are
    /// heavily skewed, so equal chunks would leave one worker running long
    /// after the rest finished.
    ///
    /// Each worker keeps one `S` for the whole run and hands it to `f` — a
    /// read buffer, typically ([`InstalledPackage::contents_into`]), so a
    /// scan allocates per worker rather than per package. `()` when a scan
    /// needs no scratch.
    ///
    /// Drain the receiver until it disconnects; dropping it early stops the
    /// scan. Results arrive as they finish, so a caller needing a stable
    /// order sorts what it collects. `jobs` is the worker count, `None`
    /// picking a default from `available_parallelism`; `Some(1)` is the
    /// serial path, useful for A/B measurement.
    pub fn scan<T, S, F>(&self, jobs: Option<usize>, f: F) -> flume::Receiver<T>
    where
        T: Send + 'static,
        S: Default + Send + 'static,
        F: Fn(&mut S, &InstalledPackage) -> Option<T> + Send + Sync + 'static,
    {
        let (out_tx, out_rx) = flume::unbounded::<T>();
        let jobs = jobs.unwrap_or_else(default_scan_jobs).max(1);
        let vdb = self.clone();

        // Feed and join off-thread so the caller can start draining
        // immediately rather than after the whole VDB has been walked.
        std::thread::spawn(move || {
            let (work_tx, work_rx) = flume::bounded::<InstalledPackage>(jobs * 2);
            let f = Arc::new(f);
            let mut workers = Vec::with_capacity(jobs);
            for _ in 0..jobs {
                let work_rx = work_rx.clone();
                let out_tx = out_tx.clone();
                let f = Arc::clone(&f);
                workers.push(std::thread::spawn(move || {
                    let mut scratch = S::default();
                    while let Ok(pkg) = work_rx.recv() {
                        if let Some(found) = f(&mut scratch, &pkg) {
                            // Err means the caller dropped the receiver.
                            if out_tx.send(found).is_err() {
                                break;
                            }
                        }
                    }
                }));
            }
            drop(work_rx);
            drop(out_tx);

            for pkg in vdb.packages() {
                if work_tx.send(pkg).is_err() {
                    break;
                }
            }
            drop(work_tx);

            for worker in workers {
                let _ = worker.join();
            }
        });

        out_rx
    }

    /// Total number of installed packages
    pub fn len(&self) -> usize {
        self.categories()
            .into_iter()
            .map(|cat| cat.packages().into_iter().count())
            .sum()
    }

    /// Whether the VDB contains no installed packages
    pub fn is_empty(&self) -> bool {
        self.packages().into_iter().next().is_none()
    }
}

/// Lazy, composable flat iterator over all installed packages in a VDB
///
/// Produced by [`Vdb::packages`]. Iterates categories in sorted order, then
/// packages within each category sorted by CPV.
pub struct AllPackages {
    categories: Categories,
    filter: Option<Arc<PackageFilter>>,
}

/// Concrete iterator produced by [`AllPackages::into_iter`]
pub struct AllPackagesIter {
    current: Option<PackagesIter>,
    remaining: CategoriesIter,
    filter: Option<Arc<PackageFilter>>,
}

impl AllPackages {
    fn new(categories: Categories) -> Self {
        Self {
            categories,
            filter: None,
        }
    }

    /// Retain only packages matching the predicate
    pub fn filter<F>(mut self, f: F) -> Self
    where
        F: Fn(&InstalledPackage) -> bool + Send + Sync + 'static,
    {
        self.filter = Some(Arc::new(f));
        self
    }

    /// Collect all matching packages into a `Vec`
    pub fn collect_vec(self) -> Vec<InstalledPackage> {
        self.into_iter().collect()
    }
}

impl IntoIterator for AllPackages {
    type Item = InstalledPackage;
    type IntoIter = AllPackagesIter;

    fn into_iter(self) -> AllPackagesIter {
        let mut remaining = self.categories.into_iter();
        let current = remaining.next().map(|cat| cat.packages().into_iter());
        AllPackagesIter {
            current,
            remaining,
            filter: self.filter,
        }
    }
}

impl Iterator for AllPackagesIter {
    type Item = InstalledPackage;

    fn next(&mut self) -> Option<InstalledPackage> {
        loop {
            if let Some(current) = &mut self.current {
                match current.next() {
                    Some(pkg) => {
                        if self.filter.as_ref().is_some_and(|f| !f(&pkg)) {
                            continue;
                        }
                        return Some(pkg);
                    }
                    None => self.current = None,
                }
            } else {
                let cat = self.remaining.next()?;
                self.current = Some(cat.packages().into_iter());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_fake_vdb(dir: &std::path::Path) {
        let pkgs = [
            ("app-shells", "bash-5.3_p9-r2"),
            ("sys-libs", "glibc-2.43-r1"),
        ];

        for (cat, pf) in &pkgs {
            let pkg_dir = dir.join(cat).join(pf);
            fs::create_dir_all(&pkg_dir).unwrap();
            fs::write(pkg_dir.join("DESCRIPTION"), "test package").unwrap();
            fs::write(pkg_dir.join("EAPI"), "8").unwrap();
            fs::write(pkg_dir.join("SLOT"), "0").unwrap();
            fs::write(pkg_dir.join("CONTENTS"), "dir /usr\n").unwrap();
        }
    }

    fn open_temp_vdb(dir: &std::path::Path) -> Vdb {
        let root: Utf8PathBuf = dir.to_path_buf().try_into().unwrap();
        Vdb::open(root).unwrap()
    }

    #[test]
    fn open_and_iterate() {
        let tmp = tempfile::tempdir().unwrap();
        make_fake_vdb(tmp.path());

        let vdb = open_temp_vdb(tmp.path());
        let mut pkgs: Vec<_> = vdb.packages().into_iter().map(|p| p.to_string()).collect();
        pkgs.sort();
        assert_eq!(
            pkgs,
            vec!["app-shells/bash-5.3_p9-r2", "sys-libs/glibc-2.43-r1"]
        );
    }

    #[test]
    fn categories_names() {
        let tmp = tempfile::tempdir().unwrap();
        make_fake_vdb(tmp.path());

        let vdb = open_temp_vdb(tmp.path());
        let names: Vec<_> = vdb
            .categories()
            .into_iter()
            .map(|c| c.name().to_string())
            .collect();
        assert_eq!(names, vec!["app-shells", "sys-libs"]);
    }

    #[test]
    fn category_lookup() {
        let tmp = tempfile::tempdir().unwrap();
        make_fake_vdb(tmp.path());

        let vdb = open_temp_vdb(tmp.path());
        let cat = vdb.category("app-shells").unwrap();
        assert_eq!(cat.name(), "app-shells");
        assert!(vdb.category("nonexistent").is_none());
    }

    #[test]
    fn category_package_lookup() {
        let tmp = tempfile::tempdir().unwrap();
        make_fake_vdb(tmp.path());

        let vdb = open_temp_vdb(tmp.path());
        let cat = vdb.category("app-shells").unwrap();
        let pkg = cat.package("bash-5.3_p9-r2").unwrap();
        assert_eq!(pkg.pf(), "bash-5.3_p9-r2");
        assert!(cat.package("nonexistent-1.0").is_none());
    }

    #[test]
    fn category_packages_filtered() {
        let tmp = tempfile::tempdir().unwrap();
        make_fake_vdb(tmp.path());

        let vdb = open_temp_vdb(tmp.path());
        let pkgs = vdb.category("sys-libs").unwrap().packages().collect_vec();
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].pf(), "glibc-2.43-r1");
    }

    #[test]
    fn len_and_empty() {
        let tmp = tempfile::tempdir().unwrap();
        make_fake_vdb(tmp.path());

        let vdb = open_temp_vdb(tmp.path());
        assert_eq!(vdb.len(), 2);
        assert!(!vdb.is_empty());
    }

    #[test]
    fn empty_vdb() {
        let tmp = tempfile::tempdir().unwrap();
        let vdb = open_temp_vdb(tmp.path());
        assert!(vdb.is_empty());
        assert_eq!(vdb.len(), 0);
    }

    #[test]
    fn open_missing_root() {
        let result = Vdb::open("/nonexistent/path");
        assert!(result.is_err());
    }

    #[test]
    fn all_packages_filter() {
        let tmp = tempfile::tempdir().unwrap();
        make_fake_vdb(tmp.path());

        let vdb = open_temp_vdb(tmp.path());
        let pkgs = vdb
            .packages()
            .filter(|p| p.category() == "app-shells")
            .collect_vec();
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].pf(), "bash-5.3_p9-r2");
    }
}
