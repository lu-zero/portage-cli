//! VDB category directory, lazy category iterator, and lazy package iterator.

use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};
use portage_atom::{Cpv, Pf};

use crate::package::InstalledPackage;

pub(crate) type PackageFilter = dyn Fn(&InstalledPackage) -> bool + Send + Sync;
type CategoryFilter = dyn Fn(&Category) -> bool + Send + Sync;

/// A category directory within the VDB (e.g. `/var/db/pkg/app-shells`).
#[derive(Debug, Clone)]
pub struct Category {
    name: String,
    path: Utf8PathBuf,
}

impl Category {
    pub(crate) fn new(name: String, path: Utf8PathBuf) -> Self {
        Self { name, path }
    }

    /// The category name (e.g. `app-shells`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Absolute path to the category directory.
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }

    /// Whether the category directory still exists on disk.
    pub fn exists(&self) -> bool {
        self.path.is_dir()
    }

    /// Lazy iterator over all installed packages in this category, sorted by CPV.
    pub fn packages(&self) -> Packages {
        Packages::new(self.path.clone(), self.name.clone())
    }

    /// Look up a specific installed package by PF (e.g. `bash-5.3_p9-r2`).
    pub fn package(&self, pf: &str) -> Option<InstalledPackage> {
        let path = self.path.join(pf);
        if !path.is_dir() {
            return None;
        }
        let cpv = parse_cpv(&self.name, pf)?;
        Some(InstalledPackage::from_dir(&path, cpv))
    }
}

/// Lazy, composable package discovery within a VDB category directory.
///
/// Produced by [`Category::packages`]. Nothing is read until the iterator
/// is driven.
pub struct Packages {
    path: Utf8PathBuf,
    category: String,
    filter: Option<Arc<PackageFilter>>,
}

/// Concrete iterator produced by [`Packages::into_iter`].
pub struct PackagesIter {
    entries: std::vec::IntoIter<InstalledPackage>,
    filter: Option<Arc<PackageFilter>>,
}

impl Packages {
    fn new(path: Utf8PathBuf, category: String) -> Self {
        Self {
            path,
            category,
            filter: None,
        }
    }

    /// Retain only packages matching the predicate.
    pub fn filter<F>(mut self, f: F) -> Self
    where
        F: Fn(&InstalledPackage) -> bool + Send + Sync + 'static,
    {
        self.filter = Some(Arc::new(f));
        self
    }

    /// Collect all matching packages into a sorted `Vec`.
    pub fn collect_vec(self) -> Vec<InstalledPackage> {
        self.into_iter().collect()
    }
}

impl IntoIterator for Packages {
    type Item = InstalledPackage;
    type IntoIter = PackagesIter;

    fn into_iter(self) -> PackagesIter {
        let Ok(entries) = std::fs::read_dir(&self.path) else {
            return PackagesIter {
                entries: Vec::new().into_iter(),
                filter: self.filter,
            };
        };

        let mut packages: Vec<InstalledPackage> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let path = e.path();
                if !path.is_dir() {
                    return None;
                }
                let file_name = e.file_name();
                let pf = file_name.to_str()?;
                if pf.starts_with('.') {
                    return None;
                }
                let cpv = parse_cpv(&self.category, pf)?;
                let utf8_path: Utf8PathBuf = path.try_into().ok()?;
                Some(InstalledPackage::from_dir(&utf8_path, cpv))
            })
            .collect();
        packages.sort_by(|a, b| a.cpv().cmp(b.cpv()));
        PackagesIter {
            entries: packages.into_iter(),
            filter: self.filter,
        }
    }
}

impl Iterator for PackagesIter {
    type Item = InstalledPackage;

    fn next(&mut self) -> Option<InstalledPackage> {
        loop {
            let pkg = self.entries.next()?;
            match &self.filter {
                Some(f) if !f(&pkg) => continue,
                _ => return Some(pkg),
            }
        }
    }
}

/// Lazy, composable category discovery over a VDB root.
///
/// Produced by [`Vdb::categories`](crate::Vdb::categories).
/// Nothing is read until the iterator is driven.
///
/// ```no_run
/// use portage_vdb::Vdb;
///
/// let vdb = Vdb::open("/var/db/pkg").unwrap();
/// for cat in vdb.categories().filter(|c| c.name().starts_with("dev-")) {
///     println!("{}", cat.name());
/// }
/// ```
pub struct Categories {
    root: Utf8PathBuf,
    filter: Option<Arc<CategoryFilter>>,
}

/// Concrete iterator produced by [`Categories::into_iter`].
pub struct CategoriesIter {
    entries: std::vec::IntoIter<Category>,
    filter: Option<Arc<CategoryFilter>>,
}

impl Categories {
    pub(crate) fn new(root: Utf8PathBuf) -> Self {
        Self { root, filter: None }
    }

    /// Retain only categories matching the predicate.
    pub fn filter<F>(mut self, f: F) -> Self
    where
        F: Fn(&Category) -> bool + Send + Sync + 'static,
    {
        self.filter = Some(Arc::new(f));
        self
    }

    /// Collect all matching categories into a `Vec`.
    pub fn collect_vec(self) -> Vec<Category> {
        self.into_iter().collect()
    }
}

impl IntoIterator for Categories {
    type Item = Category;
    type IntoIter = CategoriesIter;

    fn into_iter(self) -> CategoriesIter {
        let mut entries: Vec<Category> = std::fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let path = e.path();
                if !path.is_dir() {
                    return None;
                }
                let name = path.file_name()?.to_str()?.to_string();
                let utf8_path: Utf8PathBuf = path.try_into().ok()?;
                Some(Category::new(name, utf8_path))
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        CategoriesIter {
            entries: entries.into_iter(),
            filter: self.filter,
        }
    }
}

impl Iterator for CategoriesIter {
    type Item = Category;

    fn next(&mut self) -> Option<Category> {
        loop {
            let cat = self.entries.next()?;
            match &self.filter {
                Some(f) if !f(&cat) => continue,
                _ => return Some(cat),
            }
        }
    }
}

fn parse_cpv(category: &str, pf_str: &str) -> Option<Cpv> {
    let pf = Pf::parse(pf_str).ok()?;
    Some(Cpv::from_parts(category, pf.package.as_ref(), pf.version))
}
