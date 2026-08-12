use std::collections::HashSet;

use camino::{Utf8Path, Utf8PathBuf};

/// Names of all known Portage sets, collected from:
///
/// - `/usr/share/portage/config/sets/*.conf` — built-in sets (ini `[name]` headers)
/// - `/etc/portage/sets.conf` — user-added set definitions
/// - `/etc/portage/sets/` — one file per user-defined static set
pub struct KnownSets {
    names: HashSet<String>,
}

impl KnownSets {
    /// Load from the given portage config root (usually `/`).
    pub fn load(root: Option<&Utf8Path>) -> Self {
        let root = root.unwrap_or(Utf8Path::new("/"));
        let mut names = HashSet::new();

        // `@preserved-rebuild` is handled directly by `emerge.rs::expand_sets`
        // (a VDB/registry query, not a config-file-defined set), so it's
        // always known even on an `em`-only root that never merged
        // `sys-apps/portage` and so lacks `.../config/sets/portage.conf`.
        names.insert("preserved-rebuild".to_string());

        // Built-in sets from /usr/share/portage/config/sets/*.conf
        let builtin_dir = root.join("usr/share/portage/config/sets");
        collect_from_conf_dir(&builtin_dir, &mut names);

        // User set config overrides/additions
        let user_conf = root.join("etc/portage/sets.conf");
        if user_conf.is_file() {
            collect_from_conf_file(&user_conf, &mut names);
        }

        // Static set files: each filename is a set name
        let sets_dir = root.join("etc/portage/sets");
        if sets_dir.is_dir() {
            collect_from_sets_dir(&sets_dir, &mut names);
        }

        Self { names }
    }

    /// Return `true` if `name` (without the `@` prefix) is a known set.
    pub fn contains(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    /// Every known set name (without the `@` prefix), unordered — callers
    /// that need a stable display order (`em --info -v`) sort it themselves.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.names.iter().map(String::as_str)
    }
}

/// Parse `[section_name]` headers from all `.conf` files in `dir`.
fn collect_from_conf_dir(dir: &Utf8Path, names: &mut HashSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<Utf8PathBuf> = entries
        .flatten()
        .filter_map(|e| {
            let p = Utf8PathBuf::try_from(e.path()).ok()?;
            if p.extension() == Some("conf") {
                Some(p)
            } else {
                None
            }
        })
        .collect();
    files.sort();
    for f in &files {
        collect_from_conf_file(f, names);
    }
}

/// Parse `[section_name]` headers from a single ini-style `.conf` file.
fn collect_from_conf_file(path: &Utf8Path, names: &mut HashSet<String>) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    for line in content.lines() {
        let line = line.trim();
        if let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let name = inner.trim();
            if !name.is_empty() {
                names.insert(name.to_string());
            }
        }
    }
}

/// Each filename (non-hidden, non-directory) in `dir` is a set name.
fn collect_from_sets_dir(dir: &Utf8Path, names: &mut HashSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file()
            && let Some(name) = path.file_name().and_then(|n| n.to_str())
            && !name.starts_with('.')
        {
            names.insert(name.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    fn make_root(conf: &str, set_files: &[&str]) -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();

        // Built-in conf
        let conf_dir = root.join("usr/share/portage/config/sets");
        std::fs::create_dir_all(&conf_dir).unwrap();
        let mut f = std::fs::File::create(conf_dir.join("portage.conf")).unwrap();
        f.write_all(conf.as_bytes()).unwrap();

        // Static set files
        let sets_dir = root.join("etc/portage/sets");
        std::fs::create_dir_all(&sets_dir).unwrap();
        for name in set_files {
            std::fs::File::create(sets_dir.join(name)).unwrap();
        }

        dir
    }

    #[test]
    fn builtin_sets_from_conf() {
        let dir = make_root("[world]\nclass = foo\n\n[system]\nclass = bar\n", &[]);
        let sets = KnownSets::load(Some(Utf8Path::from_path(dir.path()).unwrap()));
        assert!(sets.contains("world"));
        assert!(sets.contains("system"));
        assert!(!sets.contains("custom"));
    }

    #[test]
    fn user_sets_from_dir() {
        let dir = make_root("", &["myset", "other-set"]);
        let sets = KnownSets::load(Some(Utf8Path::from_path(dir.path()).unwrap()));
        assert!(sets.contains("myset"));
        assert!(sets.contains("other-set"));
    }

    #[test]
    fn hidden_files_ignored() {
        let dir = make_root("", &[".hidden"]);
        let sets = KnownSets::load(Some(Utf8Path::from_path(dir.path()).unwrap()));
        assert!(!sets.contains(".hidden"));
    }

    #[test]
    fn preserved_rebuild_is_always_known() {
        // Even with no `sys-apps/portage`-installed config/sets directory at
        // all (an `em`-only root), `@preserved-rebuild` must still validate —
        // it's computed directly by `expand_sets`, not read from disk here.
        let dir = make_root("", &[]);
        let sets = KnownSets::load(Some(Utf8Path::from_path(dir.path()).unwrap()));
        assert!(sets.contains("preserved-rebuild"));
    }
}
