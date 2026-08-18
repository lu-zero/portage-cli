use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use camino::{Utf8Path, Utf8PathBuf};
use portage_atom::Cpn;

use super::ini;
use super::repository::Repository;
use crate::error::Result;

/// Where a configured repository's packages live.
#[derive(Debug, Clone)]
pub enum Location {
    /// A real on-disk repository at this path.
    Path(PathBuf),
    /// A virtual repository: no on-disk tree. Packages are derived from a
    /// source repo, re-categorized under one or more destination categories.
    /// Used by `crossdev` to present `cross-<tuple>/<pkg>` packages without a
    /// symlink overlay.
    Alias {
        /// The source repo name (e.g. `"gentoo"`) whose packages are aliased.
        source: String,
        /// Destination category → source cpns within [`source`](Self::Alias::source).
        /// Key: the category the packages appear under in this virtual repo
        /// (e.g. `cross-riscv64-unknown-linux-gnu`). Value: the real cpns
        /// (e.g. `sys-devel/gcc`) whose versions + metadata are cloned.
        aliases: HashMap<String, HashSet<Cpn>>,
    },
}

impl Location {
    /// The on-disk path, if this is a real [`Location::Path`].
    pub fn as_path(&self) -> Option<&Path> {
        match self {
            Location::Path(p) => Some(p.as_path()),
            Location::Alias { .. } => None,
        }
    }
}

/// A single repository entry parsed from `repos.conf`.
#[derive(Debug, Clone)]
pub struct RepoEntry {
    /// Section name (e.g. `gentoo`, `crossdev`).
    pub name: String,
    /// Where the repository's packages live: a real path or a virtual alias.
    pub location: Location,
    /// `masters` from repos.conf. `None` means the key is absent from every
    /// `repos.conf` file for this repo — real portage then falls back to
    /// `metadata/layout.conf`'s `masters =`. `Some(vec![])` means the key
    /// was present but empty, which explicitly opts out of that fallback
    /// (`repository/config.py`'s `RepoConfigLoader.__init__`: `self.masters
    /// = repo_opts.get("masters")`, then `if self.masters is None:
    /// self.masters = layout_data["masters"]`). repos.conf wins over
    /// layout.conf whenever it declares anything at all, even `masters =`
    /// on its own — many hand-maintained overlays (e.g. a plain
    /// `/usr/local/portage` tree) declare masters only in repos.conf and
    /// ship no `metadata/layout.conf` of their own.
    pub masters: Option<Vec<String>>,
    /// `sync-type` from repos.conf (`git`, `rsync`, …). Empty means unsyncable.
    pub sync_type: Option<String>,
    /// `sync-uri` from repos.conf (remote URL for the sync module).
    pub sync_uri: Option<String>,
    /// `auto-sync` from repos.conf. Defaults to `true` when unset (Portage).
    pub auto_sync: bool,
    /// `volatile` from repos.conf. When `true`, sync must not clobber local
    /// changes (`git reset --hard` / `clean`). Unset means “infer from
    /// ownership” at sync time (Portage: volatile if not root/portage-owned).
    pub volatile: Option<bool>,
    /// `priority` from repos.conf. Determines resolution order: repos are
    /// searched (and `emerge --info`/`em --info` list them) ascending by
    /// `(priority, name)`, lower first — a *negative* priority is searched
    /// **before** the default `0`, e.g. an overlay meant to shadow the main
    /// repo. Real portage defaults unset to `0` at sort time and forces the
    /// main repo specifically to `-1000` when *it* has no explicit priority
    /// (`repository/config.py`'s `RepoConfigLoader.__init__`) — see
    /// [`ReposConf::load_from`] for where that default is applied; this
    /// field itself stays `None` until then, distinguishing "explicitly set
    /// to 0" from "unset".
    pub priority: Option<i64>,
}

impl RepoEntry {
    /// Whether this entry can be synced (has both type and URI, on-disk path).
    pub fn is_syncable(&self) -> bool {
        self.location.as_path().is_some()
            && self.sync_type.as_ref().is_some_and(|t| !t.is_empty())
            && self.sync_uri.as_ref().is_some_and(|u| !u.is_empty())
    }
}

/// Parsed `repos.conf` describing every configured repository.
///
/// The Gentoo `repos.conf` format is read from multiple locations in
/// override order. Sections sharing a `[name]` are merged key-by-key,
/// with later files overriding earlier ones. The `[DEFAULT]` section's
/// `main-repo` key selects which repo is the main one (placed first).
///
/// See [Repository format — repos.conf](https://wiki.gentoo.org/wiki/Handbook:AMD64/Portage/CustomTree#Defining_a_custom_repository).
#[derive(Debug, Clone, Default)]
pub struct ReposConf {
    repos: Vec<RepoEntry>,
    main_repo: Option<String>,
}

impl ReposConf {
    /// Load `repos.conf` for a system rooted at `/` (`config_root = /`, no
    /// overlay).
    pub fn load() -> Result<Self> {
        Self::load_rooted(Utf8Path::new("/"), &[])
    }

    /// Load `repos.conf` in portage's search order, rooted at `config_root`: the
    /// global defaults (`<config_root>/usr/share/portage/config/repos.conf`),
    /// then the user confdir (`<config_root>/etc/portage/repos.conf`), then each
    /// `extra` confdir's `repos.conf` (e.g. a `--local`/`--prefix` overlay that
    /// layers on a host `config_root`). Mirrors portage's
    /// `load_repository_config()`. Missing paths are skipped.
    pub fn load_rooted(config_root: &Utf8Path, extra: &[&Utf8Path]) -> Result<Self> {
        let mut paths: Vec<Utf8PathBuf> = vec![
            config_root.join("usr/share/portage/config/repos.conf"),
            config_root.join("etc/portage/repos.conf"),
        ];
        paths.extend(extra.iter().map(|d| d.join("repos.conf")));
        Self::load_from(&paths)
    }

    /// Load from explicit paths in override order. Each path may be a file
    /// or a directory; directories contribute every `*.conf` they contain
    /// in alphabetical order. Missing paths are silently skipped.
    pub fn load_from<P: AsRef<Path>>(paths: &[P]) -> Result<Self> {
        let mut sections: HashMap<String, HashMap<String, String>> = HashMap::new();
        let mut order: Vec<String> = Vec::new();

        for path in paths {
            for file in ini::collect_conf_files(path.as_ref())? {
                let contents = match std::fs::read_to_string(&file) {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(super::util::io_err(&file, e)),
                };
                ini::merge_sections(&mut sections, &mut order, &contents);
            }
        }

        let main_repo = sections
            .get("DEFAULT")
            .and_then(|s| s.get("main-repo"))
            .cloned();

        let mut repos: Vec<RepoEntry> = order
            .iter()
            .filter_map(|name| {
                let s = sections.get(name)?;
                let masters: Option<Vec<String>> = s
                    .get("masters")
                    .map(|v| v.split_whitespace().map(String::from).collect());
                // A real repo has a `location = /path`. A virtual alias repo
                // has `alias-source = <repo>` + `alias-target = <dest-cat>`
                // (+ optional `alias-packages`). See `Location::Alias`.
                if let (Some(source), Some(target)) = (s.get("alias-source"), s.get("alias-target"))
                {
                    let pkgs = s
                        .get("alias-packages")
                        .map(|v| {
                            v.split_whitespace()
                                .filter_map(|cpn| Cpn::parse(cpn).ok())
                                .collect()
                        })
                        .unwrap_or_default();
                    let mut aliases = HashMap::new();
                    aliases.insert(target.clone(), pkgs);
                    return Some(RepoEntry {
                        name: name.clone(),
                        location: Location::Alias {
                            source: source.clone(),
                            aliases,
                        },
                        masters,
                        sync_type: None,
                        sync_uri: None,
                        auto_sync: false,
                        volatile: None,
                        priority: s.get("priority").and_then(|v| v.trim().parse().ok()),
                    });
                }
                let location = s.get("location")?;
                Some(RepoEntry {
                    name: name.clone(),
                    location: Location::Path(PathBuf::from(location)),
                    masters,
                    sync_type: s.get("sync-type").cloned().filter(|t| !t.is_empty()),
                    sync_uri: s.get("sync-uri").cloned().filter(|u| !u.is_empty()),
                    // Portage default: auto-sync = yes
                    auto_sync: match s.get("auto-sync").map(|v| v.trim().to_ascii_lowercase()) {
                        None => true,
                        Some(v) => matches!(v.as_str(), "yes" | "true"),
                    },
                    volatile: s
                        .get("volatile")
                        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "yes" | "true")),
                    // Invalid/non-numeric `priority =` is silently treated as
                    // unset, matching real portage's own `try: priority =
                    // int(priority) except ValueError: priority = None`.
                    priority: s.get("priority").and_then(|v| v.trim().parse().ok()),
                })
            })
            .collect();

        // Real portage: the main repo defaults to priority -1000 if it has
        // no explicit `priority =` of its own — searched before everything
        // else without requiring every other repos.conf section to also
        // carry an explicit priority just to stay behind it.
        if let Some(main) = main_repo.as_deref()
            && let Some(entry) = repos.iter_mut().find(|r| r.name == main)
            && entry.priority.is_none()
        {
            entry.priority = Some(-1000);
        }

        // Final resolution order: ascending by (priority, name) -- lower
        // priority first, unset treated as 0, name as a stable tiebreak for
        // predictable ordering when priorities collide (`config.py`'s own
        // `sorted(prepos.items(), key=lambda r: (r[1].priority or 0,
        // r[1].name))`). Replaces the old "just move main-repo to the
        // front, otherwise keep file-encounter order" logic, which ignored
        // every other repo's `priority =` entirely.
        repos.sort_by(|a, b| {
            (a.priority.unwrap_or(0), a.name.as_str())
                .cmp(&(b.priority.unwrap_or(0), b.name.as_str()))
        });

        Ok(ReposConf { repos, main_repo })
    }

    /// Every configured repository in resolution order: ascending by
    /// `(priority, name)` — the main repo sorts first in practice (its
    /// default priority is `-1000` when unset) unless another repo's
    /// explicit `priority =` is even lower, which is legitimate and matches
    /// real portage rather than being overridden.
    pub fn repos(&self) -> &[RepoEntry] {
        &self.repos
    }

    /// The main repo, if a `[DEFAULT] main-repo` is set and resolves.
    pub fn main_repo(&self) -> Option<&RepoEntry> {
        let name = self.main_repo.as_deref()?;
        self.repos.iter().find(|r| r.name == name)
    }

    /// Look up an entry by repository name.
    pub fn find(&self, name: &str) -> Option<&RepoEntry> {
        self.repos.iter().find(|r| r.name == name)
    }

    /// Open every configured **on-disk** repository (skipping virtual/alias
    /// repos, which have no path to open). Main repo first; rest in
    /// configuration order. Uses an in-memory secondary metadata cache per
    /// repo (callers that need a durable user cache should open individually
    /// via [`Repository::builder`]).
    pub fn open_all(&self) -> Result<Vec<Repository>> {
        self.repos
            .iter()
            .filter_map(|e| e.location.as_path())
            .map(|p| Repository::builder().in_memory_cache().open(p))
            .collect()
    }

    /// Fold legacy `PORTDIR_OVERLAY` directories in as synthetic entries,
    /// then re-sort ascending `(priority, name)` together with everything
    /// `repos.conf` already declared — matches real portage folding both
    /// sources into one `prepos` dict before its own final sort
    /// (`RepoConfigLoader._add_repositories`). Each directory gets a name
    /// from its own `profiles/repo_name` (`x-<basename>` fallback, see
    /// [`super::util::resolve_repo_name`]) and ascending priority
    /// `0, 1, ...` in listed order. A directory that isn't a real
    /// directory, or whose path already matches an existing entry,
    /// contributes nothing. Reading `PORTDIR_OVERLAY` itself from
    /// make.conf isn't this module's concern — see
    /// `portage_resolve::Roots::portdir_overlay`.
    pub fn with_portdir_overlay(mut self, portdir_overlay: &[Utf8PathBuf]) -> Self {
        let existing: std::collections::HashSet<PathBuf> = self
            .repos
            .iter()
            .filter_map(|e| e.location.as_path().map(Path::to_path_buf))
            .collect();
        for (i, dir) in portdir_overlay.iter().enumerate() {
            if !dir.is_dir() || existing.contains(dir.as_std_path()) {
                continue;
            }
            let Ok(name) = super::util::resolve_repo_name(dir.as_std_path()) else {
                continue;
            };
            self.repos.push(RepoEntry {
                name,
                location: Location::Path(dir.clone().into_std_path_buf()),
                masters: None,
                sync_type: None,
                sync_uri: None,
                auto_sync: false,
                volatile: None,
                priority: Some(i as i64),
            });
        }
        self.repos.sort_by(|a, b| {
            (a.priority.unwrap_or(0), a.name.as_str())
                .cmp(&(b.priority.unwrap_or(0), b.name.as_str()))
        });
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    /// Bare directory, no `repos.conf` section at all: name falls back to
    /// its own `profiles/repo_name` (or `x-<basename>`, tested separately
    /// in `portage-repo/src/repo/util.rs`), priority `0` (first/only entry).
    #[test]
    fn with_portdir_overlay_adds_a_dir_not_in_repos_conf() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("x-portage/profiles/repo_name"),
            "x-portage\n",
        );
        let overlay = Utf8PathBuf::from_path_buf(dir.path().join("x-portage")).unwrap();

        let rc = ReposConf::default().with_portdir_overlay(std::slice::from_ref(&overlay));
        assert_eq!(rc.repos().len(), 1);
        assert_eq!(rc.repos()[0].name, "x-portage");
        assert_eq!(rc.repos()[0].priority, Some(0));
        assert_eq!(
            rc.repos()[0].location.as_path(),
            Some(overlay.as_std_path())
        );
    }

    /// A `PORTDIR_OVERLAY` directory whose path already matches an
    /// existing `repos.conf` entry contributes nothing — no duplicate.
    #[test]
    fn with_portdir_overlay_skips_a_dir_already_in_repos_conf() {
        let dir = tempfile::tempdir().unwrap();
        let gentoo = dir.path().join("gentoo");
        std::fs::create_dir_all(&gentoo).unwrap();
        let conf = dir.path().join("repos.conf");
        write(
            &conf,
            &format!("[gentoo]\nlocation = {}\n", gentoo.display()),
        );
        let rc = ReposConf::load_from(&[&conf]).unwrap();
        assert_eq!(rc.repos().len(), 1);

        let rc = rc.with_portdir_overlay(&[Utf8PathBuf::from_path_buf(gentoo).unwrap()]);
        assert_eq!(rc.repos().len(), 1, "no duplicate entry for the same path");
    }

    /// Later-listed directories outrank earlier ones (ascending priority
    /// `0, 1, ...` in listed order), independent of name.
    #[test]
    fn with_portdir_overlay_assigns_ascending_priority_by_listed_order() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("zzz/profiles/repo_name"), "zzz\n");
        write(&dir.path().join("aaa/profiles/repo_name"), "aaa\n");
        let first = Utf8PathBuf::from_path_buf(dir.path().join("zzz")).unwrap();
        let second = Utf8PathBuf::from_path_buf(dir.path().join("aaa")).unwrap();

        let rc = ReposConf::default().with_portdir_overlay(&[first, second]);
        assert_eq!(rc.repos().len(), 2);
        assert_eq!(rc.repos()[0].name, "zzz");
        assert_eq!(rc.repos()[0].priority, Some(0));
        assert_eq!(rc.repos()[1].name, "aaa");
        assert_eq!(rc.repos()[1].priority, Some(1));
    }

    #[test]
    fn parse_single_file() {
        let dir = tempfile::tempdir().unwrap();
        let conf = dir.path().join("repos.conf");
        write(
            &conf,
            r#"
[DEFAULT]
main-repo = gentoo

[gentoo]
location = /var/db/repos/gentoo
sync-type = git
sync-uri = https://github.com/gentoo-mirror/gentoo.git

[crossdev]
location = /var/db/repos/crossdev
masters = gentoo
auto-sync = no
"#,
        );
        let rc = ReposConf::load_from(&[&conf]).unwrap();
        assert_eq!(rc.repos().len(), 2);
        assert_eq!(rc.repos()[0].name, "gentoo");
        assert_eq!(rc.repos()[1].name, "crossdev");
        assert_eq!(rc.repos()[1].masters, Some(vec!["gentoo".to_string()]));
        assert_eq!(rc.main_repo().map(|r| r.name.as_str()), Some("gentoo"));
        let gentoo = rc.find("gentoo").unwrap();
        assert_eq!(gentoo.sync_type.as_deref(), Some("git"));
        assert!(gentoo.sync_uri.as_ref().unwrap().contains("gentoo-mirror"));
        assert!(gentoo.auto_sync);
        assert!(gentoo.is_syncable());
        let cross = rc.find("crossdev").unwrap();
        assert!(!cross.auto_sync);
        assert!(!cross.is_syncable());
    }

    #[test]
    fn merges_directory_alphabetical() {
        let dir = tempfile::tempdir().unwrap();
        let confdir = dir.path().join("repos.conf");
        write(
            &confdir.join("00-defaults.conf"),
            "[DEFAULT]\nmain-repo = gentoo\n[gentoo]\nlocation = /a\n",
        );
        write(
            &confdir.join("10-overlay.conf"),
            "[overlay]\nlocation = /b\n",
        );
        let rc = ReposConf::load_from(&[&confdir]).unwrap();
        let names: Vec<_> = rc.repos().iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["gentoo", "overlay"]);
    }

    #[test]
    fn later_path_overrides_earlier() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.conf");
        let b = dir.path().join("b.conf");
        write(&a, "[gentoo]\nlocation = /old\n");
        write(&b, "[gentoo]\nlocation = /new\n");
        let rc = ReposConf::load_from(&[&a, &b]).unwrap();
        assert_eq!(
            rc.find("gentoo").unwrap().location.as_path().unwrap(),
            std::path::Path::new("/new")
        );
    }

    #[test]
    fn missing_paths_are_silently_skipped() {
        let rc = ReposConf::load_from(&[Path::new("/nonexistent/path")]).unwrap();
        assert!(rc.repos().is_empty());
    }

    /// The mechanism changed (was: a hardcoded "move main-repo to position
    /// 0"; now: the main repo's implicit `priority = -1000` default sorting
    /// first among unset-priority ({0}) repos) but the observable behavior
    /// is the same for the common case of no repo setting an explicit
    /// `priority =`.
    #[test]
    fn main_repo_moves_to_front_even_when_declared_later() {
        let dir = tempfile::tempdir().unwrap();
        let conf = dir.path().join("repos.conf");
        write(
            &conf,
            r#"
[overlay]
location = /b

[gentoo]
location = /a

[DEFAULT]
main-repo = gentoo
"#,
        );
        let rc = ReposConf::load_from(&[&conf]).unwrap();
        assert_eq!(rc.repos()[0].name, "gentoo");
        assert_eq!(rc.find("gentoo").unwrap().priority, Some(-1000));
        assert_eq!(rc.find("overlay").unwrap().priority, None);
    }

    /// An explicit `priority =` reorders repos regardless of file order —
    /// the actual gap this landed to close: the old code only ever moved
    /// the main repo to the front and otherwise kept pure file-encounter
    /// order, ignoring every other repo's `priority =` entirely.
    #[test]
    fn explicit_priority_reorders_regardless_of_file_order() {
        let dir = tempfile::tempdir().unwrap();
        let conf = dir.path().join("repos.conf");
        write(
            &conf,
            r#"
[DEFAULT]
main-repo = gentoo

[gentoo]
location = /a

[first]
location = /b
priority = -5

[second]
location = /c
priority = 10
"#,
        );
        let rc = ReposConf::load_from(&[&conf]).unwrap();
        let names: Vec<_> = rc.repos().iter().map(|r| r.name.as_str()).collect();
        // gentoo (-1000, implicit) < first (-5, explicit) < second (10, explicit)
        assert_eq!(names, vec!["gentoo", "first", "second"]);
    }

    /// A repo with an explicit priority more negative than the main repo's
    /// implicit -1000 legitimately sorts *before* it — real portage allows
    /// this (an overlay meant to shadow the main repo), so it must not be
    /// clobbered by a "main repo always first" special case.
    #[test]
    fn explicit_priority_below_main_repo_default_sorts_first() {
        let dir = tempfile::tempdir().unwrap();
        let conf = dir.path().join("repos.conf");
        write(
            &conf,
            r#"
[DEFAULT]
main-repo = gentoo

[gentoo]
location = /a

[shadow]
location = /b
priority = -2000
"#,
        );
        let rc = ReposConf::load_from(&[&conf]).unwrap();
        assert_eq!(rc.repos()[0].name, "shadow");
        assert_eq!(rc.repos()[1].name, "gentoo");
    }

    /// An explicit `priority =` on the main repo itself is respected, not
    /// silently overwritten by the `-1000` default (that default only
    /// applies when the main repo's own priority is unset).
    #[test]
    fn explicit_main_repo_priority_is_not_overridden_by_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let conf = dir.path().join("repos.conf");
        write(
            &conf,
            r#"
[DEFAULT]
main-repo = gentoo

[gentoo]
location = /a
priority = 5

[other]
location = /b
"#,
        );
        let rc = ReposConf::load_from(&[&conf]).unwrap();
        assert_eq!(rc.find("gentoo").unwrap().priority, Some(5));
        // other (unset -> 0) now sorts before gentoo (explicit 5).
        assert_eq!(rc.repos()[0].name, "other");
        assert_eq!(rc.repos()[1].name, "gentoo");
    }

    /// A non-numeric `priority =` is silently treated as unset, matching
    /// real portage's own `try: int(priority) except ValueError: None`.
    #[test]
    fn non_numeric_priority_is_treated_as_unset() {
        let dir = tempfile::tempdir().unwrap();
        let conf = dir.path().join("repos.conf");
        write(&conf, "[weird]\nlocation = /a\npriority = not-a-number\n");
        let rc = ReposConf::load_from(&[&conf]).unwrap();
        assert_eq!(rc.find("weird").unwrap().priority, None);
    }
}
