//! Open a [`portage_repo::Repository`] with em's durable user metadata cache
//!
//! Production code should use these helpers so resolve, search, and regen
//! share one secondary store layout without re-deriving paths at each call
//! site.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use portage_repo::{RepoSet, Repository, Result};

/// Open a tree with secondary at `$XDG_CACHE_HOME/em/md5-cache/<repo-name>`
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
    default_master: Option<&str>,
) -> Result<Repository> {
    Repository::builder()
        .user_cache_root(crate::xdg::md5_cache_root())
        .open_with_masters(path, repos_dir, masters_override, default_master)
}

/// The full priority-ordered [`RepoSet`] for this invocation: `main` plus
/// every repo `roots.repos_conf()` returns, already merged and sorted
/// ascending `(priority, name)` there — walked **descending** so a
/// higher-priority repo's cpv wins a duplicate. `main`'s priority comes
/// from the same entry the path filter below matches, defaulted to
/// `-1000` by `ReposConf` if unset.
///
/// `main` is always included, even with no path-matching entry (a symlink
/// can break that match) — falls back to priority `-1000` here too. Each
/// overlay's masters resolve, in order: its own entry (a `PORTDIR_OVERLAY`
/// entry never has one), else its own `metadata/layout.conf`, else `main`.
/// A repo that fails to open is reported and skipped, not fatal.
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
        match open_with_masters(
            path.to_path_buf(),
            &repos_dir,
            e.masters.as_deref(),
            Some(main.name()),
        ) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;

    fn write(path: &camino::Utf8Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap().as_std_path()).unwrap();
        std::fs::write(path.as_std_path(), body).unwrap();
    }

    fn make_ebuild_repo(dir: &camino::Utf8Path, name: &str, cpn: &str, version: &str) {
        write(
            &dir.join("profiles/categories"),
            &format!("{}\n", cpn.split('/').next().unwrap()),
        );
        make_masterless_ebuild_repo(dir, name, cpn, version);
    }

    // Like [`make_ebuild_repo`], but with no `profiles/categories` of its
    // own — a category-`cpn` ebuild here is only discoverable through a
    // master's own category list.
    fn make_masterless_ebuild_repo(dir: &camino::Utf8Path, name: &str, cpn: &str, version: &str) {
        write(&dir.join("metadata/layout.conf"), "");
        write(&dir.join("profiles/repo_name"), &format!("{name}\n"));
        write(
            &dir.join(format!(
                "{cpn}/{}-{version}.ebuild",
                cpn.split('/').nth(1).unwrap()
            )),
            "EAPI=8\nSLOT=0\n",
        );
    }

    // A repo declared only via `PORTDIR_OVERLAY` in make.conf — no
    // `repos.conf` section of its own — must still be discovered and must
    // still shadow main's copy of the same cpv.
    #[test]
    fn portdir_overlay_only_entry_shadows_main() {
        let root = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(root.path().to_path_buf()).unwrap();

        let main_dir = root.join("gentoo");
        make_ebuild_repo(&main_dir, "gentoo", "app-misc/foo", "1");

        let overlay_dir = root.join("x-portage");
        make_ebuild_repo(&overlay_dir, "x-portage", "app-misc/foo", "1");

        write(
            &root.join("config/etc/portage/repos.conf/repos.conf"),
            &format!("[DEFAULT]\nmain-repo = gentoo\n\n[gentoo]\nlocation = {main_dir}\n"),
        );
        write(
            &root.join("config/etc/portage/make.conf"),
            &format!("PORTDIR_OVERLAY=\"{overlay_dir}\"\n"),
        );

        let main = open(main_dir.as_std_path()).unwrap();
        let roots = portage_resolve::Roots::default().with_config(Some(root.join("config")));
        let set = repo_set_from_conf(main, &roots, true);

        assert_eq!(set.len(), 2, "main + the PORTDIR_OVERLAY entry");
        assert!(
            set.by_name("x-portage").is_some(),
            "overlay must be discovered at all"
        );

        let ebuilds: Vec<_> = set.ebuilds().unwrap().collect();
        assert_eq!(
            ebuilds.len(),
            1,
            "duplicate cpv must be shadowed, not duplicated"
        );
        assert!(
            ebuilds[0].ebuild.path().starts_with(&overlay_dir),
            "the PORTDIR_OVERLAY copy must win the shadow, got {:?}",
            ebuilds[0].ebuild.path()
        );
    }

    // Multiple `PORTDIR_OVERLAY` entries get ascending priority `0, 1, ...`
    // by listed order, not by name: the *second*-listed entry here has a
    // name that sorts alphabetically *before* the first-listed one, so a
    // name-only tie-break would pick the wrong winner. It must still win,
    // because listed order gives it the higher priority (`1` vs `0`) —
    // and that priority must also outrank a `repos.conf` overlay sharing
    // the same default-`0` priority, proving the two sources feed one
    // merged sort rather than `PORTDIR_OVERLAY` always trailing.
    #[test]
    fn portdir_overlay_ascending_priority_follows_listed_order_not_name() {
        let root = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(root.path().to_path_buf()).unwrap();

        let main_dir = root.join("gentoo");
        make_ebuild_repo(&main_dir, "gentoo", "app-misc/foo", "1");

        // repos.conf overlay -- unset priority, defaults to 0.
        let conf_overlay_dir = root.join("mmm-overlay");
        make_ebuild_repo(&conf_overlay_dir, "mmm-overlay", "app-misc/foo", "1");

        // Listed first -> priority 0. Name "zzz-po-first" sorts *after*
        // "aaa-po-second" alphabetically, which would win a same-priority
        // name tie-break -- it must NOT win here, since its priority (0)
        // is lower.
        let first_dir = root.join("zzz-po-first");
        make_ebuild_repo(&first_dir, "zzz-po-first", "app-misc/foo", "1");

        // Listed second -> priority 1, strictly above everything else.
        let second_dir = root.join("aaa-po-second");
        make_ebuild_repo(&second_dir, "aaa-po-second", "app-misc/foo", "1");

        write(
            &root.join("config/etc/portage/repos.conf/repos.conf"),
            &format!(
                "[DEFAULT]\nmain-repo = gentoo\n\n[gentoo]\nlocation = {main_dir}\n\n[mmm-overlay]\nlocation = {conf_overlay_dir}\n"
            ),
        );
        write(
            &root.join("config/etc/portage/make.conf"),
            &format!("PORTDIR_OVERLAY=\"{first_dir} {second_dir}\"\n"),
        );

        let main = open(main_dir.as_std_path()).unwrap();
        let roots = portage_resolve::Roots::default().with_config(Some(root.join("config")));
        let set = repo_set_from_conf(main, &roots, true);

        assert_eq!(set.len(), 4);
        let ebuilds: Vec<_> = set.ebuilds().unwrap().collect();
        assert_eq!(
            ebuilds.len(),
            1,
            "quadruple duplicate cpv must be shadowed to one"
        );
        assert!(
            ebuilds[0].ebuild.path().starts_with(&second_dir),
            "the second-listed PORTDIR_OVERLAY entry (priority 1) must win, got {:?}",
            ebuilds[0].ebuild.path()
        );
    }

    // A `PORTDIR_OVERLAY` entry with no `masters =` anywhere (no
    // `repos.conf` section to declare one, empty `layout.conf`) must still
    // default to `main` as its master, so it can see a category it
    // doesn't list itself — real portage's own fallback when a repo
    // declares masters nowhere at all.
    #[test]
    fn portdir_overlay_entry_defaults_to_main_as_master() {
        let root = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(root.path().to_path_buf()).unwrap();

        let main_dir = root.join("gentoo");
        make_ebuild_repo(&main_dir, "gentoo", "app-misc/foo", "1");

        let overlay_dir = root.join("x-portage");
        make_masterless_ebuild_repo(&overlay_dir, "x-portage", "app-misc/bar", "2");

        write(
            &root.join("config/etc/portage/repos.conf/repos.conf"),
            &format!("[DEFAULT]\nmain-repo = gentoo\n\n[gentoo]\nlocation = {main_dir}\n"),
        );
        write(
            &root.join("config/etc/portage/make.conf"),
            &format!("PORTDIR_OVERLAY=\"{overlay_dir}\"\n"),
        );

        let main = open(main_dir.as_std_path()).unwrap();
        let roots = portage_resolve::Roots::default().with_config(Some(root.join("config")));
        let set = repo_set_from_conf(main, &roots, true);

        let ebuilds: Vec<_> = set.ebuilds().unwrap().collect();
        assert!(
            ebuilds
                .iter()
                .any(|e| e.ebuild.path().starts_with(&overlay_dir)),
            "app-misc/bar must be discoverable via the inherited main-repo category list, got {:?}",
            ebuilds.iter().map(|e| e.ebuild.path()).collect::<Vec<_>>()
        );
    }
}
