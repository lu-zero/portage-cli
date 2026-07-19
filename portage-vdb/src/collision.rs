//! Collision detection for planned merges.
//!
//! Before registering a new package, the caller can check whether any files it
//! would install are already owned by a different package in the VDB.

use camino::Utf8PathBuf;
use portage_atom::Cpv;

use crate::contents::{ContentsEntry, ContentsKind};
use crate::error::Error;
use crate::package::InstalledPackage;
use crate::{Result, Vdb};

/// A file-ownership conflict between a planned install and the existing VDB.
#[derive(Debug)]
pub struct Collision {
    /// The path that would collide.
    pub path: Utf8PathBuf,
    /// The currently installed package that owns the path.
    pub owner: InstalledPackage,
}

impl Vdb {
    /// Check `planned` CONTENTS for files already owned by another package.
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
        // Collect the paths we care about: only obj and sym entries.
        let target_paths: Vec<&Utf8PathBuf> = planned
            .iter()
            .filter(|e| matches!(e.kind, ContentsKind::Obj | ContentsKind::Sym))
            .map(|e| &e.path)
            .collect();

        if target_paths.is_empty() {
            return Ok(vec![]);
        }

        let mut collisions = Vec::new();

        for pkg in self.packages().into_iter() {
            // Skip the package being replaced.
            if exclude.is_some_and(|ex| ex == pkg.cpv()) {
                continue;
            }

            let pkg_contents = match pkg.contents() {
                Ok(c) => c,
                // Ignore packages whose CONTENTS can't be read (corrupted VDB).
                Err(Error::Io { .. }) => continue,
                Err(e) => return Err(e),
            };

            for entry in &pkg_contents {
                if !matches!(entry.kind, ContentsKind::Obj | ContentsKind::Sym) {
                    continue;
                }
                if target_paths.contains(&&entry.path) {
                    collisions.push(Collision {
                        path: entry.path.clone(),
                        owner: pkg.clone(), // clone the package handle
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
