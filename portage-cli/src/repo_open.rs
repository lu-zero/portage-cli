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

/// Open a tree and its masters (owned by the returned `Repository`, see
/// [`Repository::masters`]); same user-cache root for every repo name.
pub fn open_with_masters(
    path: impl Into<PathBuf>,
    repos_dir: impl AsRef<Path>,
) -> Result<Repository> {
    Repository::builder()
        .user_cache_root(crate::xdg::md5_cache_root())
        .open_with_masters(path, repos_dir)
}

/// The priority-ordered [`RepoSource`] sequence `load_repos` should merge —
/// main plus every `repos.conf` overlay, **descending** by
/// `(priority, name)` so a higher-priority repo's cpv wins a duplicate over
/// a lower one (real portage: `porttree.py`'s `findname2`/`xmatch` walk the
/// ascending `ReposConf::repos()` order in reverse; `man 5 portage`,
/// "packages... with higher priority are preferred"). Main's own priority
/// comes from the same `repos.conf` entry the path filter below matches
/// against — not a separate lookup by name via `ReposConf::main_repo()`,
/// which can disagree with `main` (unset `main-repo =`, or a `main` that
/// isn't actually the conf's main repo) — and defaults to `-1000` already
/// (`ReposConf::load_from` applies that default at parse time), so this
/// function never needs to re-derive it.
///
/// Also returns the alias (virtual, no on-disk tree) entries `load_repos`
/// derives cross-`<tuple>` packages from.
///
/// `[RepoSource::Main]` alone when `multi_repo` is false — main is always a
/// source, unconditionally; there just aren't any overlays to merge with it.
/// Masters resolve relative to the main repo's parent directory, so e.g. the
/// crossdev overlay's `masters = gentoo` finds `/var/db/repos/gentoo`. A
/// repo that fails to open is reported and skipped rather than failing the
/// command.
///
/// Shared: a caller that skips the overlays sees an incomplete tree and reports
/// every overlay-only package as missing.
pub fn overlays_from_conf(
    main: &Repository,
    roots: &portage_resolve::Roots,
    multi_repo: bool,
) -> (
    Vec<portage_resolve::repo::RepoSource>,
    Vec<portage_repo::RepoEntry>,
) {
    use portage_resolve::repo::RepoSource;

    if !multi_repo {
        return (vec![RepoSource::Main], Vec::new());
    }
    let Ok(conf) = roots.repos_conf() else {
        return (vec![RepoSource::Main], Vec::new());
    };
    let repos_dir = main.path().parent().map(PathBuf::from).unwrap_or_default();
    let sources = conf
        .repos()
        .iter()
        .rev()
        .filter_map(|e| {
            if e.location
                .as_path()
                .is_some_and(|p| p == main.path().as_std_path())
            {
                return Some(RepoSource::Main);
            }
            let path = e.location.as_path()?.to_path_buf();
            match open_with_masters(path, &repos_dir) {
                Ok(repo) => Some(RepoSource::Overlay(repo)),
                Err(err) => {
                    crate::style::warn_line!(
                        "skipping repo '{}' at {}: {err}",
                        e.name,
                        e.location.as_path().unwrap_or(Path::new("")).display()
                    );
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
    (sources, aliases)
}
