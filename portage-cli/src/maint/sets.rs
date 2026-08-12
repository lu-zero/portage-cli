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

        // VDB-aware built-ins are resolved by `resolve_vdb_set` (a VDB/registry
        // query, not a config-file-defined set), so they're always known even
        // on an `em`-only root that never merged `sys-apps/portage` and so
        // lacks `.../config/sets/portage.conf`. (Other built-ins like
        // `@security` stay discovered-from-disk only — `em` can't yet resolve
        // them, so there's no point advertising them on an em-only root.)
        for name in [
            "preserved-rebuild",
            "live-rebuild",
            "deprecated-live-rebuild",
        ] {
            names.insert(name.to_string());
        }

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
/// These sets (`@preserved-rebuild`, `@live-rebuild`,
/// `@deprecated-live-rebuild`, and — to follow — `@module-rebuild`/
/// `@x11-module-rebuild`) query the installed-package database
/// (`var/db/pkg`) and/or related registries, so they can't go through
/// `portage_repo::SetResolver` (which is profile/config-only and has no VDB
/// access). Both `emerge::expand_sets` (root-target expansion) and
/// `maint::world::resolve_set` (display/audit) route VDB-aware names
/// through here first.
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
        "live-rebuild" => variable_set_atoms(&vdb, "PROPERTIES", &["live"]),
        "deprecated-live-rebuild" => variable_set_atoms(
            &vdb,
            "INHERITED",
            &[
                "bzr",
                "cvs",
                "darcs",
                "git-2",
                "git-r3",
                "golang-vcs",
                "mercurial",
                "subversion",
            ],
        ),
        _ => unreachable!("is_vdb_set_name guards the match arms above"),
    })
}

/// Whether `name` is a built-in set resolved through [`resolve_vdb_set`]
/// (rather than `SetResolver`). Kept in sync with the match arms there.
fn is_vdb_set_name(name: &str) -> bool {
    matches!(
        name,
        "preserved-rebuild" | "live-rebuild" | "deprecated-live-rebuild"
    )
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

/// `@live-rebuild` / `@deprecated-live-rebuild` core: keep every installed
/// package whose `variable` field (space-split) intersects `includes`.
///
/// Matches portage's `VariableSet` (`portage/_sets/dbapi.py:146`) with
/// `metadata-source` defaulting to `vartree` (the VDB itself), non-`*DEPEND`
/// branch (`_filter`, lines 213-219): non-empty `includes ∩ values` → kept.
/// Atoms are emitted as `cat/pkg:{main_slot}` — portage's `EverythingSet.load`
/// builds `Atom(f"{pkg.cp}:{pkg.slot}")`, and `_pkg_str.slot` is the main
/// slot only (subslot kept separate); defaulting to `"0"` mirrors portage's
/// `slot_invalid` fallback.
fn variable_set_atoms(
    vdb: &portage_vdb::Vdb,
    variable: &str,
    includes: &[&str],
) -> Result<Vec<Dep>> {
    let mut out = Vec::new();
    for pkg in vdb.packages() {
        let Some(value) = pkg.field(variable)? else {
            continue;
        };
        if !value.split_whitespace().any(|tok| includes.contains(&tok)) {
            continue;
        }
        let slot = pkg
            .slot_main()
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "0".to_string());
        let atom = format!("{}:{slot}", pkg.cpn());
        out.push(Dep::parse(&atom).with_context(|| format!("parsing set atom {atom}"))?);
    }
    Ok(out)
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

    // --- @live-rebuild / @deprecated-live-rebuild (VariableSet) ---

    /// Write a minimal VDB package dir under `eroot/var/db/pkg/<cat>/<pf>`
    /// with the given metadata fields. Enumeration only needs the dir + a
    /// parseable pf, but the variable/slot accessors read files, so callers
    /// pass the fields they need (`SLOT`, `PROPERTIES`, `INHERITED`, …).
    fn write_vdb_pkg(eroot: &Utf8Path, cat_pf: &str, fields: &[(&str, &str)]) {
        let dir = eroot.join("var/db/pkg").join(cat_pf);
        std::fs::create_dir_all(&dir).unwrap();
        for (k, v) in fields {
            std::fs::write(dir.join(k), v).unwrap();
        }
    }

    #[test]
    fn live_rebuild_keeps_only_packages_with_properties_live() {
        let (_keep, eroot) = vdb_eroot();
        write_vdb_pkg(
            &eroot,
            "app-misc/live-one-1.0",
            &[("SLOT", "0"), ("PROPERTIES", "live")],
        );
        write_vdb_pkg(
            &eroot,
            "app-misc/normal-1.0",
            &[("SLOT", "0"), ("PROPERTIES", "foo bar")],
        );
        // No PROPERTIES file at all → field is None → not included.
        write_vdb_pkg(&eroot, "app-misc/noprops-1.0", &[("SLOT", "0")]);

        let atoms: Vec<String> = resolve_vdb_set("live-rebuild", &eroot)
            .expect("Some")
            .unwrap()
            .into_iter()
            .map(|d| d.to_string())
            .collect();
        assert_eq!(atoms, vec!["app-misc/live-one:0"]);
    }

    #[test]
    fn live_rebuild_atom_uses_main_slot_not_subslot() {
        // Portage's `_pkg_str.slot` is the main slot only; the atom emitted
        // is `cat/pkg:{main}`, not `:{main}/{subslot}`. SLOT="2/5.1" → `:2`.
        let (_keep, eroot) = vdb_eroot();
        write_vdb_pkg(
            &eroot,
            "sys-libs/foo-1.0",
            &[("SLOT", "2/5.1"), ("PROPERTIES", "live")],
        );
        let atoms: Vec<String> = resolve_vdb_set("live-rebuild", &eroot)
            .expect("Some")
            .unwrap()
            .into_iter()
            .map(|d| d.to_string())
            .collect();
        assert_eq!(atoms, vec!["sys-libs/foo:2"]);
    }

    #[test]
    fn live_rebuild_matches_live_among_multiple_properties_tokens() {
        // PROPERTIES is space-split; `live` need only be one of the tokens.
        let (_keep, eroot) = vdb_eroot();
        write_vdb_pkg(
            &eroot,
            "app-misc/multi-1.0",
            &[("SLOT", "0"), ("PROPERTIES", "interactive live")],
        );
        let atoms: Vec<String> = resolve_vdb_set("live-rebuild", &eroot)
            .expect("Some")
            .unwrap()
            .into_iter()
            .map(|d| d.to_string())
            .collect();
        assert_eq!(atoms, vec!["app-misc/multi:0"]);
    }

    #[test]
    fn deprecated_live_rebuild_matches_inherited_vcs_eclasses() {
        // variable=INHERITED, includes the LIVE_ECLASSES set. A package that
        // inherited any of them is kept; one with unrelated eclasses is not.
        let (_keep, eroot) = vdb_eroot();
        write_vdb_pkg(
            &eroot,
            "dev-vcs/git-2.45",
            &[("SLOT", "0"), ("INHERITED", "toolchain git-r3")],
        );
        write_vdb_pkg(
            &eroot,
            "app-misc/plain-1.0",
            &[("SLOT", "0"), ("INHERITED", "cmake-utils")],
        );

        let atoms: Vec<String> = resolve_vdb_set("deprecated-live-rebuild", &eroot)
            .expect("Some")
            .unwrap()
            .into_iter()
            .map(|d| d.to_string())
            .collect();
        assert_eq!(atoms, vec!["dev-vcs/git:0"]);
    }

    #[test]
    fn live_rebuild_empty_vdb_resolves_to_empty_not_error() {
        let (_keep, eroot) = vdb_eroot();
        let res = resolve_vdb_set("live-rebuild", &eroot);
        assert!(matches!(res, Some(Ok(ref atoms)) if atoms.is_empty()));
        let res = resolve_vdb_set("deprecated-live-rebuild", &eroot);
        assert!(matches!(res, Some(Ok(ref atoms)) if atoms.is_empty()));
    }
}
