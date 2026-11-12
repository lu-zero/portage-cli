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
    /// A real directory holding `rels` at their repo-relative paths: the checkout itself, or a
    /// copy under `copies` kept per tree and reused
    fn checkout(&self, rels: &[&Utf8Path], copies: Option<&Utf8Path>) -> io::Result<Utf8PathBuf>;
    /// Whether the tree is a checkout on disk, which writers may change in place
    fn is_checkout(&self) -> bool;
}

/// How a backend produces the valid metadata entries of a repository
pub trait EntrySource: Send + Sync {
    /// Every valid `(cpv, entry)` of `repo`
    fn entries<'a>(&'a self, repo: &'a Repository) -> BoxFuture<'a, Vec<(Cpv, CacheEntry)>>;
    /// Every stored entry's text, unchecked (only the highest version of each package when
    /// `latest_per_cpn`); `None` means read the checkout's md5-cache directory
    fn raw_entries(&self, _latest_per_cpn: bool) -> Option<io::Result<Vec<(Cpv, String)>>> {
        None
    }
}

/// The storage of the repository at `location`: its checkout, serving both files and entries
pub(crate) fn storage_at(location: &Utf8Path) -> io::Result<Storage> {
    Ok(Storage::checkout(location))
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
pub(crate) fn tree_at(location: &Utf8Path) -> io::Result<Arc<dyn TreeSource>> {
    Ok(Arc::new(FsTree::new(location)))
}

/// Whether `location` holds a repository: a checkout with `metadata/layout.conf` or
/// `profiles/repo_name`
pub(crate) fn is_repository(location: &Utf8Path) -> bool {
    location.join("metadata/layout.conf").is_file() || location.join("profiles/repo_name").is_file()
}

/// One configuration file, or a directory of them, in some tree
#[derive(Clone)]
pub struct TreeFile {
    tree: Arc<dyn TreeSource>,
    rel: Utf8PathBuf,
}

impl std::fmt::Debug for TreeFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.tree.display(&self.rel))
    }
}

impl TreeFile {
    pub(crate) fn new(tree: Arc<dyn TreeSource>, rel: Utf8PathBuf) -> Self {
        TreeFile { tree, rel }
    }

    /// A file, or directory of files, on disk
    pub fn on_disk(path: &Utf8Path) -> Self {
        let dir = path.parent().unwrap_or(Utf8Path::new("/"));
        let rel = Utf8PathBuf::from(path.file_name().unwrap_or_default());
        TreeFile::new(Arc::new(FsTree::new(dir)), rel)
    }

    /// Whether it is a directory
    pub fn is_dir(&self) -> bool {
        matches!(self.tree.kind(&self.rel), Ok(Some(EntryKind::Dir)))
    }

    /// The texts it holds, each with the name to show for it: the file's, or those of the files
    /// in the directory form in name order (dotfiles and `~` backups skipped); missing or
    /// unreadable files are left out
    pub fn texts(&self) -> Vec<(String, String)> {
        let show = |rel: &Utf8Path| self.tree.display(rel);
        if let Ok(Some(text)) = read_text(&*self.tree, self.rel.as_str()) {
            return vec![(show(&self.rel), text)];
        }
        let Ok(Some(names)) = config_files(&*self.tree, &self.rel) else {
            return Vec::new();
        };
        names
            .into_iter()
            .filter_map(|name| {
                let rel = self.rel.join(name);
                let text = read_text(&*self.tree, rel.as_str()).ok().flatten()?;
                Some((show(&rel), text))
            })
            .collect()
    }
}

/// An ebuild in its repository's tree, made a real file only when a build needs one
#[derive(Clone)]
pub struct EbuildFile {
    tree: Arc<dyn TreeSource>,
    copies: Option<Utf8PathBuf>,
    rel: Utf8PathBuf,
    location: Utf8PathBuf,
}

impl std::fmt::Debug for EbuildFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.tree.display(&self.rel))
    }
}

impl EbuildFile {
    pub(crate) fn new(
        tree: Arc<dyn TreeSource>,
        copies: Option<Utf8PathBuf>,
        rel: Utf8PathBuf,
        location: Utf8PathBuf,
    ) -> Self {
        EbuildFile {
            tree,
            copies,
            rel,
            location,
        }
    }

    /// The ebuild at `path`, in the checkout three levels above it
    pub fn on_disk(path: &Utf8Path) -> Self {
        let location = path
            .ancestors()
            .nth(3)
            .unwrap_or(Utf8Path::new(""))
            .to_owned();
        let rel = path.strip_prefix(&location).unwrap_or(path).to_owned();
        EbuildFile {
            tree: Arc::new(FsTree::new(location.clone())),
            copies: None,
            rel,
            location,
        }
    }

    /// Where its repository is configured, to open it by
    pub fn location(&self) -> &Utf8Path {
        &self.location
    }

    /// The ebuild as a real file in a real package directory, for `FILESDIR` and `EBUILD`
    pub fn checkout(&self) -> Result<Utf8PathBuf> {
        let pkg = self.rel.parent().unwrap_or(Utf8Path::new(""));
        let root = self
            .tree
            .checkout(&[pkg], self.copies.as_deref())
            .map_err(|e| crate::repo::util::io_err(self.tree.display(pkg), e))?;
        Ok(root.join(&self.rel))
    }
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

    fn checkout(&self, _rels: &[&Utf8Path], _copies: Option<&Utf8Path>) -> io::Result<Utf8PathBuf> {
        Ok(self.root.clone())
    }

    fn is_checkout(&self) -> bool {
        true
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
    let Some(names) =
        config_files(tree, path).map_err(|e| crate::repo::util::io_err(tree.display(path), e))?
    else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for name in names {
        if let Some(text) = read_text(tree, path.join(name).as_str())? {
            out.extend(content_lines(&text).map(String::from));
        }
    }
    Ok(out)
}

/// The files of the configuration directory `rel` in name order, dotfiles and `~` backups
/// skipped; `None` when it is not a directory
fn config_files(tree: &dyn TreeSource, rel: &Utf8Path) -> io::Result<Option<Vec<String>>> {
    let Some(children) = tree.list(rel)? else {
        return Ok(None);
    };
    let mut names: Vec<String> = children
        .into_iter()
        .filter(|(name, kind)| {
            *kind == EntryKind::File && crate::repo::util::config_basename_included(name)
        })
        .map(|(name, _)| name)
        .collect();
    names.sort();
    Ok(Some(names))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_of_files_reads_in_name_order_without_dotfiles_or_backups() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        let conf = root.join("package.use");
        std::fs::create_dir_all(&conf).unwrap();
        for (name, text) in [("b", "2"), ("a", "1"), (".hidden", "x"), ("c~", "x")] {
            std::fs::write(conf.join(name), text).unwrap();
        }
        let texts = TreeFile::on_disk(&conf).texts();
        let shown: Vec<_> = texts
            .iter()
            .map(|(name, text)| (name.as_str(), text.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                (conf.join("a").as_str(), "1"),
                (conf.join("b").as_str(), "2")
            ]
        );
        assert!(TreeFile::on_disk(&conf).is_dir());
        assert!(TreeFile::on_disk(&root.join("missing")).texts().is_empty());
    }

    #[test]
    fn an_ebuild_on_disk_is_its_own_checkout() {
        let path = Utf8Path::new("/var/db/repos/gentoo/app-misc/jq/jq-1.8.2.ebuild");
        let ebuild = EbuildFile::on_disk(path);
        assert_eq!(ebuild.location(), "/var/db/repos/gentoo");
        assert_eq!(ebuild.checkout().unwrap(), path);
    }
}
