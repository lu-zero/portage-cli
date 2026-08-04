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

/// The `repos.conf` overlays to load alongside `main`, and the alias (virtual,
/// no on-disk tree) entries `load_repos` derives cross-`<tuple>` packages from.
///
/// Empty when `multi_repo` is false. Masters resolve relative to the main
/// repo's parent directory, so e.g. the crossdev overlay's `masters = gentoo`
/// finds `/var/db/repos/gentoo`. A repo that fails to open is reported and
/// skipped rather than failing the command.
///
/// Shared: a caller that skips the overlays sees an incomplete tree and reports
/// every overlay-only package as missing.
pub fn overlays_from_conf(
    main: &Repository,
    roots: &portage_resolve::Roots,
    multi_repo: bool,
) -> (
    Vec<(Repository, Vec<Repository>)>,
    Vec<portage_repo::RepoEntry>,
) {
    if !multi_repo {
        return (Vec::new(), Vec::new());
    }
    let Ok(conf) = roots.repos_conf() else {
        return (Vec::new(), Vec::new());
    };
    let repos_dir = main.path().parent().map(PathBuf::from).unwrap_or_default();
    let overlays = conf
        .repos()
        .iter()
        .filter(|e| {
            e.location
                .as_path()
                .is_none_or(|p| p != main.path().as_std_path())
        })
        .filter_map(|e| {
            let path = e.location.as_path()?.to_path_buf();
            match open_with_masters(path, &repos_dir) {
                Ok(pair) => Some(pair),
                Err(err) => {
                    crate::style::warn_line(&format!(
                        "skipping repo '{}' at {}: {err}",
                        e.name,
                        e.location.as_path().unwrap_or(Path::new("")).display()
                    ));
                    None
                }
            }
        })
        .collect();
    let aliases = conf
        .repos()
        .iter()
        .filter(|e| matches!(e.location, portage_repo::Location::Alias { .. }))
        .cloned()
        .collect();
    (overlays, aliases)
}
