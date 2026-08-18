//! Open a [`portage_repo::Repository`] with em's durable user metadata cache.
//!
//! Production code should use these helpers so resolve, search, and regen
//! share one secondary store layout without re-deriving paths at each call
//! site.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use portage_repo::{RepoSet, Repository, Result};

/// Open a tree with secondary at `$XDG_CACHE_HOME/em/md5-cache/<repo-name>`.
pub fn open(path: impl Into<PathBuf>) -> Result<Repository> {
    Repository::builder()
        .user_cache_root(crate::xdg::md5_cache_root())
        .open(path)
}

/// Open a tree and its masters (owned by the returned `Repository`, see
/// [`Repository::masters`]); same user-cache root for every repo name.
pub fn open_with_masters(
    path: impl Into<PathBuf>,
    repos_dir: impl AsRef<Path>,
    masters_override: Option<&[String]>,
) -> Result<Repository> {
    Repository::builder()
        .user_cache_root(crate::xdg::md5_cache_root())
        .open_with_masters(path, repos_dir, masters_override)
}

/// The full priority-ordered [`RepoSet`] for this invocation: `main` plus
/// every `repos.conf` overlay, **descending** by `(priority, name)` so a
/// higher-priority repo's cpv wins a duplicate over a lower one (real
/// portage: `porttree.py`'s `findname2`/`xmatch` walk the ascending
/// `ReposConf::repos()` order in reverse; `man 5 portage`, "packages...
/// with higher priority are preferred"). Main's own priority comes from the
/// same `repos.conf` entry the path filter below matches against — not a
/// separate lookup by name via `ReposConf::main_repo()`, which can disagree
/// with `main` (unset `main-repo =`, or a `main` that isn't actually the
/// conf's main repo) — and defaults to `-1000` already (`ReposConf::
/// load_from` applies that default at parse time), so this function never
/// needs to re-derive it.
///
/// `main` is in the returned set **unconditionally**, even if no
/// `repos.conf` entry's `location` string-matches its path (a symlink,
/// trailing slash, or `Cli::repo_path()`'s `/var/db/repos/gentoo` fallback
/// can all break that match) — previously main would simply be dropped from
/// the merge in that case.
///
/// A single-repo set (main only) when `multi_repo` is false or repos.conf
/// can't be read — main is always a source, unconditionally; there just
/// aren't any overlays to merge with it. Masters resolve relative to the
/// main repo's parent directory, so e.g. the crossdev overlay's
/// `masters = gentoo` finds `/var/db/repos/gentoo`. A repo that fails to
/// open is reported and skipped rather than failing the command.
///
/// Takes `main` by value — the caller opens it once and hands it over;
/// nothing here clones a `Repository`.
pub fn repo_set_from_conf(
    main: Repository,
    roots: &portage_resolve::Roots,
    multi_repo: bool,
) -> RepoSet {
    if !multi_repo {
        return RepoSet::single(main);
    }
    let Ok(conf) = roots.repos_conf() else {
        return RepoSet::single(main);
    };
    let repos_dir = main.path().parent().map(PathBuf::from).unwrap_or_default();
    let main = Arc::new(main);
    let mut repos: Vec<Arc<Repository>> = Vec::new();
    let mut main_index = None;
    for e in conf.repos().iter().rev() {
        if e.location
            .as_path()
            .is_some_and(|p| p == main.path().as_std_path())
        {
            main_index = Some(repos.len());
            repos.push(Arc::clone(&main));
            continue;
        }
        let Some(path) = e.location.as_path() else {
            continue; // alias: no on-disk tree, handled by `aliases` below
        };
        match open_with_masters(path.to_path_buf(), &repos_dir, e.masters.as_deref()) {
            Ok(r) => repos.push(Arc::new(r)),
            Err(err) => {
                crate::style::warn_line!("skipping repo '{}' at {}: {err}", e.name, path.display());
            }
        }
    }
    let main_index = main_index.unwrap_or_else(|| {
        repos.push(Arc::clone(&main));
        repos.len() - 1
    });
    let aliases = conf
        .repos()
        .iter()
        .filter(|e| matches!(e.location, portage_repo::Location::Alias { .. }))
        .cloned()
        .collect();
    RepoSet::from_ordered(repos, main_index, aliases)
}
