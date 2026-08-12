use std::collections::HashSet;

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use portage_atom::Dep;

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

        // `@preserved-rebuild` is resolved by `resolve_vdb_set` (a VDB/registry
        // query, not a config-file-defined set), so it's always known even on
        // an `em`-only root that never merged `sys-apps/portage` and so lacks
        // `.../config/sets/portage.conf`.
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

/// Resolve a VDB-aware built-in set under `eroot`.
///
/// `@preserved-rebuild` (and — to follow — `@live-rebuild`,
/// `@deprecated-live-rebuild`, `@module-rebuild`, `@x11-module-rebuild`)
/// queries the installed-package database (`var/db/pkg`) and/or related
/// registries, so it can't go through `portage_repo::SetResolver` (which is
/// profile/config-only and has no VDB access). Both `emerge::expand_sets`
/// (root-target expansion) and `maint::world::resolve_set` (display/audit)
/// route VDB-aware names through here first.
///
/// Returns `None` when `name` is not a VDB-aware built-in (caller falls
/// back to `SetResolver`); `Some(Ok(atoms))` on success; `Some(Err(_))`
/// when the name is recognized but its VDB query failed. Callers decide
/// how to handle the error (warn-and-skip vs propagate) — they differ
/// deliberately between the two call sites.
pub(crate) fn resolve_vdb_set(name: &str, eroot: &Utf8Path) -> Option<Result<Vec<Dep>>> {
    // Only open the VDB for names we actually recognize; non-VDB names
    // (`@system`, `@world`, user sets) must fall through to `SetResolver`
    // without requiring a readable `var/db/pkg` at all.
    if !is_vdb_set_name(name) {
        return None;
    }
    let vdb = match portage_vdb::Vdb::open(eroot.join("var/db/pkg")) {
        Ok(v) => v,
        Err(e) => {
            return Some(Err(e).with_context(|| format!("opening VDB under {eroot}")));
        }
    };
    Some(match name {
        "preserved-rebuild" => preserved_rebuild_atoms(&vdb, eroot),
        _ => unreachable!("is_vdb_set_name guards the match arms above"),
    })
}

/// Whether `name` is a built-in set resolved through [`resolve_vdb_set`]
/// (rather than `SetResolver`). Kept in sync with the match arms there.
fn is_vdb_set_name(name: &str) -> bool {
    matches!(name, "preserved-rebuild")
}

/// `@preserved-rebuild`: packages owning a shared lib whose last provider was
/// just unmerged, so they need rebuilding against the surviving provider.
/// Sourced from the preserve-libs registry + a link scan of the VDB.
fn preserved_rebuild_atoms(vdb: &portage_vdb::Vdb, eroot: &Utf8Path) -> Result<Vec<Dep>> {
    let registry = crate::preserve_libs::PreservedLibsRegistry::load(eroot);
    Ok(crate::preserve_libs::preserved_rebuild_atoms(
        vdb, &registry, eroot,
    ))
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
        // it's resolved by `resolve_vdb_set`, not read from disk here.
        let dir = make_root("", &[]);
        let sets = KnownSets::load(Some(Utf8Path::from_path(dir.path()).unwrap()));
        assert!(sets.contains("preserved-rebuild"));
    }

    // --- resolve_vdb_set dispatch ---

    /// A scratch eroot with an (empty) `var/db/pkg`, enough for the
    /// VDB-aware resolvers to open without error.
    fn vdb_eroot() -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempdir().unwrap();
        let eroot = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        std::fs::create_dir_all(eroot.join("var/db/pkg")).unwrap();
        (dir, eroot)
    }

    #[test]
    fn resolve_vdb_set_returns_none_for_non_vdb_names() {
        let (_keep, eroot) = vdb_eroot();
        assert!(resolve_vdb_set("system", &eroot).is_none());
        assert!(resolve_vdb_set("my-user-set", &eroot).is_none());
    }

    #[test]
    fn resolve_vdb_set_preserved_rebuild_is_empty_with_empty_vdb() {
        // No packages installed → no consumers of any preserved lib → empty.
        // This is the dispatch-level analogue of `expand_sets`' own
        // `expand_sets_preserved_rebuild_is_empty_with_no_registry`.
        let (_keep, eroot) = vdb_eroot();
        let res = resolve_vdb_set("preserved-rebuild", &eroot);
        assert!(matches!(res, Some(Ok(ref atoms)) if atoms.is_empty()));
    }
}
