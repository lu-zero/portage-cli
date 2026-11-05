//! Where a repository's files and metadata entries come from
//!
//! [`TreeSource`] serves bytes by repo-relative path; [`EntrySource`] produces the valid
//! metadata entries, which each backend answers its own way. See `docs/design/tree-storage.md`.

use std::io;
use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};
use futures_util::future::BoxFuture;
use portage_atom::Cpv;
use portage_metadata::CacheEntry;

use crate::error::Result;
use crate::metadata_cache::{DirMetadataCache, MetadataCache};
use crate::repo::Repository;

#[cfg(feature = "redb")]
pub mod redb;

/// What a directory child is
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    /// A regular file, or a symlink to one
    File,
    /// A directory, or a symlink to one
    Dir,
}

/// Bytes of one repository's tree, addressed by repo-relative path
pub trait TreeSource: Send + Sync {
    /// The file's content; `Ok(None)` when it does not exist or is a directory
    fn read(&self, rel: &Utf8Path) -> io::Result<Option<Vec<u8>>>;
    /// The immediate children of `rel`; `Ok(None)` when it does not exist or is not a directory
    fn list(&self, rel: &Utf8Path) -> io::Result<Option<Vec<(String, EntryKind)>>>;
    /// What `rel` is, following symlinks; `Ok(None)` when it does not exist
    fn kind(&self, rel: &Utf8Path) -> io::Result<Option<EntryKind>>;
    /// How a file is named to the user: a path, or a URI into a database
    fn display(&self, rel: &Utf8Path) -> String;
    /// Every `cat/pkg/*.ebuild` path, when the backend keeps an index of them; `None` means
    /// walk the tree instead
    fn ebuild_index(&self) -> Option<io::Result<Vec<Utf8PathBuf>>> {
        None
    }
}

/// How a backend produces the valid metadata entries of a repository
pub trait EntrySource: Send + Sync {
    /// Every valid `(cpv, entry)` of `repo`
    fn entries<'a>(&'a self, repo: &'a Repository) -> BoxFuture<'a, Vec<(Cpv, CacheEntry)>>;
}

/// The storage of the repository at `location`
///
/// With the `redb` feature, `<location>.redb` holds the tree and `<location>.md5-cache.redb` the
/// entries, when they exist; otherwise the checkout at `location` serves both.
pub(crate) fn storage_at(location: &Utf8Path) -> Storage {
    #[cfg(feature = "redb")]
    if let Some(found) = redb::storage_at(location) {
        return found;
    }
    Storage::checkout(location)
}

/// Where a repository's files, entries and primary metadata cache come from
#[derive(Clone)]
pub struct Storage {
    /// Files by repo-relative path
    pub tree: Arc<dyn TreeSource>,
    /// The valid metadata entries
    pub entries: Arc<dyn EntrySource>,
    /// The repository's own metadata cache, read by [`Repository::cache_entry`]
    pub primary: Arc<dyn MetadataCache>,
}

impl Storage {
    /// A checkout at `location`, with its md5-cache directory
    pub fn checkout(location: &Utf8Path) -> Self {
        Storage {
            tree: Arc::new(FsTree::new(location)),
            entries: Arc::new(FsEntries),
            primary: Arc::new(DirMetadataCache::new(location.join("metadata/md5-cache"))),
        }
    }
}

/// The tree of the repository at `location`, as [`storage_at`] finds it
pub(crate) fn tree_at(location: &Utf8Path) -> Arc<dyn TreeSource> {
    #[cfg(feature = "redb")]
    if let Some(tree) = redb::tree_at(location) {
        return tree;
    }
    Arc::new(FsTree::new(location))
}

/// Whether the repository at `location` is stored somewhere other than its checkout
pub(crate) fn stored_elsewhere(location: &Utf8Path) -> bool {
    #[cfg(feature = "redb")]
    if redb::tree_file(location).is_file() {
        return true;
    }
    let _ = location;
    false
}

/// Whether `location` holds a repository: a checkout with `metadata/layout.conf` or
/// `profiles/repo_name`, or (with the `redb` feature) a tree database next to it
pub(crate) fn is_repository(location: &Utf8Path) -> bool {
    #[cfg(feature = "redb")]
    if redb::tree_file(location).is_file() {
        return true;
    }
    location.join("metadata/layout.conf").is_file() || location.join("profiles/repo_name").is_file()
}

/// A checkout on disk
#[derive(Clone, Debug)]
pub struct FsTree {
    root: Utf8PathBuf,
}

impl FsTree {
    /// A tree rooted at `root`
    pub fn new(root: impl Into<Utf8PathBuf>) -> Self {
        FsTree { root: root.into() }
    }
}

fn missing(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::IsADirectory | io::ErrorKind::NotADirectory
    )
}

impl TreeSource for FsTree {
    fn read(&self, rel: &Utf8Path) -> io::Result<Option<Vec<u8>>> {
        match std::fs::read(self.root.join(rel)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if missing(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn list(&self, rel: &Utf8Path) -> io::Result<Option<Vec<(String, EntryKind)>>> {
        let dir = match std::fs::read_dir(self.root.join(rel)) {
            Ok(dir) => dir,
            Err(e) if missing(&e) => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        for entry in dir {
            let entry = entry?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            // `file_type` does not follow symlinks; overlays symlink whole package dirs.
            let mut ft = entry.file_type()?;
            if ft.is_symlink() {
                match std::fs::metadata(entry.path()) {
                    Ok(m) => ft = m.file_type(),
                    Err(_) => continue,
                }
            }
            let kind = if ft.is_dir() {
                EntryKind::Dir
            } else {
                EntryKind::File
            };
            out.push((name, kind));
        }
        Ok(Some(out))
    }

    fn kind(&self, rel: &Utf8Path) -> io::Result<Option<EntryKind>> {
        match std::fs::metadata(self.root.join(rel)) {
            Ok(m) if m.is_dir() => Ok(Some(EntryKind::Dir)),
            Ok(_) => Ok(Some(EntryKind::File)),
            Err(e) if missing(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn display(&self, rel: &Utf8Path) -> String {
        self.root.join(rel).into_string()
    }
}

/// Entries of a checkout: the md5-cache, checked against the files it describes
#[derive(Clone, Copy, Debug, Default)]
pub struct FsEntries;

impl EntrySource for FsEntries {
    fn entries<'a>(&'a self, repo: &'a Repository) -> BoxFuture<'a, Vec<(Cpv, CacheEntry)>> {
        Box::pin(crate::entries::repo_entries(repo))
    }
}

/// A file's text; `None` when it does not exist
pub(crate) fn read_text(tree: &dyn TreeSource, rel: &str) -> Result<Option<String>> {
    let rel = Utf8Path::new(rel);
    let Some(bytes) = tree
        .read(rel)
        .map_err(|e| crate::repo::util::io_err(tree.display(rel), e))?
    else {
        return Ok(None);
    };
    String::from_utf8(bytes).map(Some).map_err(|e| {
        crate::repo::util::io_err(
            tree.display(rel),
            std::io::Error::new(std::io::ErrorKind::InvalidData, e),
        )
    })
}

/// Trimmed lines that are neither blank nor comments
pub(crate) fn content_lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
}

/// [`util::read_lines`] over a tree: a file, or a directory of files read in name order
pub(crate) fn tree_lines(tree: &dyn TreeSource, rel: &str) -> Result<Vec<String>> {
    if let Some(text) = read_text(tree, rel)? {
        return Ok(content_lines(&text).map(String::from).collect());
    }
    let path = Utf8Path::new(rel);
    let Some(children) = tree
        .list(path)
        .map_err(|e| crate::repo::util::io_err(tree.display(path), e))?
    else {
        return Ok(Vec::new());
    };
    let mut names: Vec<String> = children
        .into_iter()
        .filter(|(name, kind)| {
            *kind == EntryKind::File && crate::repo::util::config_basename_included(name)
        })
        .map(|(name, _)| name)
        .collect();
    names.sort();
    let mut out = Vec::new();
    for name in names {
        if let Some(text) = read_text(tree, path.join(name).as_str())? {
            out.extend(content_lines(&text).map(String::from));
        }
    }
    Ok(out)
}

/// Names under `rel`, skipping dotfiles; empty when `rel` is missing
pub(crate) fn tree_names(tree: &dyn TreeSource, rel: &str) -> Result<Vec<(String, EntryKind)>> {
    let path = Utf8Path::new(rel);
    let mut names: Vec<(String, EntryKind)> = tree
        .list(path)
        .map_err(|e| crate::repo::util::io_err(tree.display(path), e))?
        .unwrap_or_default()
        .into_iter()
        .filter(|(name, _)| !name.starts_with('.'))
        .collect();
    names.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    Ok(names)
}
