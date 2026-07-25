//! Open a [`portage_repo::Repository`] with em's durable user metadata cache.
//!
//! Production code should use these helpers so resolve, search, and regen
//! share one secondary store layout without re-deriving paths at each call
//! site.

use std::path::{Path, PathBuf};

use portage_repo::{Repository, Result};

/// Open a tree with secondary at `$XDG_CACHE_HOME/em/md5-cache/<repo-name>`.
pub fn open(path: impl Into<PathBuf>) -> Result<Repository> {
    Repository::builder()
        .user_cache_root(crate::xdg::md5_cache_root())
        .open(path)
}

/// Open a tree and its masters; same user-cache root for every repo name.
pub fn open_with_masters(
    path: impl Into<PathBuf>,
    repos_dir: impl AsRef<Path>,
) -> Result<(Repository, Vec<Repository>)> {
    Repository::builder()
        .user_cache_root(crate::xdg::md5_cache_root())
        .open_with_masters(path, repos_dir)
}
