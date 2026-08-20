use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use memchr::memmem;
use portage_atom::{Cpn, Dep, Slot, SlotDep};
use portage_vdb::{ContentsKind, ContentsRef, InstalledPackage};

/// Names of all known Portage sets, collected from:
///
/// - `/usr/share/portage/config/sets/*.conf` — built-in sets (ini `[name]` headers)
/// - `/etc/portage/sets.conf` — user-added set definitions
/// - `/etc/portage/sets/` — one file per user-defined static set
pub struct KnownSets {
    names: HashSet<String>,
    /// The subset declared by a `.conf` section rather than a static set
    /// file — Portage defines these through a `class = portage.sets.…`, so a
    /// resolution failure means `em` has no implementation, not that the
    /// name is bogus. See [`Self::is_declared`].
    declared: HashSet<String>,
}

impl KnownSets {
    /// Load from the given portage config root (usually `/`)
    pub fn load(root: Option<&Utf8Path>) -> Self {
        let root = root.unwrap_or(Utf8Path::new("/"));
        let mut names = HashSet::new();
        let mut declared = HashSet::new();

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
            "module-rebuild",
            "x11-module-rebuild",
            "security",
        ] {
            names.insert(name.to_string());
        }

        // Built-in sets from /usr/share/portage/config/sets/*.conf
        let builtin_dir = root.join("usr/share/portage/config/sets");
        collect_from_conf_dir(&builtin_dir, &mut declared);

        // User set config overrides/additions
        let user_conf = root.join("etc/portage/sets.conf");
        if user_conf.is_file() {
            collect_from_conf_file(&user_conf, &mut declared);
        }

        names.extend(declared.iter().cloned());

        // Static set files: each filename is a set name
        let sets_dir = root.join("etc/portage/sets");
        if sets_dir.is_dir() {
            collect_from_sets_dir(&sets_dir, &mut names);
        }

        Self { names, declared }
    }

    /// Return `true` if `name` (without the `@` prefix) is a known set
    pub fn contains(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    /// Whether a `.conf` section declares `name` — i.e. Portage defines the
    /// set even though this resolver may have no implementation for it.
    pub fn is_declared(&self, name: &str) -> bool {
        self.declared.contains(name)
    }

    /// Every known set name (without the `@` prefix), unordered — callers
    /// that need a stable display order (`em --info -v`) sort it themselves.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.names.iter().map(String::as_str)
    }
}

/// Which VDB-aware built-in set a name refers to, if any — the single
/// source of truth for both "is this name VDB-aware" (`vdb_set_kind(name)
/// .is_some()`) and dispatch ([`resolve_vdb_set`]). Previously these were
/// two independently hand-synced lists (a `matches!` and a `match` with an
/// `unreachable!()` fallback); a name added to one and not the other would
/// have turned a resolvable set into a panic.
enum VdbSet {
    Preserved,
    Live,
    DeprecatedLive,
    Module,
    X11Module,
}

fn vdb_set_kind(name: &str) -> Option<VdbSet> {
    Some(match name {
        "preserved-rebuild" => VdbSet::Preserved,
        "live-rebuild" => VdbSet::Live,
        "deprecated-live-rebuild" => VdbSet::DeprecatedLive,
        "module-rebuild" => VdbSet::Module,
        "x11-module-rebuild" => VdbSet::X11Module,
        _ => return None,
    })
}

/// Resolve a VDB-aware built-in set under `eroot`
///
/// These sets (`@preserved-rebuild`, `@live-rebuild`,
/// `@deprecated-live-rebuild`, and — to follow — `@module-rebuild`/
/// `@x11-module-rebuild`) query the installed-package database and/or
/// related registries, so they can't go through `portage_repo::SetResolver`
/// (profile/config-only, no VDB access). Both `emerge::expand_sets` and
/// `maint::world::resolve_set` route VDB-aware names through here first.
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
    let kind = vdb_set_kind(name)?;
    let vdb = match portage_vdb::Vdb::open(eroot.join("var/db/pkg")) {
        Ok(v) => v,
        Err(e) => {
            return Some(Err(e).with_context(|| format!("opening VDB under {eroot}")));
        }
    };
    Some(match kind {
        VdbSet::Preserved => preserved_rebuild_atoms(&vdb, eroot),
        VdbSet::Live => variable_set_atoms(&vdb, "PROPERTIES", &["live"]),
        VdbSet::DeprecatedLive => variable_set_atoms(
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
        VdbSet::Module => owner_set_atoms(&vdb, &["/lib/modules"], &["/usr/src/linux*"], eroot),
        VdbSet::X11Module => {
            owner_set_atoms(&vdb, &["/usr/lib*/xorg/modules"], &["/usr/bin/Xorg"], eroot)
        }
    })
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
/// Matches portage's `VariableSet` with `metadata-source` defaulting to
/// `vartree` (the VDB itself), non-`*DEPEND` branch: non-empty
/// `includes ∩ values` → kept.
///
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
        // A single package's unreadable field (corrupted/mid-removal VDB
        // entry) skips just that package rather than aborting the whole set —
        // the same leniency `owner_set_atoms` gives an unreadable CONTENTS.
        let Some(value) = pkg.field(variable).ok().flatten() else {
            continue;
        };
        if !value.split_whitespace().any(|tok| includes.contains(&tok)) {
            continue;
        }
        // An unreadable/corrupted SLOT is the same VDB-read failure
        // `preserved_rebuild_atoms` skips the package for; matched here too
        // (see `slot_member`) rather than fabricating a `:0` atom.
        if let Some(member) = slot_member(&pkg) {
            out.push(set_atom(member));
        }
    }
    Ok(out)
}

/// `@module-rebuild` / `@x11-module-rebuild` core (`OwnerSet`): the set of
/// packages owning at least one path matched by `files`, minus any package
/// that also owns a path matched by `exclude_files`.
///
/// Matches portage's `OwnerSet`: `mapPathsToAtoms` first `glob`s each
/// pattern **against the live filesystem** (so `/usr/lib*/xorg/modules`
/// expands to the concrete `/usr/lib64/xorg/modules`), then looks up
/// owners via `_match_contents` — an **exact CONTENTS-entry match**, with
/// symlink-aware parent resolution so `/lib/modules` matches a
/// `/usr/lib/modules` entry when `/lib` → `/usr/lib`.
///
/// `exclude-files` narrows the result: a package owning any excluded path
/// is dropped entirely.
///
/// Verified against a live host: `emerge -p @module-rebuild` returns empty
/// when no installed package has a CONTENTS entry for `/lib/modules`
/// (despite the dir being populated with hand-built kernel modules) —
/// confirming exact-match, not a directory-subtree/prefix match.
fn owner_set_atoms(
    vdb: &portage_vdb::Vdb,
    files: &[&str],
    exclude_files: &[&str],
    eroot: &Utf8Path,
) -> Result<Vec<Dep>> {
    // No live path matched the `files` globs, so nothing can own one and the
    // result is empty whatever the excludes say (they only ever *remove*
    // packages). Bail before touching the VDB rather than parsing every
    // installed CONTENTS to prove it — on a host with no X11 this is the
    // whole of `@x11-module-rebuild`.
    let paths = expand_glob_patterns(files, eroot);
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let exclude_paths = expand_glob_patterns(exclude_files, eroot);

    // Recorded `sym` entries across *every* installed package: a query path
    // like `/lib/modules` is typically routed through a symlink owned by a
    // different one (baselayout's `/lib` -> `usr/lib`). Lazy — a live root
    // resolves through the filesystem and never builds it.
    let symlinks = LazySymlinks::new(vdb);

    // Resolve each query once here rather than once per installed package —
    // the basename, needle and symlink-resolved form depend only on the query.
    let includes = Query::prepare(&paths, eroot, &symlinks);
    let excludes = Query::prepare(&exclude_paths, eroot, &symlinks);

    // One buffer for the whole walk: CONTENTS averages a few hundred KB, so a
    // fresh allocation per package churns the entire VDB through the
    // allocator to answer a question about one path.
    let mut raw = String::new();
    let mut out = Vec::new();
    for pkg in vdb.packages() {
        if !pkg.contents_into(&mut raw).unwrap_or(false) {
            continue;
        }
        if !owns_any(&raw, &includes, eroot, &symlinks) {
            continue;
        }
        // Excludes are only ever subtractive, so they matter for a package
        // that already matched and for nobody else. Testing them up front
        // would scan every installed package for basenames like `linux`,
        // which most paths in the tree end a component with.
        if owns_any(&raw, &excludes, eroot, &symlinks) {
            continue;
        }
        if let Some(member) = slot_member(&pkg) {
            out.push(set_atom(member));
        }
    }
    Ok(out)
}

/// Expand filesystem glob patterns (portage `files`/`exclude-files`) under
/// `eroot`, returning concrete absolute paths (ROOT-relative, leading `/`).
///
/// Portage joins `EROOT` + pattern and globs the live FS, then strips the
/// EROOT prefix (`mapPathsToAtoms`, lines 87-101). A literal pattern (no
/// wildcard) yields itself if it exists; `/usr/lib*/xorg/modules` expands to
/// each concrete `/usr/lib64/…` directory present.
fn expand_glob_patterns(patterns: &[&str], eroot: &Utf8Path) -> Vec<Utf8PathBuf> {
    let mut out = Vec::new();
    for pat in patterns {
        let full = eroot.join(pat.trim_start_matches('/'));
        for matched in glob::glob(full.as_str())
            .ok()
            .into_iter()
            .flatten()
            .flatten()
        {
            // Re-anchor as a leading-`/` ROOT-relative path to match the
            // absolute paths stored in CONTENTS.
            let Ok(rel) = matched.strip_prefix(eroot.as_std_path()) else {
                continue;
            };
            let mut abs = std::path::PathBuf::from("/");
            abs.push(rel);
            if let Ok(u) = Utf8PathBuf::from_path_buf(abs) {
                out.push(u);
            }
        }
    }
    out
}

/// Whether a package's unparsed CONTENTS owns any include path, and whether
/// it owns any exclude path — both in a single streaming pass, since the
/// caller always wants both and the text runs to hundreds of thousands of
/// lines.
///
/// Unlike [`InstalledPackage::owns`] (restricted to `Obj`/`Sym` for the
/// `qfile` use case), this matches **any** CONTENTS kind — including
/// `dir`, which is what `@module-rebuild`'s `/lib/modules` query needs.
///
/// Symlink-aware: if no exact string match, fall back to comparing each
/// side's resolved real path, so `/lib/modules` matches a
/// `/usr/lib/modules` entry when `/lib` → `/usr/lib`.
///
/// Queries and CONTENTS paths are ROOT-relative (leading `/`, `eroot` already
/// stripped — see [`expand_glob_patterns`]); resolution happens in the same
/// ROOT-relative space regardless of which of the two produced it, so the two
/// sides stay comparable ([`real_path`]).
fn owns_any(raw: &str, queries: &[Query], eroot: &Utf8Path, symlinks: &LazySymlinks) -> bool {
    candidate_lines(raw, queries).any(|line| {
        ContentsRef::parse_line(line).is_some_and(|e| {
            matches_any(queries, e.path, base_name(e.path.as_str()), eroot, symlinks)
        })
    })
}

/// The CONTENTS lines that could possibly match one of `queries`
///
/// Both of [`matches_any`]'s arms need the entry's last path component to
/// be a query basename, and a CONTENTS path field ends at a space, newline
/// or end of text — so every possible match sits at an occurrence of the
/// query's `/basename` followed by one of those three. Jumping straight to
/// those positions is an exact reject, skipping the hundreds of thousands
/// of lines per package that cannot match.
///
/// A line may be yielded more than once, or without matching (the needle
/// can fall in a symlink target); the caller re-checks it properly.
fn candidate_lines<'a>(raw: &'a str, queries: &'a [Query]) -> impl Iterator<Item = &'a str> + 'a {
    // Byte offsets throughout, but every one lands on a char boundary: the
    // needle is ASCII, and the bounds below sit on `\n` or the ends of `raw`.
    queries.iter().flat_map(move |q| {
        q.finder.find_iter(raw.as_bytes()).filter_map(move |at| {
            // `None` covers a final line with no trailing newline; `\r` a
            // CRLF file, which `parse_line` still handles because it trims.
            let end_of_field = at + q.finder.needle().len();
            let ends_field = raw.as_bytes().get(end_of_field);
            if !matches!(ends_field, None | Some(b' ') | Some(b'\n') | Some(b'\r')) {
                return None;
            }
            let start = memchr::memrchr(b'\n', &raw.as_bytes()[..at]).map_or(0, |nl| nl + 1);
            let end = memchr::memchr(b'\n', &raw.as_bytes()[end_of_field..])
                .map_or(raw.len(), |nl| end_of_field + nl);
            Some(&raw[start..end])
        })
    })
}

/// Whether the CONTENTS entry at `path` (basename `base`) matches any query
fn matches_any(
    queries: &[Query],
    path: &Utf8Path,
    base: &str,
    eroot: &Utf8Path,
    symlinks: &LazySymlinks,
) -> bool {
    queries.iter().any(|q| {
        path.as_str() == q.path.as_str()
            || (base == q.base.as_str()
                && real_path(path, eroot, symlinks).as_str() == q.real.as_str())
    })
}

/// One `files`/`exclude-files` path to test installed packages against, with
/// the per-query work hoisted out of the per-package loop.
///
/// Compare these through [`Utf8Path::as_str`], never `==` on the paths
/// themselves: `Utf8Path`'s `PartialEq` is component-wise
/// (`Components::eq_by`) where `str`'s is a length check plus `memcmp`, and
/// this runs against every CONTENTS entry of every installed package. Both
/// sides are already absolute and ROOT-relative, so the two agree.
struct Query {
    path: Utf8PathBuf,
    base: Utf8PathBuf,
    real: Utf8PathBuf,
    /// Searcher for `/` + [`Self::base`], the text a matching CONTENTS entry must contain (see
    /// [`candidate_lines`])
    ///
    /// Built once and reused across every installed package: `str::match_indices` would rebuild
    /// Two-Way's tables per package and search scalar, where this is SIMD.
    finder: memmem::Finder<'static>,
}

impl Query {
    fn prepare(paths: &[Utf8PathBuf], eroot: &Utf8Path, symlinks: &LazySymlinks) -> Vec<Self> {
        paths
            .iter()
            .map(|p| {
                let base = base_name(p.as_str());
                Self {
                    finder: memmem::Finder::new(&format!("/{base}")).into_owned(),
                    base: base.into(),
                    real: real_path(p, eroot, symlinks),
                    path: p.clone(),
                }
            })
            .collect()
    }
}

/// Trailing `/`-separated component of a CONTENTS path
///
/// `Utf8Path::file_name` builds a `Components` iterator per call, which dominates the scan
/// once it runs over every entry of every installed package.
fn base_name(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// Every VDB-recorded symlink, collected on first use and then reused
///
/// [`real_path`] consults it only when the live filesystem can't resolve a
/// path — which on a normal root never happens — and building it parses every
/// installed package's `CONTENTS`. Deferring keeps that whole pass off the
/// common path while leaving the scratch-root fallback intact.
struct LazySymlinks {
    /// Owned so the whole thing can be shared with [`portage_vdb::Vdb::scan`]'s workers, which
    /// outlive the call frame
    ///
    /// A `Vdb` is a path.
    vdb: portage_vdb::Vdb,
    /// `OnceLock`, not `OnceCell`: whichever worker needs the map first builds
    /// it for all of them.
    map: std::sync::OnceLock<HashMap<Utf8PathBuf, Utf8PathBuf>>,
}

impl LazySymlinks {
    fn new(vdb: &portage_vdb::Vdb) -> Self {
        Self {
            vdb: vdb.clone(),
            map: std::sync::OnceLock::new(),
        }
    }

    fn get(&self) -> &HashMap<Utf8PathBuf, Utf8PathBuf> {
        self.map.get_or_init(|| collect_symlinks(&self.vdb))
    }
}

/// The real (symlink-resolved) form of a ROOT-relative path, as best as it
/// can be determined.
///
/// Prefers the live filesystem (`eroot`-anchored `canonicalize`, then
/// re-stripped back to ROOT-relative) since that reflects any symlink
/// regardless of which package's CONTENTS recorded it.
///
/// Falls back to [`resolve_via_recorded_symlinks`] (VDB-only, no
/// filesystem access) when the path isn't materialized on disk yet — e.g.
/// this codebase's own `walk_image` binpkg-image assembly, or a stage
/// build mid-merge — where CONTENTS metadata alone still resolves a
/// recorded symlink like `/lib` → `usr/lib`.
fn real_path(p: &Utf8Path, eroot: &Utf8Path, symlinks: &LazySymlinks) -> Utf8PathBuf {
    let full = eroot.join(p.as_str().trim_start_matches('/'));
    let live = std::fs::canonicalize(full.as_std_path())
        .ok()
        .and_then(|c| Utf8PathBuf::from_path_buf(c).ok())
        .and_then(|c| c.strip_prefix(eroot).ok().map(Utf8Path::to_path_buf));
    match live {
        Some(rel) => Utf8Path::new("/").join(rel),
        None => resolve_via_recorded_symlinks(p, symlinks.get()),
    }
}

/// Resolve `path` against VDB-recorded symlinks ([`collect_symlinks`]),
/// substituting the longest ancestor recorded as a symlink source with its
/// target, repeatedly (bounded, to guard against a cyclic/self-referential
/// CONTENTS record). Pure path algebra, no filesystem access.
fn resolve_via_recorded_symlinks(
    path: &Utf8Path,
    symlinks: &HashMap<Utf8PathBuf, Utf8PathBuf>,
) -> Utf8PathBuf {
    let mut current = path.to_path_buf();
    for _ in 0..16 {
        let hit = current.ancestors().find_map(|a| {
            symlinks
                .get(a)
                .map(|target| (a.to_path_buf(), target.clone()))
        });
        let Some((src, target)) = hit else {
            return current;
        };
        let target_abs = if target.is_absolute() {
            target
        } else {
            src.parent().unwrap_or(Utf8Path::new("/")).join(&target)
        };
        let Ok(rest) = current.strip_prefix(&src) else {
            return current;
        };
        let next = target_abs.join(rest);
        if next == current {
            return current;
        }
        current = next;
    }
    current
}

/// Every `sym` CONTENTS entry across every installed package, as a
/// `path -> target` map (ROOT-relative, leading `/`; `target` as recorded,
/// which may itself be relative). See [`real_path`].
fn collect_symlinks(vdb: &portage_vdb::Vdb) -> HashMap<Utf8PathBuf, Utf8PathBuf> {
    let mut map = HashMap::new();
    for pkg in vdb.packages() {
        let Ok(Some(raw)) = pkg.contents_raw() else {
            continue;
        };
        for e in ContentsRef::parse(&raw) {
            if e.kind == ContentsKind::Sym
                && let Some(target) = e.target
            {
                map.insert(e.path.to_path_buf(), target.to_path_buf());
            }
        }
    }
    map
}

/// The `cat/pkg:{slot}` dep for a set member
///
/// Built from the parts rather than formatted and re-parsed: both already
/// come off the VDB entry interned and validated.
///
/// Unsorted and undeduplicated on purpose. A set has no order of its own —
/// `em --info -v` sorts its own display — and each package contributes at
/// most one member, so with a VDB unable to hold two packages sharing a cpn
/// *and* a slot the members are distinct already.
fn set_atom((cpn, slot): (Cpn, Slot)) -> Dep {
    let mut dep = Dep::new(cpn);
    dep.slot_dep = Some(SlotDep::Slot {
        slot: Some(slot),
        op: None,
    });
    dep
}

/// The `(cpn, main slot)` of an installed package — what these sets emit,
/// matching portage's `Atom(f"{pkg.cp}:{pkg.slot}")`.
///
/// Both halves come straight off the VDB entry, already interned and
/// validated, so nothing here formats an atom string for [`Dep::parse`] to
/// take apart again.
///
/// An empty (but present) `SLOT` defaults to `"0"` (portage's `slot_invalid`
/// fallback in `_pkg_str`, for the legitimate old-EAPI-implicit-slot case);
/// an unreadable/corrupted `SLOT` file returns `None` instead of guessing —
/// the same handling `preserved_rebuild_atoms` (`preserve_libs.rs`) gives the
/// identical VDB-read failure, so the two VDB-set families no longer disagree
/// on how to treat a mid-removal/corrupted package.
fn slot_member(pkg: &InstalledPackage) -> Option<(Cpn, Slot)> {
    // The main slot only, matching portage's `Atom(f"{pkg.cp}:{pkg.slot}")` —
    // `_pkg_str.slot` keeps the sub-slot separate. `from_name` reuses the
    // handle the VDB already interned instead of resolving and re-interning.
    Some((*pkg.cpn(), Slot::from_name(pkg.slot_main().ok()?)))
}

/// Parse `[section_name]` headers from all `.conf` files in `dir`
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

/// Parse `[section_name]` headers from a single ini-style `.conf` file
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

/// Each filename (non-hidden, non-directory) in `dir` is a set name
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

    // A scratch eroot with an (empty) `var/db/pkg`, enough for the
    // VDB-aware resolvers to open without error.
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

    // Write a minimal VDB package dir under `eroot/var/db/pkg/<cat>/<pf>` with the given
    // metadata fields
    //
    // Enumeration only needs the dir + a parseable pf, but the variable/slot accessors read
    // files, so callers pass the fields they need (`SLOT`, `PROPERTIES`, `INHERITED`, …).
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

    // --- @module-rebuild / @x11-module-rebuild (OwnerSet) ---
    //
    // OwnerSet first globs the *live filesystem* for each pattern, then does
    // an exact CONTENTS-path match (any kind, incl. `dir`). So a fixture needs
    // BOTH the live path to exist (glob expands it) AND a package whose
    // CONTENTS owns it.

    #[test]
    fn module_rebuild_returns_package_owning_lib_modules_dir_entry() {
        let (_keep, eroot) = vdb_eroot();
        std::fs::create_dir_all(eroot.join("lib/modules")).unwrap();
        // Owns the `/lib/modules` directory entry → in @module-rebuild.
        write_vdb_pkg(
            &eroot,
            "sys-kernel/linux-modules-6.18",
            &[("SLOT", "0"), ("CONTENTS", "dir /lib/modules\n")],
        );
        // Owns only a file *under* /lib/modules, not the dir → must be absent.
        write_vdb_pkg(
            &eroot,
            "app-emulation/vm-modules-1.0",
            &[
                ("SLOT", "0"),
                ("CONTENTS", "obj /lib/modules/6.18/vm.ko deadbeef 0\n"),
            ],
        );

        let atoms: Vec<String> = resolve_vdb_set("module-rebuild", &eroot)
            .expect("Some")
            .unwrap()
            .into_iter()
            .map(|d| d.to_string())
            .collect();
        assert_eq!(atoms, vec!["sys-kernel/linux-modules:0"]);
    }

    #[test]
    fn module_rebuild_does_not_subtree_match_files_under_lib_modules() {
        // The headline exact-vs-prefix check: a package owning a kernel
        // module FILE under /lib/modules but not the /lib/modules dir entry
        // itself is NOT returned. Confirms exact-path match (mirrors the
        // empty `emerge -p @module-rebuild` observed on a live host whose
        // /lib/modules is populated only by hand-built modules).
        let (_keep, eroot) = vdb_eroot();
        std::fs::create_dir_all(eroot.join("lib/modules/6.18")).unwrap();
        write_vdb_pkg(
            &eroot,
            "x11-drivers/nvidia-drivers-550",
            &[
                ("SLOT", "0"),
                ("CONTENTS", "obj /lib/modules/6.18/nvidia.ko aaaa 0\n"),
            ],
        );
        let atoms = resolve_vdb_set("module-rebuild", &eroot)
            .expect("Some")
            .unwrap();
        assert!(atoms.is_empty(), "subtree/prefix match must not apply");
    }

    #[test]
    fn module_rebuild_matches_a_final_line_with_no_trailing_newline() {
        // `candidate_lines` finds a match by looking at the byte *after* the
        // query basename, so the last line of a CONTENTS that doesn't end in
        // a newline is the one place that lookup can fall off the end.
        let (_keep, eroot) = vdb_eroot();
        std::fs::create_dir_all(eroot.join("lib/modules")).unwrap();
        write_vdb_pkg(
            &eroot,
            "sys-kernel/linux-modules-1",
            &[("SLOT", "0"), ("CONTENTS", "dir /lib/modules")],
        );
        let atoms: Vec<String> = resolve_vdb_set("module-rebuild", &eroot)
            .expect("Some")
            .unwrap()
            .into_iter()
            .map(|d| d.to_string())
            .collect();
        assert_eq!(atoms, vec!["sys-kernel/linux-modules:0"]);
    }

    #[test]
    fn module_rebuild_ignores_a_longer_component_sharing_the_query_basename() {
        // `/modules` occurs in `/lib/modules-backup`, but not as a whole
        // component — the byte after it is `-`, not a field terminator.
        let (_keep, eroot) = vdb_eroot();
        std::fs::create_dir_all(eroot.join("lib/modules")).unwrap();
        write_vdb_pkg(
            &eroot,
            "app-misc/backup-1",
            &[("SLOT", "0"), ("CONTENTS", "dir /lib/modules-backup\n")],
        );
        let atoms = resolve_vdb_set("module-rebuild", &eroot)
            .expect("Some")
            .unwrap();
        assert!(atoms.is_empty(), "a longer component must not match");
    }

    #[test]
    fn module_rebuild_excludes_package_owning_a_src_linux_exclude_path() {
        // exclude-files = /usr/src/linux* : a package owning both
        // /lib/modules and a matched exclude path is dropped entirely.
        let (_keep, eroot) = vdb_eroot();
        std::fs::create_dir_all(eroot.join("lib/modules")).unwrap();
        std::fs::create_dir_all(eroot.join("usr/src/linux-6.18")).unwrap();
        write_vdb_pkg(
            &eroot,
            "sys-kernel/gentoo-sources-6.18",
            &[
                ("SLOT", "0"),
                ("CONTENTS", "dir /lib/modules\ndir /usr/src/linux-6.18\n"),
            ],
        );
        let atoms = resolve_vdb_set("module-rebuild", &eroot)
            .expect("Some")
            .unwrap();
        assert!(
            atoms.is_empty(),
            "a package owning an excluded path is dropped from the result"
        );
    }

    #[test]
    fn module_rebuild_empty_when_no_package_owns_lib_modules() {
        let (_keep, eroot) = vdb_eroot();
        std::fs::create_dir_all(eroot.join("lib/modules")).unwrap();
        write_vdb_pkg(
            &eroot,
            "app-misc/unrelated-1.0",
            &[("SLOT", "0"), ("CONTENTS", "obj /usr/bin/foo aaaa 0\n")],
        );
        let atoms = resolve_vdb_set("module-rebuild", &eroot)
            .expect("Some")
            .unwrap();
        assert!(atoms.is_empty());
    }

    #[test]
    fn x11_module_rebuild_globs_lib_wildcard_to_concrete_dir() {
        // files = /usr/lib*/xorg/modules: the `*` must expand against the
        // live filesystem to e.g. /usr/lib64/xorg/modules.
        let (_keep, eroot) = vdb_eroot();
        std::fs::create_dir_all(eroot.join("usr/lib64/xorg/modules")).unwrap();
        write_vdb_pkg(
            &eroot,
            "x11-base/xorg-server-21",
            &[("SLOT", "0"), ("CONTENTS", "dir /usr/lib64/xorg/modules\n")],
        );
        let atoms: Vec<String> = resolve_vdb_set("x11-module-rebuild", &eroot)
            .expect("Some")
            .unwrap()
            .into_iter()
            .map(|d| d.to_string())
            .collect();
        assert_eq!(atoms, vec!["x11-base/xorg-server:0"]);
    }

    #[test]
    fn live_rebuild_skips_package_with_unreadable_slot_instead_of_defaulting() {
        // A package dir with no SLOT file at all (corrupted/mid-removal VDB
        // entry) must be skipped, not folded into a fabricated `:0` atom —
        // matches `preserved_rebuild_atoms`' handling of the same failure.
        let (_keep, eroot) = vdb_eroot();
        write_vdb_pkg(&eroot, "app-misc/noslot-1.0", &[("PROPERTIES", "live")]);
        let atoms = resolve_vdb_set("live-rebuild", &eroot)
            .expect("Some")
            .unwrap();
        assert!(atoms.is_empty());
    }

    #[test]
    fn live_rebuild_empty_slot_file_still_defaults_to_zero() {
        // Distinct from the unreadable case above: an empty-but-present SLOT
        // file is the legitimate old-EAPI-implicit-slot case and still
        // defaults to "0".
        let (_keep, eroot) = vdb_eroot();
        write_vdb_pkg(
            &eroot,
            "app-misc/emptyslot-1.0",
            &[("SLOT", ""), ("PROPERTIES", "live")],
        );
        let atoms: Vec<String> = resolve_vdb_set("live-rebuild", &eroot)
            .expect("Some")
            .unwrap()
            .into_iter()
            .map(|d| d.to_string())
            .collect();
        assert_eq!(atoms, vec!["app-misc/emptyslot:0"]);
    }

    #[test]
    fn module_rebuild_resolves_ownership_through_a_recorded_but_unmaterialized_symlink() {
        // The query path (`/lib/modules`) is a real, plain directory on this
        // fixture's disk — `expand_glob_patterns` needs *something* to exist
        // there to yield it as a candidate at all. The owning package
        // records its CONTENTS via a *different* path (`/compat/modules`)
        // that only resolves to the same real location through a symlink
        // (`/compat` -> `lib`) recorded in the VDB but never actually
        // created on disk (the CONTENTS record exists before the live
        // symlink does, as in `walk_image`-style binpkg image assembly).
        //
        // The old canonicalize-only comparison would silently drop this
        // package (its own CONTENTS path fails to canonicalize at all);
        // the VDB-only fallback still resolves the equivalence.
        let (_keep, eroot) = vdb_eroot();
        std::fs::create_dir_all(eroot.join("lib/modules")).unwrap();
        write_vdb_pkg(
            &eroot,
            "sys-apps/compat-links-1.0",
            &[("SLOT", "0"), ("CONTENTS", "sym /compat -> lib 0\n")],
        );
        write_vdb_pkg(
            &eroot,
            "sys-kernel/linux-modules-6.18",
            &[("SLOT", "0"), ("CONTENTS", "dir /compat/modules\n")],
        );
        let atoms: Vec<String> = resolve_vdb_set("module-rebuild", &eroot)
            .expect("Some")
            .unwrap()
            .into_iter()
            .map(|d| d.to_string())
            .collect();
        assert_eq!(atoms, vec!["sys-kernel/linux-modules:0"]);
    }
}
