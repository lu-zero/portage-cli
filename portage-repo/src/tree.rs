//! Where a repository's files and metadata entries come from
//!
//! [`TreeSource`] serves bytes by repo-relative path; [`EntrySource`] produces the valid
//! metadata entries, which each backend answers its own way. See `docs/design/tree-storage.md`.

use std::io;

use camino::{Utf8Path, Utf8PathBuf};
use futures_util::future::BoxFuture;
use portage_atom::Cpv;
use portage_metadata::CacheEntry;

use crate::repo::Repository;

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
    /// How a file is named to the user: a path, or a URI into a database
    fn display(&self, rel: &Utf8Path) -> String;
}

/// How a backend produces the valid metadata entries of a repository
pub trait EntrySource: Send + Sync {
    /// Every valid `(cpv, entry)` of `repo`
    fn entries<'a>(&'a self, repo: &'a Repository) -> BoxFuture<'a, Vec<(Cpv, CacheEntry)>>;
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
