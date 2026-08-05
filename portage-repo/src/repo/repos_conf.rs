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
    /// Names of master repositories (often empty; layout.conf normally wins).
    pub masters: Vec<String>,
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
                let masters = s
                    .get("masters")
                    .map(|v| v.split_whitespace().map(String::from).collect())
                    .unwrap_or_default();
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
                })
            })
            .collect();

        if let Some(main) = main_repo.as_deref()
            && let Some(pos) = repos.iter().position(|r| r.name == main)
            && pos != 0
        {
            let m = repos.remove(pos);
            repos.insert(0, m);
        }

        Ok(ReposConf { repos, main_repo })
    }

    /// Every configured repository in resolution order (main first).
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
        assert_eq!(rc.repos()[1].masters, vec!["gentoo"]);
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
    }
}
