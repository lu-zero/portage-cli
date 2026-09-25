//! Collision detection for planned merges
//!
//! Before registering a new package, the caller can check whether any files it
//! would install are already owned by a different package in the VDB.

use std::borrow::Borrow;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use camino::{Utf8Path, Utf8PathBuf};
use portage_atom::Cpv;

use crate::contents::{ContentsEntry, ContentsKind, ContentsRef};
use crate::error::Error;
use crate::package::InstalledPackage;
use crate::{Result, Vdb};

/// A file-ownership conflict between a planned install and the existing VDB
#[derive(Debug)]
pub struct Collision {
    /// The path that would collide
    pub path: Utf8PathBuf,
    /// The currently installed package that owns the path
    pub owner: InstalledPackage,
}

/// Snapshot of installed file ownership built from VDB `CONTENTS` files.
///
/// The index is a run-scoped snapshot; discard or rebuild it after any VDB
/// mutation. `register` and `unregister` do not update it. [`Self::is_current`]
/// detects normal directory/counter changes; in-place `CONTENTS` edits still
/// require an explicit rebuild. A merge batch can update it with
/// [`Self::add_package`] and [`Self::remove_package`] while holding its lock.
pub struct OwnershipIndex {
    owners: HashMap<PathKey, PathId>,
    path_owners: Vec<PathOwners>,
    paths_by_cpv: HashMap<Cpv, Vec<PathId>>,
    stamp: Option<VdbStamp>,
}

struct OwnershipBuildState {
    index: OwnershipIndex,
    error: Option<Error>,
}

impl Default for OwnershipBuildState {
    fn default() -> Self {
        Self {
            index: OwnershipIndex::empty(),
            error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
}

impl FileStamp {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
        }
    }

    fn read(path: &Utf8Path) -> Option<Self> {
        Some(Self::from_metadata(&fs::metadata(path.as_std_path()).ok()?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VdbStamp {
    root: FileStamp,
    counter: Option<(FileStamp, Vec<u8>)>,
    categories: Vec<(String, FileStamp)>,
}

impl VdbStamp {
    fn capture(vdb: &Vdb) -> Option<Self> {
        let root = FileStamp::read(vdb.root())?;
        let counter_path = vdb.root().join("COUNTER");
        let counter = match fs::read(counter_path.as_std_path()) {
            Ok(contents) => Some((FileStamp::read(&counter_path)?, contents)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return None,
        };

        let mut categories = Vec::new();
        for entry in fs::read_dir(vdb.root().as_std_path()).ok()? {
            let entry = entry.ok()?;
            let path = Utf8PathBuf::from_path_buf(entry.path()).ok()?;
            if !path.is_dir() {
                continue;
            }
            categories.push((path.file_name()?.to_owned(), FileStamp::read(&path)?));
        }
        categories.sort_by(|a, b| a.0.cmp(&b.0));

        Some(Self {
            root,
            counter,
            categories,
        })
    }
}

type PathId = usize;

#[derive(Clone)]
struct PathKey(Arc<Utf8PathBuf>);

impl PathKey {
    fn as_path(&self) -> &Utf8Path {
        self.0.as_ref()
    }
}

impl Borrow<Utf8Path> for PathKey {
    fn borrow(&self) -> &Utf8Path {
        self.as_path()
    }
}

impl PartialEq for PathKey {
    fn eq(&self, other: &Self) -> bool {
        self.as_path() == other.as_path()
    }
}

impl Eq for PathKey {}

impl Hash for PathKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_path().hash(state);
    }
}

#[derive(Default)]
struct PathOwners {
    first: Option<Arc<InstalledPackage>>,
    rest: Vec<Arc<InstalledPackage>>,
}

impl PathOwners {
    fn push(&mut self, owner: Arc<InstalledPackage>) {
        if self.first.is_none() {
            self.first = Some(owner);
        } else {
            self.rest.push(owner);
        }
    }

    fn iter(&self) -> impl Iterator<Item = &Arc<InstalledPackage>> {
        self.first.iter().chain(self.rest.iter())
    }

    fn remove(&mut self, cpv: &Cpv) {
        if self.first.as_ref().is_some_and(|owner| owner.cpv() == cpv) {
            self.first = None;
        }
        self.rest.retain(|owner| owner.cpv() != cpv);
    }
}

impl OwnershipIndex {
    fn empty() -> Self {
        Self {
            owners: HashMap::new(),
            path_owners: Vec::new(),
            paths_by_cpv: HashMap::new(),
            stamp: None,
        }
    }

    fn insert_path(&mut self, owner: &Arc<InstalledPackage>, path: Utf8PathBuf) {
        let id = if let Some(&id) = self.owners.get(Utf8Path::new(&path)) {
            id
        } else {
            let id = self.path_owners.len();
            self.path_owners.push(PathOwners::default());
            self.owners.insert(PathKey(Arc::new(path)), id);
            id
        };
        self.path_owners[id].push(Arc::clone(owner));
        self.paths_by_cpv
            .entry(owner.cpv().clone())
            .or_default()
            .push(id);
    }

    /// Build an ownership snapshot by scanning every installed package once.
    pub fn build(vdb: &Vdb) -> Result<Self> {
        let state = Arc::new(Mutex::new(OwnershipBuildState::default()));
        let callback_state = Arc::clone(&state);
        let receiver: flume::Receiver<()> =
            vdb.scan(None, move |buf: &mut String, pkg: &InstalledPackage| {
                match pkg.contents_into(buf) {
                    Ok(false) => {}
                    Ok(true) => {}
                    Err(Error::Io { .. }) => return None,
                    Err(error) => {
                        let mut state = callback_state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if state.error.is_none() {
                            state.error = Some(error);
                        }
                        return None;
                    }
                }

                let owner = Arc::new(pkg.clone());
                let mut state = callback_state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                for entry in ContentsRef::parse(buf) {
                    if matches!(entry.kind, ContentsKind::Obj | ContentsKind::Sym) {
                        state.index.insert_path(&owner, entry.path.to_path_buf());
                    }
                }
                None
            });
        while receiver.recv().is_ok() {}

        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match state.error.take() {
            Some(error) => Err(error),
            None => {
                let mut index = std::mem::replace(&mut state.index, OwnershipIndex::empty());
                index.refresh(vdb);
                Ok(index)
            }
        }
    }

    /// Return whether the VDB metadata still matches this snapshot.
    ///
    /// This is a best-effort guard for normal VDB directory and counter changes;
    /// callers must still rebuild after an in-place `CONTENTS` mutation.
    pub fn is_current(&self, vdb: &Vdb) -> bool {
        self.stamp
            .as_ref()
            .is_some_and(|stamp| VdbStamp::capture(vdb).as_ref() == Some(stamp))
    }

    /// Refresh the metadata stamp after updating the VDB through this index.
    pub fn refresh(&mut self, vdb: &Vdb) {
        self.stamp = VdbStamp::capture(vdb);
    }

    /// Find all existing owners of the object and symlink paths in `planned`.
    pub fn find_collisions(
        &self,
        planned: &[ContentsEntry],
        exclude: Option<&Cpv>,
    ) -> Vec<Collision> {
        let mut seen = HashSet::new();
        let target_paths: Vec<&Utf8Path> = planned
            .iter()
            .filter(|entry| matches!(entry.kind, ContentsKind::Obj | ContentsKind::Sym))
            .filter_map(|entry| {
                let path = entry.path.as_path();
                seen.insert(path).then_some(path)
            })
            .collect();
        if target_paths.is_empty() {
            return Vec::new();
        }

        let mut collisions = Vec::new();
        for path in target_paths {
            let Some(&id) = self.owners.get(path) else {
                continue;
            };
            for owner in self.path_owners[id].iter() {
                if exclude.is_some_and(|excluded| excluded == owner.cpv()) {
                    continue;
                }
                collisions.push(Collision {
                    path: path.to_path_buf(),
                    owner: owner.as_ref().clone(),
                });
            }
        }
        collisions.sort_by(|a, b| {
            a.owner
                .path()
                .as_str()
                .cmp(b.owner.path().as_str())
                .then_with(|| a.path.as_str().cmp(b.path.as_str()))
        });
        collisions
    }

    /// Add or replace a package using the already-known `CONTENTS` entries.
    ///
    /// Call [`Self::refresh`] after the corresponding VDB write.
    pub fn add_package(&mut self, pkg: &InstalledPackage, contents: &[ContentsEntry]) {
        self.remove_package(pkg.cpv());
        let owner = Arc::new(pkg.clone());
        for entry in contents {
            if !matches!(entry.kind, ContentsKind::Obj | ContentsKind::Sym) {
                continue;
            }
            self.insert_path(&owner, entry.path.to_path_buf());
        }
    }

    /// Remove every path owned by `cpv` from the snapshot.
    ///
    /// Call [`Self::refresh`] after the corresponding VDB write.
    pub fn remove_package(&mut self, cpv: &Cpv) {
        let Some(paths) = self.paths_by_cpv.remove(cpv) else {
            return;
        };
        let mut seen = HashSet::new();
        for id in paths {
            if !seen.insert(id) {
                continue;
            }
            self.path_owners[id].remove(cpv);
        }
    }
}

impl Vdb {
    /// Check `planned` CONTENTS for files already owned by another package
    ///
    /// Only `obj` and `sym` entries are checked — directories are legitimately
    /// shared between packages and are never considered collisions.
    ///
    /// `exclude` lets the caller skip a specific CPV (the old version of the
    /// same package when re-merging).  Pass `None` for fresh installs.
    ///
    /// Returns a list of collisions.  An empty vec means the install is clean.
    pub fn find_collisions(
        &self,
        planned: &[ContentsEntry],
        exclude: Option<&Cpv>,
    ) -> Result<Vec<Collision>> {
        let target_paths: HashSet<&Utf8Path> = planned
            .iter()
            .filter(|entry| matches!(entry.kind, ContentsKind::Obj | ContentsKind::Sym))
            .map(|entry| entry.path.as_path())
            .collect();
        if target_paths.is_empty() {
            return Ok(Vec::new());
        }

        let mut collisions = Vec::new();
        for pkg in self.packages() {
            if exclude.is_some_and(|excluded| excluded == pkg.cpv()) {
                continue;
            }
            let raw = match pkg.contents_raw() {
                Ok(Some(raw)) => raw,
                Ok(None) => continue,
                Err(Error::Io { .. }) => continue,
                Err(error) => return Err(error),
            };
            for entry in ContentsRef::parse(&raw) {
                if matches!(entry.kind, ContentsKind::Obj | ContentsKind::Sym)
                    && target_paths.contains(entry.path)
                {
                    collisions.push(Collision {
                        path: entry.path.to_path_buf(),
                        owner: pkg.clone(),
                    });
                }
            }
        }
        Ok(collisions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContentsKind;
    use crate::write::MergeSpec;
    use portage_atom::Cpv;

    fn dir_entry(path: &str) -> ContentsEntry {
        ContentsEntry {
            kind: ContentsKind::Dir,
            path: path.into(),
            md5: None,
            mtime: None,
            target: None,
        }
    }

    fn obj_entry(path: &str) -> ContentsEntry {
        ContentsEntry {
            kind: ContentsKind::Obj,
            path: path.into(),
            md5: Some("deadbeef".into()),
            mtime: Some(0),
            target: None,
        }
    }

    fn sym_entry(path: &str, target: &str) -> ContentsEntry {
        ContentsEntry {
            kind: ContentsKind::Sym,
            path: path.into(),
            md5: None,
            mtime: Some(0),
            target: Some(target.into()),
        }
    }

    fn simple_spec(cpv: Cpv, contents: Vec<ContentsEntry>) -> MergeSpec {
        MergeSpec {
            cpv,
            eapi: "8".into(),
            slot: "0".into(),
            use_flags: vec![],
            iuse: vec![],
            iuse_effective: vec![],
            depend: None,
            rdepend: None,
            bdepend: None,
            pdepend: None,
            idepend: None,
            keywords: vec![],
            license: None,
            description: "test".into(),
            homepage: None,
            restrict: None,
            properties: None,
            defined_phases: vec![],
            repository: None,
            inherited: vec![],
            features: None,
            chost: None,
            cbuild: None,
            cflags: None,
            cxxflags: None,
            ldflags: None,
            rustflags: None,
            needed: vec![],
            needed_elf2: vec![],
            requires: vec![],
            provides: vec![],
            contents,
            build_time: 0,
            size: 0,
            counter: 1,
        }
    }

    fn open_vdb(dir: &std::path::Path) -> Vdb {
        let root: camino::Utf8PathBuf = dir.to_path_buf().try_into().unwrap();
        Vdb::open(root).unwrap()
    }

    #[test]
    fn no_collision_on_empty_vdb() {
        let tmp = tempfile::tempdir().unwrap();
        let vdb = open_vdb(tmp.path());
        let planned = vec![obj_entry("/usr/bin/foo")];
        let collisions = vdb.find_collisions(&planned, None).unwrap();
        assert!(collisions.is_empty());
    }

    #[test]
    fn no_collision_when_files_differ() {
        let tmp = tempfile::tempdir().unwrap();
        let vdb = open_vdb(tmp.path());

        let cpv = Cpv::parse("app-misc/foo-1.0").unwrap();
        vdb.register(&simple_spec(cpv, vec![obj_entry("/usr/bin/foo")]))
            .unwrap();

        let planned = vec![obj_entry("/usr/bin/bar")];
        let collisions = vdb.find_collisions(&planned, None).unwrap();
        assert!(collisions.is_empty());
    }

    #[test]
    fn detects_obj_collision() {
        let tmp = tempfile::tempdir().unwrap();
        let vdb = open_vdb(tmp.path());

        let cpv = Cpv::parse("app-misc/foo-1.0").unwrap();
        vdb.register(&simple_spec(cpv, vec![obj_entry("/usr/bin/shared")]))
            .unwrap();

        let planned = vec![obj_entry("/usr/bin/shared")];
        let collisions = vdb.find_collisions(&planned, None).unwrap();
        assert_eq!(collisions.len(), 1);
        assert_eq!(collisions[0].path.as_str(), "/usr/bin/shared");
        assert_eq!(collisions[0].owner.cpv().to_string(), "app-misc/foo-1.0");
    }

    #[test]
    fn detects_sym_collision() {
        let tmp = tempfile::tempdir().unwrap();
        let vdb = open_vdb(tmp.path());

        let cpv = Cpv::parse("app-misc/foo-1.0").unwrap();
        vdb.register(&simple_spec(
            cpv,
            vec![sym_entry("/usr/lib/libfoo.so", "libfoo.so.1")],
        ))
        .unwrap();

        let planned = vec![sym_entry("/usr/lib/libfoo.so", "libfoo.so.2")];
        let collisions = vdb.find_collisions(&planned, None).unwrap();
        assert_eq!(collisions.len(), 1);
    }

    #[test]
    fn component_equivalent_paths_collide_consistently() {
        let tmp = tempfile::tempdir().unwrap();
        let vdb = open_vdb(tmp.path());
        let cpv = Cpv::parse("app-misc/foo-1.0").unwrap();
        vdb.register(&simple_spec(cpv, vec![obj_entry("/usr//bin/foo")]))
            .unwrap();

        let planned = vec![obj_entry("/usr/bin/foo")];
        assert!(!vdb.find_collisions(&planned, None).unwrap().is_empty());
        assert!(
            !OwnershipIndex::build(&vdb)
                .unwrap()
                .find_collisions(&planned, None)
                .is_empty()
        );
    }

    #[test]
    fn unreadable_contents_is_not_partially_indexed() {
        let tmp = tempfile::tempdir().unwrap();
        let vdb = open_vdb(tmp.path());
        let cpv = Cpv::parse("app-misc/foo-1.0").unwrap();
        let package = vdb
            .register(&simple_spec(cpv, vec![obj_entry("/usr/bin/shared")]))
            .unwrap();
        let mut raw = b"obj /usr/bin/shared deadbeef 0\n".to_vec();
        raw.push(0xff);
        std::fs::write(package.path().join("CONTENTS"), raw).unwrap();

        let planned = vec![obj_entry("/usr/bin/shared")];
        assert!(
            OwnershipIndex::build(&vdb)
                .unwrap()
                .find_collisions(&planned, None)
                .is_empty()
        );
    }

    #[test]
    fn dirs_not_considered_collisions() {
        let tmp = tempfile::tempdir().unwrap();
        let vdb = open_vdb(tmp.path());

        let cpv = Cpv::parse("app-misc/foo-1.0").unwrap();
        vdb.register(&simple_spec(cpv, vec![dir_entry("/usr/share/doc")]))
            .unwrap();

        // Same directory in planned install — should not collide.
        let planned = vec![dir_entry("/usr/share/doc"), obj_entry("/usr/share/doc/bar")];
        let collisions = vdb.find_collisions(&planned, None).unwrap();
        assert!(collisions.is_empty());
    }

    #[test]
    fn exclude_skips_same_package() {
        let tmp = tempfile::tempdir().unwrap();
        let vdb = open_vdb(tmp.path());

        let cpv = Cpv::parse("app-misc/foo-1.0").unwrap();
        vdb.register(&simple_spec(cpv.clone(), vec![obj_entry("/usr/bin/foo")]))
            .unwrap();

        // Re-merging the same CPV: with exclude it's clean, without it collides.
        let planned = vec![obj_entry("/usr/bin/foo")];
        assert!(
            vdb.find_collisions(&planned, Some(&cpv))
                .unwrap()
                .is_empty()
        );
        assert!(!vdb.find_collisions(&planned, None).unwrap().is_empty());
    }

    #[test]
    fn ownership_index_updates_after_package_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let vdb = open_vdb(tmp.path());
        let path = "/usr/bin/shared";
        let first_contents = vec![obj_entry(path)];
        let first_cpv = Cpv::parse("app-misc/first-1.0").unwrap();
        let first = vdb
            .register(&simple_spec(first_cpv, first_contents.clone()))
            .unwrap();

        let mut index = OwnershipIndex::build(&vdb).unwrap();
        assert_eq!(index.find_collisions(&first_contents, None).len(), 1);

        let second_contents = vec![obj_entry(path)];
        let second_cpv = Cpv::parse("app-misc/second-1.0").unwrap();
        let second = vdb
            .register(&simple_spec(second_cpv, second_contents.clone()))
            .unwrap();
        assert_eq!(index.find_collisions(&second_contents, None).len(), 1);
        assert!(!index.is_current(&vdb));
        assert_eq!(
            OwnershipIndex::build(&vdb)
                .unwrap()
                .find_collisions(&second_contents, None)
                .len(),
            2
        );
        index.add_package(&second, &second_contents);
        index.refresh(&vdb);
        assert!(index.is_current(&vdb));
        assert_eq!(index.find_collisions(&second_contents, None).len(), 2);

        index.remove_package(first.cpv());
        let collisions = index.find_collisions(&second_contents, None);
        assert_eq!(collisions.len(), 1);
        assert_eq!(collisions[0].owner.cpv(), second.cpv());
    }

    #[test]
    fn ownership_stamp_notices_unregister() {
        let tmp = tempfile::tempdir().unwrap();
        let vdb = open_vdb(tmp.path());
        let contents = vec![obj_entry("/usr/bin/shared")];
        let cpv = Cpv::parse("app-misc/foo-1.0").unwrap();
        let package = vdb.register(&simple_spec(cpv, contents.clone())).unwrap();
        let index = OwnershipIndex::build(&vdb).unwrap();

        vdb.unregister(&package).unwrap();
        assert!(!index.is_current(&vdb));
        assert!(
            OwnershipIndex::build(&vdb)
                .unwrap()
                .find_collisions(&contents, None)
                .is_empty()
        );
    }

    #[test]
    fn multiple_collisions_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let vdb = open_vdb(tmp.path());

        let cpv_a = Cpv::parse("app-misc/a-1.0").unwrap();
        let cpv_b = Cpv::parse("app-misc/b-1.0").unwrap();
        vdb.register(&simple_spec(cpv_a, vec![obj_entry("/usr/bin/x")]))
            .unwrap();
        vdb.register(&simple_spec(cpv_b, vec![obj_entry("/usr/bin/y")]))
            .unwrap();

        let planned = vec![obj_entry("/usr/bin/x"), obj_entry("/usr/bin/y")];
        let collisions = vdb.find_collisions(&planned, None).unwrap();
        assert_eq!(collisions.len(), 2);
    }
}
