//! The tree database as a [`TreeSource`], and the package directories copied out of it for builds

use std::io;

use camino::{Utf8Path, Utf8PathBuf};
use portage_atom::Cpv;
use redb::{ReadOnlyDatabase, ReadableDatabase, ReadableTable};

use super::schema::{
    EBUILDS, KIND_BLOB, KIND_TREE, META, MODE_DIR, MODE_LINK, OBJECTS, Oid, PATHS, TREE_SCHEMA,
    check_schema, db_err, decode, open_err, resolve, row_is_link, tree_entries,
};
use crate::tree::{EntryKind, TreeSource};

/// A repository tree read from a redb database
pub struct RedbTree {
    pub(super) db: ReadOnlyDatabase,
    pub(super) file: Utf8PathBuf,
    root: Oid,
}

impl RedbTree {
    /// Open the database at `file`
    pub fn open(file: impl Into<Utf8PathBuf>) -> io::Result<Self> {
        let file = file.into();
        let db = ReadOnlyDatabase::open(&file).map_err(|e| open_err(e, "em sync"))?;
        let txn = db.begin_read().map_err(db_err)?;
        check_schema(&txn, TREE_SCHEMA)?;
        let meta = txn.open_table(META).map_err(db_err)?;
        let hex = meta
            .get("tree")
            .map_err(db_err)?
            .ok_or_else(|| db_err("no tree recorded"))?;
        let mut root = [0u8; 20];
        hex::decode_to_slice(hex.value(), &mut root).map_err(db_err)?;
        drop(meta);
        Ok(RedbTree { db, file, root })
    }

    /// The object `oid` decoded, with its kind byte
    fn object(
        table: &redb::ReadOnlyTable<&[u8], &[u8]>,
        oid: &Oid,
    ) -> io::Result<Option<(u8, Vec<u8>)>> {
        let Some(stored) = table.get(&oid[..]).map_err(db_err)? else {
            return Ok(None);
        };
        Ok(Some(decode(stored.value())?))
    }

    /// The git mode and oid at `rel`, following symlinks
    fn resolve(
        &self,
        objects: &redb::ReadOnlyTable<&[u8], &[u8]>,
        rel: &str,
    ) -> io::Result<Option<(Vec<u8>, Oid)>> {
        resolve(&self.root, rel, true, |oid| Self::object(objects, oid))
    }
}

impl TreeSource for RedbTree {
    fn read(&self, rel: &Utf8Path) -> io::Result<Option<Vec<u8>>> {
        // `paths` holds files only, as `[mode u16][oid]`: one lookup instead of a tree walk.
        let txn = self.db.begin_read().map_err(db_err)?;
        let paths = txn.open_table(PATHS).map_err(db_err)?;
        let row = paths.get(rel.as_str()).map_err(db_err)?;
        let direct = match &row {
            Some(row) if !row_is_link(row.value()) => Some(row.value()),
            Some(_) => None,
            // Only a symlinked directory on the way can make a path `paths` lacks exist.
            None => {
                let mut via_link = false;
                for dir in rel.ancestors().skip(1).filter(|d| !d.as_str().is_empty()) {
                    if paths
                        .get(dir.as_str())
                        .map_err(db_err)?
                        .is_some_and(|r| row_is_link(r.value()))
                    {
                        via_link = true;
                        break;
                    }
                }
                if !via_link {
                    return Ok(None);
                }
                None
            }
        };
        let objects = txn.open_table(OBJECTS).map_err(db_err)?;
        let oid = match direct {
            Some(row) => row
                .get(2..22)
                .and_then(|o| Oid::try_from(o).ok())
                .ok_or_else(|| db_err("malformed path entry"))?,
            None => match self.resolve(&objects, rel.as_str())? {
                Some((mode, oid)) if mode != MODE_DIR => oid,
                _ => return Ok(None),
            },
        };
        Ok(match Self::object(&objects, &oid)? {
            Some((KIND_BLOB, data)) => Some(data),
            _ => None,
        })
    }

    fn list(&self, rel: &Utf8Path) -> io::Result<Option<Vec<(String, EntryKind)>>> {
        let txn = self.db.begin_read().map_err(db_err)?;
        let objects = txn.open_table(OBJECTS).map_err(db_err)?;
        let Some((_, oid)) = self.resolve(&objects, rel.as_str())? else {
            return Ok(None);
        };
        let Some((KIND_TREE, data)) = Self::object(&objects, &oid)? else {
            return Ok(None);
        };
        let mut out = Vec::new();
        for e in tree_entries(&data) {
            let Ok(name) = String::from_utf8(e.name.to_vec()) else {
                continue;
            };
            // A link is what it points to; a broken one or a loop is left out, as `FsTree` does.
            let mode = if e.mode == MODE_LINK {
                match self.resolve(&objects, rel.join(&name).as_str()) {
                    Ok(Some((mode, _))) => mode,
                    Ok(None) | Err(_) => continue,
                }
            } else {
                e.mode.to_vec()
            };
            out.push((name, kind_of(&mode)));
        }
        Ok(Some(out))
    }

    fn kind(&self, rel: &Utf8Path) -> io::Result<Option<EntryKind>> {
        let txn = self.db.begin_read().map_err(db_err)?;
        let objects = txn.open_table(OBJECTS).map_err(db_err)?;
        Ok(self
            .resolve(&objects, rel.as_str())?
            .map(|(mode, _)| kind_of(&mode)))
    }

    fn display(&self, rel: &Utf8Path) -> String {
        format!(
            "redb://{}?{}",
            uri_part(self.file.as_str()),
            uri_part(rel.as_str())
        )
    }

    fn ebuild_index(&self) -> Option<io::Result<Vec<Utf8PathBuf>>> {
        Some(self.ebuild_paths())
    }

    fn checkout(&self, rels: &[&Utf8Path], copies: Option<&Utf8Path>) -> io::Result<Utf8PathBuf> {
        let copies =
            copies.ok_or_else(|| db_err(format!("{} has no checkout to build from", self.file)))?;
        let root = copies.join(hex::encode(self.root));
        let txn = self.db.begin_read().map_err(db_err)?;
        let objects = txn.open_table(OBJECTS).map_err(db_err)?;
        for rel in rels {
            let dest = root.join(rel);
            if dest.symlink_metadata().is_ok() {
                continue;
            }
            let Some((mode, oid)) = self.resolve(&objects, rel.as_str())? else {
                continue;
            };
            // Each path appears whole or not at all, so a concurrent em never sees half of it.
            std::fs::create_dir_all(copies)?;
            let staging = tempfile::Builder::new()
                .prefix(".staging-")
                .tempdir_in(copies)?;
            let tmp = Utf8Path::from_path(staging.path())
                .ok_or_else(|| db_err("non-UTF-8 staging directory"))?
                .join("x");
            let source = Source {
                objects: &objects,
                root: &self.root,
            };
            source.write(rel.as_str(), &mode, &oid, &tmp, 0, &mut Vec::new())?;
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if let Err(e) = std::fs::rename(&tmp, &dest)
                && dest.symlink_metadata().is_err()
            {
                return Err(e);
            }
        }
        std::fs::create_dir_all(&root)?;
        // A copy in use stays recent, so another em building from it never sees it pruned.
        std::fs::File::open(&root)?.set_modified(std::time::SystemTime::now())?;
        prune_copies(copies, &root);
        Ok(root)
    }

    fn is_checkout(&self) -> bool {
        false
    }
}

/// How long a copy of another tree, or a staging directory left by a killed em, is kept unused
const COPY_KEEP: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);

/// Copies of other trees under `copies`, and leftover staging directories, unused for [`COPY_KEEP`]
fn prune_copies(copies: &Utf8Path, keep: &Utf8Path) {
    let Ok(dir) = copies.read_dir_utf8() else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in dir.flatten() {
        let name = entry.file_name();
        let ours = (name.len() == 40 && hex::decode(name).is_ok()) || name.starts_with(".staging-");
        let unused = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| now.duration_since(t).unwrap_or_default() > COPY_KEEP);
        if ours && unused && entry.path() != keep {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// What `kind` reports for a tree entry's git mode
fn kind_of(mode: &[u8]) -> EntryKind {
    if mode == MODE_DIR {
        EntryKind::Dir
    } else {
        EntryKind::File
    }
}

/// How deep a copied package directory may nest, symlinked directories included
const MAX_COPY_DEPTH: usize = 32;

/// The tree a package directory is copied from
struct Source<'t> {
    objects: &'t redb::ReadOnlyTable<&'static [u8], &'static [u8]>,
    root: &'t Oid,
}

impl Source<'_> {
    /// Write the object `oid`, at `rel` in the tree with git mode `mode`, to `dest`; a symlink is
    /// written as what it points to, which may be outside the copied directory, except a link to
    /// one of the directories `copying` (`files/self -> .`), which stays a link
    fn write(
        &self,
        rel: &str,
        mode: &[u8],
        oid: &Oid,
        dest: &Utf8Path,
        depth: usize,
        copying: &mut Vec<Oid>,
    ) -> io::Result<()> {
        if depth > MAX_COPY_DEPTH {
            return Err(db_err(format!("{rel}: nested too deep, a symlink loop?")));
        }
        let missing = || db_err(format!("object {} missing for {dest}", hex::encode(oid)));
        if mode == b"160000" {
            return std::fs::create_dir(dest);
        }
        if mode == MODE_LINK {
            return match resolve(self.root, rel, true, |o| RedbTree::object(self.objects, o))? {
                Some((target_mode, target))
                    if target_mode == MODE_DIR && copying.contains(&target) =>
                {
                    let Some((KIND_BLOB, text)) = RedbTree::object(self.objects, oid)? else {
                        return Err(missing());
                    };
                    std::os::unix::fs::symlink(String::from_utf8(text).map_err(db_err)?, dest)
                }
                Some((mode, target)) => self.write(rel, &mode, &target, dest, depth + 1, copying),
                None => Ok(()),
            };
        }
        let (kind, data) = RedbTree::object(self.objects, oid)?.ok_or_else(missing)?;
        match (mode, kind) {
            (MODE_DIR, KIND_TREE) => {
                std::fs::create_dir(dest)?;
                copying.push(*oid);
                for e in tree_entries(&data) {
                    let Ok(name) = std::str::from_utf8(e.name) else {
                        continue;
                    };
                    let child = format!("{rel}/{name}");
                    self.write(&child, e.mode, e.oid, &dest.join(name), depth + 1, copying)?;
                }
                copying.pop();
                Ok(())
            }
            (_, KIND_BLOB) => {
                std::fs::write(dest, data)?;
                if mode == b"100755" {
                    use std::os::unix::fs::PermissionsExt as _;
                    std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))?;
                }
                Ok(())
            }
            _ => Err(db_err(format!("unexpected object for {dest}"))),
        }
    }
}

impl RedbTree {
    /// `cat/pkg/pkg-ver.ebuild` for every key of the `ebuilds` table (`cat/pkg-ver`)
    fn ebuild_paths(&self) -> io::Result<Vec<Utf8PathBuf>> {
        Ok(self
            .cpvs()?
            .into_iter()
            .map(|cpv| {
                let pf = format!("{}-{}", cpv.cpn.package, cpv.version);
                Utf8PathBuf::from(format!(
                    "{}/{}/{pf}.ebuild",
                    cpv.cpn.category, cpv.cpn.package
                ))
            })
            .collect())
    }

    /// Every cpv in the `ebuilds` table
    pub(super) fn cpvs(&self) -> io::Result<Vec<Cpv>> {
        let txn = self.db.begin_read().map_err(db_err)?;
        let table = txn.open_table(EBUILDS).map_err(db_err)?;
        let mut out = Vec::new();
        for row in table.iter().map_err(db_err)? {
            let (key, _) = row.map_err(db_err)?;
            if let Ok(cpv) = Cpv::parse(key.value()) {
                out.push(cpv);
            }
        }
        Ok(out)
    }
}

/// `text` for a URI: `%`, `#`, spaces and control characters
/// percent-encoded, everything else (`+` included) as-is
fn uri_part(text: &str) -> std::borrow::Cow<'_, str> {
    let encode = |c: char| matches!(c, '%' | '#' | ' ') || c.is_control();
    if !text.chars().any(encode) {
        return text.into();
    }
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        if encode(c) {
            let mut buf = [0u8; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        } else {
            out.push(c);
        }
    }
    out.into()
}

#[cfg(test)]
mod tests {
    use super::super::schema::CODEC_RAW;
    use super::*;

    #[test]
    fn uri_parts_encode_only_what_would_break_the_uri() {
        assert_eq!(uri_part("sys-apps/gtk+-3.ebuild"), "sys-apps/gtk+-3.ebuild");
        assert_eq!(uri_part("files/a b#1%.patch"), "files/a%20b%231%25.patch");
    }

    /// A database holding `objects` (oid byte, kind, body) with `root` as its tree
    fn database(file: &Utf8Path, root: u8, objects: &[(u8, u8, Vec<u8>)]) -> RedbTree {
        let db = redb::Database::create(file).unwrap();
        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(OBJECTS).unwrap();
            for (oid, kind, body) in objects {
                let stored = [&[*kind, CODEC_RAW][..], body].concat();
                table.insert(&[*oid; 20][..], &stored[..]).unwrap();
            }
            let mut meta = txn.open_table(META).unwrap();
            meta.insert("tree", hex::encode([root; 20]).as_bytes())
                .unwrap();
            meta.insert("schema", TREE_SCHEMA.as_bytes()).unwrap();
        }
        txn.commit().unwrap();
        drop(db);
        RedbTree::open(file).unwrap()
    }

    fn tree_body(entries: &[(&str, &str, u8)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (mode, name, oid) in entries {
            out.extend_from_slice(format!("{mode} {name}\0").as_bytes());
            out.extend_from_slice(&[*oid; 20]);
        }
        out
    }

    #[test]
    fn list_and_kind_walk_the_tree_objects() {
        let dir = tempfile::tempdir().unwrap();
        let dir = Utf8Path::from_path(dir.path()).unwrap();
        let tree = database(
            &dir.join("t.redb"),
            1,
            &[
                (1, KIND_TREE, tree_body(&[("40000", "eclass", 2)])),
                (
                    2,
                    KIND_TREE,
                    tree_body(&[("100644", "a.eclass", 3), ("40000", "tests", 4)]),
                ),
                (3, KIND_BLOB, b"text".to_vec()),
                (4, KIND_TREE, Vec::new()),
            ],
        );
        let mut names = tree.list(Utf8Path::new("eclass")).unwrap().unwrap();
        names.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            names,
            [
                ("a.eclass".into(), EntryKind::File),
                ("tests".into(), EntryKind::Dir)
            ]
        );
        assert_eq!(
            tree.kind(Utf8Path::new("eclass/a.eclass")).unwrap(),
            Some(EntryKind::File)
        );
        assert_eq!(
            tree.kind(Utf8Path::new("eclass/tests")).unwrap(),
            Some(EntryKind::Dir)
        );
        assert_eq!(tree.kind(Utf8Path::new("eclass/none")).unwrap(), None);
        assert_eq!(tree.list(Utf8Path::new("eclass/a.eclass")).unwrap(), None);
        assert_eq!(tree.list(Utf8Path::new("profiles")).unwrap(), None);
    }

    #[test]
    fn checkout_copies_modes_and_link_targets_and_prunes_only_unused_copies() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let dir = Utf8Path::from_path(dir.path()).unwrap();
        let copies = dir.join("copies");
        let age = |path: &Utf8Path, days: u64| {
            std::fs::create_dir_all(path).unwrap();
            let when = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86400);
            std::fs::File::open(path)
                .unwrap()
                .set_modified(when)
                .unwrap();
        };
        let stale = copies.join(hex::encode([9u8; 20]));
        let in_use = copies.join(hex::encode([8u8; 20]));
        let left_behind = copies.join(".staging-x");
        age(&stale, 8);
        age(&left_behind, 8);
        age(&in_use, 1);

        let tree = database(
            &dir.join("t.redb"),
            1,
            &[
                (
                    1,
                    KIND_TREE,
                    tree_body(&[("40000", "eclass", 2), ("40000", "skipped", 2)]),
                ),
                (
                    2,
                    KIND_TREE,
                    tree_body(&[
                        ("100644", "a.eclass", 3),
                        ("100755", "run", 4),
                        ("120000", "b.eclass", 5),
                    ]),
                ),
                (3, KIND_BLOB, b"inherited".to_vec()),
                (4, KIND_BLOB, b"#!/bin/sh".to_vec()),
                (5, KIND_BLOB, b"a.eclass".to_vec()),
            ],
        );
        let rels = [Utf8Path::new("eclass"), Utf8Path::new("missing")];
        let root = tree.checkout(&rels, Some(&copies)).unwrap();
        assert_eq!(root, copies.join(hex::encode([1u8; 20])));
        let eclass = root.join("eclass");
        assert_eq!(
            std::fs::read_to_string(eclass.join("b.eclass")).unwrap(),
            "inherited"
        );
        assert!(
            !eclass
                .join("b.eclass")
                .symlink_metadata()
                .unwrap()
                .is_symlink(),
            "a link is copied as what it points to"
        );
        let mode = |p: &Utf8Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&eclass.join("run")), 0o755);
        assert!(!root.join("skipped").exists() && !root.join("missing").exists());
        assert!(
            !stale.exists() && !left_behind.exists(),
            "unused for a week: pruned"
        );
        assert!(in_use.exists(), "another em may still be building from it");

        std::fs::write(eclass.join("a.eclass"), "kept").unwrap();
        tree.checkout(&rels, Some(&copies)).unwrap();
        assert_eq!(
            std::fs::read_to_string(eclass.join("a.eclass")).unwrap(),
            "kept"
        );
        assert!(tree.checkout(&rels, None).is_err());
    }
}
