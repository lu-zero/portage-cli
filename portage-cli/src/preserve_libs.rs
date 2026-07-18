//! portage's `preserve-libs`: never physically delete a shared library that
//! another still-installed object's `NEEDED.ELF.2` genuinely requires and
//! that no other installed copy still provides — matching real portage's
//! `_find_libs_to_preserve`/`PreservedLibsRegistry`
//! (`portage/dbapi/vartree.py`, `util/_dyn_libs/{LinkageMapELF,
//! PreservedLibsRegistry}.py`), simplified to match by (multilib category,
//! soname) alone rather than full RPATH/RUNPATH-directory resolution. That
//! can only make `em` preserve a file real portage would have deleted,
//! never the reverse — the safe direction for a deletion guard.
//!
//! Hooked into `ebuild::unmerge_package`, the one function both `-C` and
//! ordinary version-bump replacement funnel through, so both benefit.

use std::collections::{HashMap, HashSet};

use camino::{Utf8Path, Utf8PathBuf};

use portage_atom::Cpv;
use portage_vdb::{ContentsEntry, ContentsKind, InstalledPackage, Vdb};

use crate::elfscan;
use crate::util::write_atomic;

/// One parsed `NEEDED.ELF.2` line: `MACHINE;path;SONAME;RPATH;needed,csv;category`
/// — the exact format `elfscan::scan_image` writes.
struct NeededRecord {
    category: String,
    path: Utf8PathBuf,
    soname: Option<String>,
    needed: Vec<String>,
}

fn parse_needed_line(line: &str) -> Option<NeededRecord> {
    let mut parts = line.splitn(6, ';');
    let _machine = parts.next()?;
    let path = parts.next()?;
    let soname = parts.next()?;
    let _rpath = parts.next()?;
    let needed = parts.next()?;
    let category = parts.next()?;
    Some(NeededRecord {
        category: category.to_string(),
        path: Utf8PathBuf::from(path),
        soname: (!soname.is_empty()).then(|| soname.to_string()),
        needed: needed
            .split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
    })
}

fn package_needed(pkg: &InstalledPackage) -> Vec<NeededRecord> {
    pkg.field("NEEDED.ELF.2")
        .ok()
        .flatten()
        .map(|content| content.lines().filter_map(parse_needed_line).collect())
        .unwrap_or_default()
}

/// One `cp:slot`'s registered preserved-lib set. `paths` are stored as
/// plain `String`s (not `Utf8PathBuf`) since `camino` doesn't implement
/// `serde` traits without an extra feature flag on a shared workspace dep —
/// converted at the [`PreservedLibsRegistry`] API boundary instead.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct RegistryEntry {
    cpv: String,
    counter: u64,
    paths: Vec<String>,
}

/// `var/lib/portage/preserved_libs_registry` — real portage's own exact
/// relative path, so it's recognizable to anyone inspecting the root. JSON
/// map of `"cp:slot" -> {cpv, counter, paths}`; close to real portage's
/// shape without needing to be byte-compatible (it's internal state either
/// way, not a PMS-defined format).
pub struct PreservedLibsRegistry {
    path: Utf8PathBuf,
    data: HashMap<String, RegistryEntry>,
}

impl PreservedLibsRegistry {
    /// Load the registry under `root` (the merge root / EROOT), or start
    /// empty if it doesn't exist yet or fails to parse.
    pub fn load(root: &Utf8Path) -> Self {
        let path = root.join("var/lib/portage/preserved_libs_registry");
        let data = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { path, data }
    }

    /// Write the registry back out. Best-effort: a failure here shouldn't
    /// abort an unmerge that already completed on disk.
    pub fn store(&self) {
        match serde_json::to_string_pretty(&self.data) {
            Ok(json) => {
                if let Err(e) = write_atomic(&self.path, json) {
                    eprintln!("warning: could not write {}: {e:#}", self.path);
                }
            }
            Err(e) => eprintln!("warning: could not serialize preserved_libs_registry: {e}"),
        }
    }

    /// Register (or clear, if `paths` is empty) the preserved-lib set for
    /// one `cp:slot` — matching real portage's `register`/`unregister`
    /// (the same call; empty paths means unregister).
    pub fn register(&mut self, cpv: &Cpv, slot: &str, counter: u64, paths: Vec<Utf8PathBuf>) {
        let key = format!("{}:{slot}", cpv.cpn);
        if paths.is_empty() {
            self.data.remove(&key);
        } else {
            self.data.insert(
                key,
                RegistryEntry {
                    cpv: cpv.to_string(),
                    counter,
                    paths: paths.into_iter().map(Utf8PathBuf::into_string).collect(),
                },
            );
        }
    }

    /// Reclaim preserved-libs after a merge or unmerge batch:
    ///
    /// 1. Drop registry paths that a live package's CONTENTS now owns (do not
    ///    delete — the live package owns them).
    /// 2. Delete remaining preserved files whose soname is no longer needed by
    ///    any live package's `NEEDED.ELF.2`, and drop them from the registry.
    ///
    /// Best-effort: deletion failures are logged and the path stays registered.
    pub fn reclaim(&mut self, vdb: &Vdb, root: &Utf8Path) {
        if self.data.is_empty() {
            return;
        }
        self.reclaim_provided(vdb);
        self.prune_unneeded(vdb, root);
    }

    /// Drop registry paths that a currently installed package's CONTENTS now
    /// owns (without deleting — the live package owns them).
    pub fn reclaim_provided(&mut self, vdb: &Vdb) {
        if self.data.is_empty() {
            return;
        }
        let mut live_paths: HashSet<Utf8PathBuf> = HashSet::new();
        for pkg in vdb.packages() {
            if let Ok(contents) = pkg.contents() {
                for e in contents {
                    if matches!(e.kind, ContentsKind::Obj | ContentsKind::Sym) {
                        live_paths.insert(e.path);
                    }
                }
            }
        }
        let mut remove_keys = Vec::new();
        let mut updates: Vec<(String, Vec<String>)> = Vec::new();
        for (key, entry) in &self.data {
            let remaining: Vec<String> = entry
                .paths
                .iter()
                .filter(|p| !live_paths.contains(Utf8Path::new(p.as_str())))
                .cloned()
                .collect();
            if remaining.len() == entry.paths.len() {
                continue;
            }
            let reclaimed = entry.paths.len() - remaining.len();
            println!(">>> preserved-libs: reclaimed {reclaimed} path(s) from {key}");
            if remaining.is_empty() {
                remove_keys.push(key.clone());
            } else {
                updates.push((key.clone(), remaining));
            }
        }
        for key in remove_keys {
            self.data.remove(&key);
        }
        for (key, remaining) in updates {
            if let Some(entry) = self.data.get_mut(&key) {
                entry.paths = remaining;
            }
        }
    }

    /// Delete preserved files no live package still needs via `DT_NEEDED`.
    fn prune_unneeded(&mut self, vdb: &Vdb, root: &Utf8Path) {
        if self.data.is_empty() {
            return;
        }
        // (multilib category, soname) still required by something installed.
        let mut needed: HashSet<(String, String)> = HashSet::new();
        for pkg in vdb.packages() {
            for rec in package_needed(&pkg) {
                for n in rec.needed {
                    needed.insert((rec.category.clone(), n));
                }
            }
        }

        let mut remove_keys = Vec::new();
        let mut updates: Vec<(String, Vec<String>)> = Vec::new();
        for (key, entry) in &self.data {
            let mut remaining = Vec::new();
            for path_s in &entry.paths {
                let path = Utf8Path::new(path_s);
                let rel = path_s.trim_start_matches('/');
                let abs = root.join(rel);
                if !abs.exists() {
                    println!(">>> preserved-libs: dropped missing {path}");
                    continue;
                }
                let still_needed = elfscan::scan_file(abs.as_std_path())
                    .and_then(|info| {
                        let soname = info.soname?;
                        Some(needed.contains(&(info.category, soname)))
                    })
                    // Unreadable / non-ELF: keep (safe direction).
                    .unwrap_or(true);
                if still_needed {
                    remaining.push(path_s.clone());
                    continue;
                }
                // Orphan preserved file — remove from disk.
                match std::fs::remove_file(abs.as_std_path()) {
                    Ok(()) => {
                        println!(">>> preserved-libs: removed unused {path}");
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        println!(">>> preserved-libs: dropped missing {path}");
                    }
                    Err(e) => {
                        eprintln!("warning: preserved-libs: could not remove {path}: {e}");
                        remaining.push(path_s.clone());
                    }
                }
            }
            if remaining.len() == entry.paths.len() {
                continue;
            }
            if remaining.is_empty() {
                remove_keys.push(key.clone());
            } else {
                updates.push((key.clone(), remaining));
            }
        }
        for key in remove_keys {
            self.data.remove(&key);
        }
        for (key, remaining) in updates {
            if let Some(entry) = self.data.get_mut(&key) {
                entry.paths = remaining;
            }
        }
    }

    /// Every path currently tracked as preserved, across all packages —
    /// these no longer appear in any live package's CONTENTS, so callers
    /// needing their link metadata must re-scan them directly
    /// ([`elfscan::scan_file`]).
    fn all_paths(&self) -> impl Iterator<Item = Utf8PathBuf> {
        self.data
            .values()
            .flat_map(|e| e.paths.iter())
            .map(Utf8PathBuf::from)
    }
}

/// A library kept on disk because [`find_libs_to_preserve`] found it still
/// has external consumers.
pub struct PreserveEntry {
    pub path: Utf8PathBuf,
    /// The soname symlink pointing at `path`, if one exists in the same
    /// package's contents (kept alongside — it's the only symlink form a
    /// consumer's dynamic loader actually looks up by name).
    pub soname_symlink: Option<Utf8PathBuf>,
    /// Files (belonging to other, still-installed packages) whose
    /// `DT_NEEDED` requires this library's soname — for the user-facing
    /// report.
    pub consumers: Vec<Utf8PathBuf>,
}

/// The workspace-wide `(category, soname)` provider/consumer index —
/// everything [`find_libs_to_preserve`] needs to decide what to preserve,
/// minus the one package actually being removed. Expensive to build (a
/// full VDB scan), cheap to query — build **once per invocation** (not
/// once per removed package) and reuse it across an entire `-C`/depclean
/// batch. Correct to share across the whole batch because `exclude`
/// already names every cpv the batch is committed to removing, so the
/// graph doesn't change as individual removals within it actually happen.
pub struct LinkGraph {
    providers: HashSet<(String, String)>,
    consumer_files: HashMap<(String, String), Vec<Utf8PathBuf>>,
}

/// Build the [`LinkGraph`] for a removal batch: every installed package
/// **not** in `exclude`, plus every registry-carried preserved lib from a
/// previous run (re-scanned directly via [`elfscan::scan_file`] — no
/// longer part of any live package's CONTENTS/NEEDED.ELF.2).
///
/// `exclude` is every cpv this same `-C`/depclean/replace invocation is
/// already committed to removing (so a multi-atom `em -C a b`, where `a`
/// needs a lib only `b` provides, doesn't falsely think `b` still
/// provides it).
pub fn build_link_graph(
    vdb: &Vdb,
    exclude: &HashSet<Cpv>,
    registry: &PreservedLibsRegistry,
    root: &Utf8Path,
) -> LinkGraph {
    let mut providers: HashSet<(String, String)> = HashSet::new();
    let mut consumer_files: HashMap<(String, String), Vec<Utf8PathBuf>> = HashMap::new();

    for pkg in vdb.packages() {
        if exclude.contains(pkg.cpv()) {
            continue;
        }
        for rec in package_needed(&pkg) {
            if let Some(soname) = &rec.soname {
                providers.insert((rec.category.clone(), soname.clone()));
            }
            for needed in &rec.needed {
                consumer_files
                    .entry((rec.category.clone(), needed.clone()))
                    .or_default()
                    .push(rec.path.clone());
            }
        }
    }

    // Registry-carried preserved libs from a previous run: no longer part
    // of any live package's CONTENTS/NEEDED.ELF.2, so re-scan them
    // directly to know what they still provide.
    for path in registry.all_paths() {
        let rel = path.as_str().trim_start_matches('/');
        if let Some(info) = elfscan::scan_file(root.join(rel).as_std_path())
            && let Some(soname) = info.soname
        {
            providers.insert((info.category, soname));
        }
    }

    LinkGraph {
        providers,
        consumer_files,
    }
}

/// Which of `old_contents`' own files must survive `old_pkg`'s removal,
/// because something outside the batch `graph` was built for still needs
/// them via `DT_NEEDED`, and no other installed copy of that soname
/// remains. Cheap: only reads `old_pkg`'s own `NEEDED.ELF.2` and queries
/// the already-built `graph` — call once per removed package, reusing the
/// same `graph` for the whole batch (see [`build_link_graph`]'s doc).
pub fn find_libs_to_preserve(
    graph: &LinkGraph,
    old_pkg: &InstalledPackage,
    old_contents: &[ContentsEntry],
) -> Vec<PreserveEntry> {
    let mut preserve = Vec::new();
    for rec in package_needed(old_pkg) {
        let Some(soname) = rec.soname else { continue };
        let key = (rec.category.clone(), soname.clone());
        // Nobody outside this package needs it -> nothing to preserve.
        if !graph.consumer_files.contains_key(&key) {
            continue;
        }
        // Another installed copy already provides it -> nothing orphaned.
        if graph.providers.contains(&key) {
            continue;
        }
        let Some(entry) = old_contents
            .iter()
            .find(|e| e.kind == ContentsKind::Obj && e.path == rec.path)
        else {
            continue;
        };
        let soname_symlink = old_contents
            .iter()
            .find(|e| e.kind == ContentsKind::Sym && e.path.file_name() == Some(soname.as_str()))
            .map(|e| e.path.clone());
        preserve.push(PreserveEntry {
            path: entry.path.clone(),
            soname_symlink,
            consumers: graph.consumer_files.get(&key).cloned().unwrap_or_default(),
        });
    }
    preserve
}

/// Real-portage-style report: `>>> package: <cpv>` / ` * preserved: <path>`
/// / `      used by <path> (<owning cpv>)`, shown both for a real run and
/// under `-p`.
pub fn report_preserved(cpv: &Cpv, preserved: &[PreserveEntry], vdb: &Vdb) {
    const MAX_DISPLAY: usize = 3;
    if preserved.is_empty() {
        return;
    }
    println!(">>> package: {cpv}");
    for entry in preserved {
        println!(" * preserved: {}", entry.path);
        if let Some(sym) = &entry.soname_symlink {
            println!(" * preserved: {sym}");
        }
        for c in entry.consumers.iter().take(MAX_DISPLAY) {
            let owner = vdb
                .owner(c)
                .map(|p| p.cpv().to_string())
                .unwrap_or_else(|| "unknown".to_string());
            println!("      used by {c} ({owner})");
        }
        if entry.consumers.len() > MAX_DISPLAY {
            println!(
                "      used by {} other file(s)",
                entry.consumers.len() - MAX_DISPLAY
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contents(entries: &[(ContentsKind, &str, Option<&str>)]) -> Vec<ContentsEntry> {
        entries
            .iter()
            .map(|(kind, path, target)| ContentsEntry {
                kind: kind.clone(),
                path: Utf8PathBuf::from(*path),
                md5: None,
                mtime: None,
                target: target.map(Utf8PathBuf::from),
            })
            .collect()
    }

    #[test]
    fn parse_needed_line_roundtrips_scan_image_format() {
        let rec = parse_needed_line("X86_64;/usr/lib64/libfoo.so.1;libfoo.so.1;;libc.so.6;x86_64")
            .unwrap();
        assert_eq!(rec.category, "x86_64");
        assert_eq!(rec.path, Utf8PathBuf::from("/usr/lib64/libfoo.so.1"));
        assert_eq!(rec.soname.as_deref(), Some("libfoo.so.1"));
        assert_eq!(rec.needed, vec!["libc.so.6"]);
    }

    #[test]
    fn parse_needed_line_handles_empty_soname_and_needed() {
        let rec = parse_needed_line("X86_64;/usr/bin/tool;;;;x86_64").unwrap();
        assert_eq!(rec.soname, None);
        assert!(rec.needed.is_empty());
    }

    #[test]
    fn registry_register_then_unregister_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(dir.path().to_owned()).unwrap();
        let cpv = Cpv::parse("sys-libs/foo-1.0").unwrap();

        let mut reg = PreservedLibsRegistry::load(&root);
        reg.register(
            &cpv,
            "0",
            1,
            vec![Utf8PathBuf::from("/usr/lib64/libfoo.so.1")],
        );
        reg.store();

        let reloaded = PreservedLibsRegistry::load(&root);
        assert_eq!(reloaded.all_paths().count(), 1);

        let mut reg2 = PreservedLibsRegistry::load(&root);
        reg2.register(&cpv, "0", 1, vec![]);
        reg2.store();
        let reloaded2 = PreservedLibsRegistry::load(&root);
        assert_eq!(reloaded2.all_paths().count(), 0);
    }

    #[test]
    fn reclaim_provided_drops_paths_owned_by_live_packages() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        let vdb_root = root.join("var/db/pkg");
        // Live package that "provides" the previously preserved path.
        write_fake_package(
            vdb_root.as_std_path(),
            "sys-libs",
            "libfoo-2.0",
            "obj /usr/lib64/libfoo.so.1 aaaa 0\n",
            "X86_64;/usr/lib64/libfoo.so.1;libfoo.so.1;;;x86_64\n",
        );
        let mut reg = PreservedLibsRegistry::load(&root);
        reg.register(
            &Cpv::parse("sys-libs/libfoo-1.0").unwrap(),
            "0",
            1,
            vec![
                Utf8PathBuf::from("/usr/lib64/libfoo.so.1"),
                Utf8PathBuf::from("/usr/lib64/liborphan.so.1"),
            ],
        );
        let vdb = Vdb::open(&vdb_root).unwrap();
        reg.reclaim(&vdb, &root);
        // libfoo path reclaimed (live-owned); orphan has no on-disk ELF so
        // prune_unneeded keeps it (safe direction) unless missing entirely.
        let paths: HashSet<_> = reg.all_paths().collect();
        assert!(!paths.contains(&Utf8PathBuf::from("/usr/lib64/libfoo.so.1")));
        // Missing file is dropped by prune_unneeded's NotFound branch.
        assert!(!paths.contains(&Utf8PathBuf::from("/usr/lib64/liborphan.so.1")));
    }

    #[test]
    fn reclaim_drops_missing_orphan_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        let mut reg = PreservedLibsRegistry::load(&root);
        reg.register(
            &Cpv::parse("sys-libs/old-1.0").unwrap(),
            "0",
            1,
            vec![Utf8PathBuf::from("/usr/lib64/gone.so.1")],
        );
        // Empty VDB — nothing needs the path, and the file is missing.
        let vdb_root = root.join("var/db/pkg");
        std::fs::create_dir_all(&vdb_root).unwrap();
        let vdb = Vdb::open(&vdb_root).unwrap();
        reg.reclaim(&vdb, &root);
        assert_eq!(reg.all_paths().count(), 0);
    }

    #[test]
    fn contents_shape_is_well_formed() {
        // Exercises the `contents()` test helper itself so it's covered
        // independently of the VDB-backed tests below.
        let c = contents(&[(ContentsKind::Dir, "/usr/lib64", None)]);
        assert_eq!(c.len(), 1);
    }

    /// Write one fake installed package into `vdb_root/<cat>/<pf>/`: a
    /// `CONTENTS` file (real portage line format) and a `NEEDED.ELF.2` file
    /// (the exact format [`parse_needed_line`] parses).
    fn write_fake_package(
        vdb_root: &std::path::Path,
        cat: &str,
        pf: &str,
        contents_lines: &str,
        needed_lines: &str,
    ) {
        let dir = vdb_root.join(cat).join(pf);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("CONTENTS"), contents_lines).unwrap();
        std::fs::write(dir.join("NEEDED.ELF.2"), needed_lines).unwrap();
    }

    #[test]
    fn preserves_lib_still_needed_by_another_installed_package() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        let vdb_root = root.join("var/db/pkg");

        write_fake_package(
            vdb_root.as_std_path(),
            "sys-libs",
            "libfoo-1.0",
            "obj /usr/lib64/libfoo.so.1 aaaa 0\n",
            "X86_64;/usr/lib64/libfoo.so.1;libfoo.so.1;;;x86_64\n",
        );
        write_fake_package(
            vdb_root.as_std_path(),
            "app-misc",
            "consumer-1.0",
            "obj /usr/bin/consumer bbbb 0\n",
            "X86_64;/usr/bin/consumer;;;libfoo.so.1;x86_64\n",
        );

        let vdb = Vdb::open(&vdb_root).unwrap();
        let libfoo = vdb
            .category("sys-libs")
            .unwrap()
            .package("libfoo-1.0")
            .unwrap();
        let old_contents = libfoo.contents().unwrap();
        let exclude: HashSet<Cpv> = std::iter::once(libfoo.cpv().clone()).collect();
        let registry = PreservedLibsRegistry::load(&root);

        let graph = build_link_graph(&vdb, &exclude, &registry, &root);
        let preserved = find_libs_to_preserve(&graph, &libfoo, &old_contents);

        assert_eq!(preserved.len(), 1);
        assert_eq!(
            preserved[0].path,
            Utf8PathBuf::from("/usr/lib64/libfoo.so.1")
        );
        assert_eq!(
            preserved[0].consumers,
            vec![Utf8PathBuf::from("/usr/bin/consumer")]
        );
    }

    #[test]
    fn does_not_preserve_lib_with_no_external_consumers() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        let vdb_root = root.join("var/db/pkg");

        write_fake_package(
            vdb_root.as_std_path(),
            "sys-libs",
            "libfoo-1.0",
            "obj /usr/lib64/libfoo.so.1 aaaa 0\n",
            "X86_64;/usr/lib64/libfoo.so.1;libfoo.so.1;;;x86_64\n",
        );

        let vdb = Vdb::open(&vdb_root).unwrap();
        let libfoo = vdb
            .category("sys-libs")
            .unwrap()
            .package("libfoo-1.0")
            .unwrap();
        let old_contents = libfoo.contents().unwrap();
        let exclude: HashSet<Cpv> = std::iter::once(libfoo.cpv().clone()).collect();
        let registry = PreservedLibsRegistry::load(&root);

        let graph = build_link_graph(&vdb, &exclude, &registry, &root);
        let preserved = find_libs_to_preserve(&graph, &libfoo, &old_contents);

        assert!(preserved.is_empty());
    }
}
