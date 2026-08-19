use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};

use super::package::Package;
use super::util;

type CategoryFilter = dyn Fn(&Category) -> bool + Send + Sync;
type PackageFilter = dyn Fn(&Package) -> bool + Send + Sync;

/// A category directory within an ebuild repository
///
/// Represents a directory such as `dev-lang/` containing package directories.
///
/// See [PMS 4](https://projects.gentoo.org/pms/9/pms.html#tree-layout).
#[derive(Debug, Clone)]
pub struct Category {
    name: String,
    path: Utf8PathBuf,
}

impl Category {
    pub(crate) fn new(name: String, path: Utf8PathBuf) -> Self {
        Self { name, path }
    }

    /// The category name (e.g. `dev-lang`)
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Absolute path to the category directory
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }

    /// Whether the category directory exists on disk
    pub fn exists(&self) -> bool {
        self.path.is_dir()
    }

    /// Lazy iterator over all packages in this category
    pub fn packages(&self) -> Packages {
        Packages::new(self.path.clone(), self.name.clone())
    }

    /// Look up a specific package by name
    pub fn package(&self, name: &str) -> Option<Package> {
        let path = self.path.join(name);
        if path.is_dir() {
            Some(Package::new(&self.name, name, path))
        } else {
            None
        }
    }
}

/// Lazy, composable package discovery within a category directory
///
/// Produced by [`Category::packages`]. Nothing is read until the iterator
/// is driven. Use `.filter()` to restrict and `.collect_vec()` to materialise.
pub struct Packages {
    path: Utf8PathBuf,
    category: String,
    filter: Option<Arc<PackageFilter>>,
}

/// Concrete iterator produced by [`Packages::into_iter`]
pub struct PackagesIter {
    entries: std::vec::IntoIter<Package>,
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

    /// Retain only packages matching the predicate
    pub fn filter<F>(mut self, f: F) -> Self
    where
        F: Fn(&Package) -> bool + Send + Sync + 'static,
    {
        self.filter = Some(Arc::new(f));
        self
    }

    /// Collect all matching packages into a sorted `Vec`
    pub fn collect_vec(self) -> Vec<Package> {
        self.into_iter().collect()
    }
}

impl IntoIterator for Packages {
    type Item = Package;
    type IntoIter = PackagesIter;

    fn into_iter(self) -> PackagesIter {
        let Ok(entries) = std::fs::read_dir(&self.path) else {
            return PackagesIter {
                entries: Vec::new().into_iter(),
                filter: self.filter,
            };
        };

        let mut packages = Vec::new();
        for entry in entries {
            let Ok(entry) = entry else { continue };
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            // Follow symlinks — some overlays (notably crossdev) symlink
            // their package dirs at the gentoo originals.
            let is_dir = if ft.is_dir() {
                true
            } else if ft.is_symlink() {
                std::fs::metadata(entry.path())
                    .map(|m| m.is_dir())
                    .unwrap_or(false)
            } else {
                false
            };
            if !is_dir {
                continue;
            }
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            if name.starts_with('.') || name == "CVS" {
                continue;
            }
            let Ok(path) = entry.path().try_into() else {
                continue;
            };
            packages.push(Package::new(&self.category, &name, path));
        }
        packages.sort_by(|a, b| a.name().cmp(b.name()));
        PackagesIter {
            entries: packages.into_iter(),
            filter: self.filter,
        }
    }
}

impl Iterator for PackagesIter {
    type Item = Package;

    fn next(&mut self) -> Option<Package> {
        loop {
            let pkg = self.entries.next()?;
            match &self.filter {
                Some(f) if !f(&pkg) => continue,
                _ => return Some(pkg),
            }
        }
    }
}

// --- Categories ---

/// Lazy, composable category discovery over an ebuild repository
///
/// Produced by [`Repository::categories`](crate::Repository::categories).
/// Nothing is read until the iterator is driven.
pub struct Categories {
    file: Utf8PathBuf,
    repo: Utf8PathBuf,
    filter: Option<Arc<CategoryFilter>>,
}

/// Concrete iterator produced by [`Categories::into_iter`]
pub struct CategoriesIter {
    lines: std::vec::IntoIter<String>,
    repo: Utf8PathBuf,
    filter: Option<Arc<CategoryFilter>>,
}

impl Categories {
    pub(crate) fn new(file: Utf8PathBuf, repo: Utf8PathBuf) -> Self {
        Self {
            file,
            repo,
            filter: None,
        }
    }

    /// Retain only categories matching the predicate
    pub fn filter<F>(mut self, f: F) -> Self
    where
        F: Fn(&Category) -> bool + Send + Sync + 'static,
    {
        self.filter = Some(Arc::new(f));
        self
    }

    /// Collect all matching categories into a `Vec`
    pub fn collect_vec(self) -> Vec<Category> {
        self.into_iter().collect()
    }
}

impl IntoIterator for Categories {
    type Item = Category;
    type IntoIter = CategoriesIter;

    fn into_iter(self) -> CategoriesIter {
        let lines = util::read_lines(&self.file).unwrap_or_default();
        CategoriesIter {
            lines: lines.into_iter(),
            repo: self.repo,
            filter: self.filter,
        }
    }
}

impl Iterator for CategoriesIter {
    type Item = Category;

    fn next(&mut self) -> Option<Category> {
        loop {
            let name = self.lines.next()?;
            let path = self.repo.join(&name);
            let cat = Category::new(name, path);
            match &self.filter {
                Some(f) if !f(&cat) => continue,
                _ => return Some(cat),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("metadata")).unwrap();
        std::fs::write(root.join("metadata/layout.conf"), "masters =\n").unwrap();
        std::fs::create_dir_all(root.join("profiles")).unwrap();
        std::fs::write(root.join("profiles/repo_name"), "test\n").unwrap();
        tmp
    }

    #[test]
    fn packages_lists_subdirectories() {
        let tmp = setup_repo();
        let cat_dir = tmp.path().join("dev-util");
        std::fs::create_dir_all(cat_dir.join("foo")).unwrap();
        std::fs::create_dir_all(cat_dir.join("bar")).unwrap();
        std::fs::write(cat_dir.join("README"), "not a package").unwrap();

        let cat = Category::new("dev-util".into(), cat_dir.try_into().unwrap());
        let pkgs = cat.packages().collect_vec();
        let names: Vec<&str> = pkgs.iter().map(|p| p.name()).collect();
        assert_eq!(names, vec!["bar", "foo"]);
    }

    #[test]
    fn packages_skips_dotfiles() {
        let tmp = setup_repo();
        let cat_dir = tmp.path().join("dev-util");
        std::fs::create_dir_all(cat_dir.join("foo")).unwrap();
        std::fs::create_dir_all(cat_dir.join(".hidden")).unwrap();

        let cat = Category::new("dev-util".into(), cat_dir.try_into().unwrap());
        let pkgs = cat.packages().collect_vec();
        let names: Vec<&str> = pkgs.iter().map(|p| p.name()).collect();
        assert_eq!(names, vec!["foo"]);
    }

    #[test]
    fn package_lookup_existing() {
        let tmp = setup_repo();
        let cat_dir = tmp.path().join("dev-util");
        std::fs::create_dir_all(cat_dir.join("foo")).unwrap();

        let cat = Category::new("dev-util".into(), cat_dir.try_into().unwrap());
        assert!(cat.package("foo").is_some());
        assert!(cat.package("nonexistent").is_none());
    }

    #[test]
    fn exists_checks_directory() {
        let tmp = setup_repo();
        let cat_dir = tmp.path().join("dev-util");
        std::fs::create_dir_all(&cat_dir).unwrap();

        let cat = Category::new("dev-util".into(), cat_dir.try_into().unwrap());
        assert!(cat.exists());

        let cat2 = Category::new(
            "missing".into(),
            tmp.path().join("no-such-dir").try_into().unwrap(),
        );
        assert!(!cat2.exists());
    }

    #[test]
    fn packages_filter() {
        let tmp = setup_repo();
        let cat_dir = tmp.path().join("dev-util");
        std::fs::create_dir_all(cat_dir.join("foo")).unwrap();
        std::fs::create_dir_all(cat_dir.join("bar")).unwrap();

        let cat = Category::new("dev-util".into(), cat_dir.try_into().unwrap());
        let pkgs = cat.packages().filter(|p| p.name() == "foo").collect_vec();
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name(), "foo");
    }
}
