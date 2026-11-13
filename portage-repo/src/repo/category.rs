use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};

use super::package::Package;
use crate::tree::{EntryKind, TreeSource, tree_lines};

type CategoryFilter = dyn Fn(&Category) -> bool + Send + Sync;
type PackageFilter = dyn Fn(&Package) -> bool + Send + Sync;

/// A category directory within an ebuild repository
///
/// Represents a directory such as `dev-lang/` containing package directories.
///
/// See [PMS 4](https://projects.gentoo.org/pms/9/pms.html#tree-layout).
#[derive(Clone)]
pub struct Category {
    name: String,
    path: Utf8PathBuf,
    tree: Arc<dyn TreeSource>,
}

impl std::fmt::Debug for Category {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Category")
            .field("name", &self.name)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl Category {
    /// `path` is the checkout's directory for `name`; reads go through `tree`
    pub(crate) fn new(name: String, path: Utf8PathBuf, tree: Arc<dyn TreeSource>) -> Self {
        Self { name, path, tree }
    }

    /// The category name (e.g. `dev-lang`)
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Absolute path to the category directory
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }

    /// Whether the category directory exists in the repository
    pub fn exists(&self) -> bool {
        is_dir(&*self.tree, Utf8Path::new(&self.name))
    }

    /// Lazy iterator over all packages in this category
    pub fn packages(&self) -> Packages {
        Packages::new(self.clone())
    }

    /// The names of the package directories, unsorted
    ///
    /// For scanning every name without building a [`Package`] for each; [`Self::package`] builds
    /// the ones that matter.
    pub fn package_names(&self) -> Vec<String> {
        // `list` follows symlinks: some overlays (notably crossdev) symlink their package dirs
        // at the gentoo originals.
        self.tree
            .list(Utf8Path::new(&self.name))
            .ok()
            .flatten()
            .unwrap_or_default()
            .into_iter()
            .filter(|(name, kind)| {
                *kind == EntryKind::Dir && !name.starts_with('.') && name != "CVS"
            })
            .map(|(name, _)| name)
            .collect()
    }

    /// Look up a specific package by name
    pub fn package(&self, name: &str) -> Option<Package> {
        is_dir(&*self.tree, &Utf8Path::new(&self.name).join(name)).then(|| self.child(name))
    }

    fn child(&self, name: &str) -> Package {
        Package::new(&self.name, name, self.path.join(name), self.tree.clone())
    }
}

fn is_dir(tree: &dyn TreeSource, rel: &Utf8Path) -> bool {
    matches!(tree.kind(rel), Ok(Some(EntryKind::Dir)))
}

/// Lazy, composable package discovery within a category directory
///
/// Produced by [`Category::packages`]. Nothing is read until the iterator
/// is driven. Use `.filter()` to restrict and `.collect_vec()` to materialise.
pub struct Packages {
    category: Category,
    filter: Option<Arc<PackageFilter>>,
}

/// Concrete iterator produced by [`Packages::into_iter`]
pub struct PackagesIter {
    entries: std::vec::IntoIter<Package>,
    filter: Option<Arc<PackageFilter>>,
}

impl Packages {
    fn new(category: Category) -> Self {
        Self {
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
        let cat = &self.category;
        let mut packages: Vec<Package> = cat
            .package_names()
            .iter()
            .map(|name| cat.child(name))
            .collect();
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
    repo: Utf8PathBuf,
    tree: Arc<dyn TreeSource>,
    filter: Option<Arc<CategoryFilter>>,
}

/// Concrete iterator produced by [`Categories::into_iter`]
pub struct CategoriesIter {
    lines: std::vec::IntoIter<String>,
    repo: Utf8PathBuf,
    tree: Arc<dyn TreeSource>,
    filter: Option<Arc<CategoryFilter>>,
}

impl Categories {
    /// The categories `profiles/categories` lists; `repo` is the checkout root
    pub(crate) fn new(repo: Utf8PathBuf, tree: Arc<dyn TreeSource>) -> Self {
        Self {
            repo,
            tree,
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
        let lines = tree_lines(&*self.tree, "profiles/categories").unwrap_or_default();
        CategoriesIter {
            lines: lines.into_iter(),
            repo: self.repo,
            tree: self.tree,
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
            let cat = Category::new(name, path, self.tree.clone());
            match &self.filter {
                Some(f) if !f(&cat) => continue,
                _ => return Some(cat),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    fn fs(tmp: &tempfile::TempDir) -> std::sync::Arc<dyn crate::tree::TreeSource> {
        let root = camino::Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        std::sync::Arc::new(crate::tree::FsTree::new(root))
    }

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

        let cat = Category::new("dev-util".into(), cat_dir.try_into().unwrap(), fs(&tmp));
        let pkgs = cat.packages().collect_vec();
        let names: Vec<&str> = pkgs.iter().map(|p| p.name()).collect();
        assert_eq!(names, vec!["bar", "foo"]);
    }

    #[test]
    fn package_names_lists_package_directories_only() {
        let tmp = setup_repo();
        let cat_dir = tmp.path().join("dev-util");
        for dir in ["foo", "bar", ".hidden", "CVS"] {
            std::fs::create_dir_all(cat_dir.join(dir)).unwrap();
        }
        std::fs::write(cat_dir.join("README"), "not a package").unwrap();

        let cat = Category::new("dev-util".into(), cat_dir.try_into().unwrap(), fs(&tmp));
        let mut names = cat.package_names();
        names.sort();
        assert_eq!(names, vec!["bar", "foo"]);
    }

    #[test]
    fn packages_skips_dotfiles() {
        let tmp = setup_repo();
        let cat_dir = tmp.path().join("dev-util");
        std::fs::create_dir_all(cat_dir.join("foo")).unwrap();
        std::fs::create_dir_all(cat_dir.join(".hidden")).unwrap();

        let cat = Category::new("dev-util".into(), cat_dir.try_into().unwrap(), fs(&tmp));
        let pkgs = cat.packages().collect_vec();
        let names: Vec<&str> = pkgs.iter().map(|p| p.name()).collect();
        assert_eq!(names, vec!["foo"]);
    }

    #[test]
    fn package_lookup_existing() {
        let tmp = setup_repo();
        let cat_dir = tmp.path().join("dev-util");
        std::fs::create_dir_all(cat_dir.join("foo")).unwrap();

        let cat = Category::new("dev-util".into(), cat_dir.try_into().unwrap(), fs(&tmp));
        assert!(cat.package("foo").is_some());
        assert!(cat.package("nonexistent").is_none());
    }

    #[test]
    fn exists_checks_directory() {
        let tmp = setup_repo();
        let cat_dir = tmp.path().join("dev-util");
        std::fs::create_dir_all(&cat_dir).unwrap();

        let cat = Category::new("dev-util".into(), cat_dir.try_into().unwrap(), fs(&tmp));
        assert!(cat.exists());

        let cat2 = Category::new(
            "missing".into(),
            tmp.path().join("no-such-dir").try_into().unwrap(),
            fs(&tmp),
        );
        assert!(!cat2.exists());
    }

    #[test]
    fn packages_filter() {
        let tmp = setup_repo();
        let cat_dir = tmp.path().join("dev-util");
        std::fs::create_dir_all(cat_dir.join("foo")).unwrap();
        std::fs::create_dir_all(cat_dir.join("bar")).unwrap();

        let cat = Category::new("dev-util".into(), cat_dir.try_into().unwrap(), fs(&tmp));
        let pkgs = cat.packages().filter(|p| p.name() == "foo").collect_vec();
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name(), "foo");
    }
}
