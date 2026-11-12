//! Writing the two databases: an import or a sync applied in one transaction
//!
//! The caller brings the tree diff and the objects it names; nothing here knows about git
//! transport. A failure before the commit leaves both databases as they were.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io;

use camino::{Utf8Path, Utf8PathBuf};
use portage_atom::{Cpn, Cpv};
use redb::{Database, ReadableDatabase, ReadableTable, TableError, WriteTransaction};

use super::schema::{
    CACHE_ENTRIES, CACHE_SCHEMA, EBUILDS, ECLASSES, KIND_BLOB, KIND_TREE, LINKED, LINKS,
    MD5_CACHE_DIR, META, MODE_DIR, OBJECTS, ObjectKind, Oid, PACKAGES, PATHS, REFCOUNT, SYNC_LOG,
    TREE_SCHEMA, check_schema, db_err, decode, encode, in_file, md5_cache_key, open_err, resolve,
    row_is_link, tree_entries,
};

/// How many sync records the tree database keeps
const KEEP_LOG: u64 = 50;

const MODE_TREE: u16 = 0o040000;
/// A submodule: names a commit of another repository, never stored here
const MODE_GITLINK: u16 = 0o160000;
/// A symlink: its blob is the target, so what it derives comes from what it points to
const MODE_LINK: u16 = 0o120000;

/// A tree entry: its git mode and object id
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeItem {
    /// Git mode (`0o100644`, `0o100755`, `0o120000`, `0o040000`, …)
    pub mode: u16,
    /// The object it names
    pub oid: Oid,
}

impl TreeItem {
    fn is_tree(self) -> bool {
        self.mode == MODE_TREE
    }
}

/// A path whose entry differs between the stored tree and the new one
#[derive(Clone, Debug)]
pub struct PathChange {
    /// Repo-relative path
    pub path: String,
    /// The entry in the stored tree; `None` when added
    pub old: Option<TreeItem>,
    /// The entry in the new tree; `None` when removed
    pub new: Option<TreeItem>,
}

/// A new commit for the databases: the changes from the stored tree and the objects they need
#[derive(Debug, Default)]
pub struct TreeUpdate {
    /// The stored commit the changes were computed against; `None` for an empty database
    pub from: Option<Oid>,
    /// The new commit
    pub commit: Oid,
    /// Its root tree
    pub tree: Oid,
    /// Its committer time, as recorded in `meta`
    pub commit_time: String,
    /// Where it was fetched from
    pub remote: String,
    /// Who signed it, when the signature was checked
    pub signer: Option<String>,
    /// Every entry that differs, directories included, as a tree diff reports them
    pub changes: Vec<PathChange>,
    /// The commit, its tree and every object the changes name that the database may lack
    pub objects: HashMap<Oid, (ObjectKind, Vec<u8>)>,
}

/// What an update changed
#[derive(Debug, Default)]
pub struct UpdateSummary {
    /// Files added
    pub files_added: usize,
    /// Files whose content or mode changed
    pub files_modified: usize,
    /// Files removed
    pub files_removed: usize,
    /// Objects stored
    pub objects_written: usize,
    /// Objects no tree entry names any more, dropped
    pub objects_deleted: usize,
    /// Packages that gained their first ebuild
    pub packages_added: Vec<Cpn>,
    /// Packages that lost their last ebuild
    pub packages_removed: Vec<Cpn>,
    /// Ebuilds added
    pub versions_added: Vec<Cpv>,
    /// Ebuilds removed
    pub versions_removed: Vec<Cpv>,
    /// Ebuilds whose text changed
    pub versions_modified: Vec<Cpv>,
    /// md5-cache entries written to the cache database
    pub cache_written: usize,
    /// md5-cache entries removed from it
    pub cache_removed: usize,
}

/// What a changed path means for the derived tables
enum Derived<'p> {
    Ebuild(Cpv),
    Eclass(&'p str),
    CacheEntry(&'p str),
    Other,
}

fn derived(path: &str) -> Derived<'_> {
    if let Some(key) = md5_cache_key(path) {
        return Derived::CacheEntry(key);
    }
    if let Some(name) = path
        .strip_prefix("eclass/")
        .and_then(|rest| rest.strip_suffix(".eclass"))
        .filter(|name| !name.contains('/'))
    {
        return Derived::Eclass(name);
    }
    ebuild_cpv(path).map_or(Derived::Other, Derived::Ebuild)
}

/// The cpv of `cat/pn/pf.ebuild`, when `pf` names a version of `pn`
fn ebuild_cpv(path: &str) -> Option<Cpv> {
    let mut parts = path.split('/');
    let (cat, pn, file) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let pf = file.strip_suffix(".ebuild")?;
    let cpv = Cpv::parse(&format!("{cat}/{pf}")).ok()?;
    (cpv.cpn.package.as_str() == pn).then_some(cpv)
}

/// The `ebuilds` table key of `cpv`: `cat/PF`
fn ebuild_key(cpv: &Cpv) -> String {
    format!("{}/{}-{}", cpv.cpn.category, cpv.cpn.package, cpv.version)
}

/// The objects of the tree database, read by the caller to compute its diff
pub struct ObjectReader {
    table: Option<redb::ReadOnlyTable<&'static [u8], &'static [u8]>>,
}

impl ObjectReader {
    /// The object `oid`; `None` when it is not stored
    pub fn get(&self, oid: &Oid) -> io::Result<Option<(ObjectKind, Vec<u8>)>> {
        let Some(table) = &self.table else {
            return Ok(None);
        };
        let Some(stored) = table.get(&oid[..]).map_err(db_err)? else {
            return Ok(None);
        };
        let (kind, body) = decode(stored.value())?;
        Ok(Some((ObjectKind::from_byte(kind)?, body)))
    }
}

/// The two databases of a repository, open for writing
pub struct TreeWriter {
    db: Database,
    file: Utf8PathBuf,
    cache_file: Utf8PathBuf,
}

impl TreeWriter {
    /// Open the databases of the repository at `location`; `None` when it has none yet
    pub fn open(location: &Utf8Path) -> io::Result<Option<Self>> {
        let file = super::tree_file(location);
        if !file.is_file() {
            return Ok(None);
        }
        let db = Database::open(&file).map_err(|e| in_file(&file, open_err(e, "another em")))?;
        check_schema(&db.begin_read().map_err(db_err)?, TREE_SCHEMA)
            .map_err(|e| db_err(format!("{file}: {e}")))?;
        Ok(Some(TreeWriter {
            db,
            cache_file: super::cache_file(location),
            file,
        }))
    }

    /// Create the databases of the repository at `location` from `update`, which must start from
    /// nothing: written under a staging name and renamed into place whole, so a failed import
    /// leaves no database behind
    ///
    /// Holds a lock on the parent directory throughout, since each import clears the staging name
    /// first, and refuses a database another import published meanwhile.
    pub fn import(location: &Utf8Path, update: TreeUpdate) -> io::Result<UpdateSummary> {
        let parent = location
            .parent()
            .filter(|p| !p.as_str().is_empty())
            .unwrap_or(Utf8Path::new("."));
        std::fs::create_dir_all(parent)?;
        let turn = std::fs::File::open(parent)?;
        turn.try_lock().map_err(|e| match e {
            std::fs::TryLockError::WouldBlock => io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("another em is importing a repository into {parent}"),
            ),
            std::fs::TryLockError::Error(e) => e,
        })?;
        if super::tree_file(location).symlink_metadata().is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{location}.redb was imported meanwhile; sync again to update it"),
            ));
        }
        let staged = Utf8PathBuf::from(format!("{location}.importing"));
        let (tree, cache) = (super::tree_file(&staged), super::cache_file(&staged));
        for leftover in [&tree, &cache] {
            match std::fs::remove_file(leftover) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        let db = Database::create(&tree).map_err(|e| db_err(format!("{tree}: {e}")))?;
        let mut writer = TreeWriter {
            db,
            file: tree.clone(),
            cache_file: cache.clone(),
        };
        let summary = writer.apply(update);
        drop(writer);
        let summary = match summary {
            Ok(summary) => summary,
            Err(e) => {
                let _ = std::fs::remove_file(&tree);
                let _ = std::fs::remove_file(&cache);
                return Err(e);
            }
        };
        // The tree last: its file is what readers look for.
        std::fs::rename(&cache, super::cache_file(location))?;
        std::fs::rename(&tree, super::tree_file(location))?;
        Ok(summary)
    }

    /// The stored commit and its root tree; `None` for an empty database
    pub fn head(&self) -> io::Result<Option<(Oid, Oid)>> {
        let txn = self.db.begin_read().map_err(db_err)?;
        let meta = match txn.open_table(META) {
            Ok(meta) => meta,
            Err(TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(db_err(e)),
        };
        let oid = |key: &str| -> io::Result<Option<Oid>> {
            let Some(hex) = meta.get(key).map_err(db_err)? else {
                return Ok(None);
            };
            let mut oid = [0u8; 20];
            hex::decode_to_slice(hex.value(), &mut oid).map_err(db_err)?;
            Ok(Some(oid))
        };
        match (oid("head")?, oid("tree")?) {
            (Some(head), Some(tree)) => Ok(Some((head, tree))),
            _ => Ok(None),
        }
    }

    /// The stored objects, for the caller's diff and for completing thin packs
    pub fn objects(&self) -> io::Result<ObjectReader> {
        let txn = self.db.begin_read().map_err(db_err)?;
        let table = match txn.open_table(OBJECTS) {
            Ok(table) => Some(table),
            Err(TableError::TableDoesNotExist(_)) => None,
            Err(e) => return Err(db_err(e)),
        };
        Ok(ObjectReader { table })
    }

    /// Bring both databases to `update.commit`: the tree in one transaction, then the md5-cache
    pub fn apply(&mut self, mut update: TreeUpdate) -> io::Result<UpdateSummary> {
        let stored = self.head()?.map(|(head, _)| head);
        if stored != update.from {
            return Err(db_err(format!(
                "{}: holds {}, the update was computed from {}",
                self.file,
                stored.map_or("nothing".into(), hex::encode),
                update.from.map_or("nothing".into(), hex::encode),
            )));
        }
        let prepared = prepare(std::mem::take(&mut update.objects), &update.changes);
        let mut txn = self.db.begin_write().map_err(db_err)?;
        txn.set_quick_repair(false);
        let (mut summary, cache_changes) = apply_tree(&txn, &update, &prepared)?;
        txn.commit().map_err(db_err)?;
        while self.db.compact().map_err(db_err)? {}
        let at = self.cache_commit()?;
        let from = update.from.filter(|from| at == Some(*from));
        self.update_cache(update.commit, from, &cache_changes, &mut summary)?;
        Ok(summary)
    }

    /// Rebuild the md5-cache database when a failed write left it behind the stored tree;
    /// whether it had to
    pub fn catch_up_cache(&self) -> io::Result<bool> {
        let Some((head, _)) = self.head()? else {
            return Ok(false);
        };
        if self.cache_commit()? == Some(head) {
            return Ok(false);
        }
        self.update_cache(head, None, &[], &mut UpdateSummary::default())?;
        Ok(true)
    }

    /// The commit the md5-cache database was last brought to
    fn cache_commit(&self) -> io::Result<Option<Oid>> {
        if !self.cache_file.is_file() {
            return Ok(None);
        }
        let cache = Database::open(&self.cache_file)
            .map_err(|e| db_err(format!("{}: {e}", self.cache_file)))?;
        let txn = cache.begin_read().map_err(db_err)?;
        let meta = match txn.open_table(META) {
            Ok(meta) => meta,
            Err(TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(db_err(e)),
        };
        let Some(hex) = meta.get("tree_commit").map_err(db_err)? else {
            return Ok(None);
        };
        let mut oid = [0u8; 20];
        Ok(hex::decode_to_slice(hex.value(), &mut oid)
            .ok()
            .map(|()| oid))
    }

    /// Bring the md5-cache database to `commit`: only `changes` when it is at `from`, otherwise
    /// every entry copied from the tree
    fn update_cache(
        &self,
        commit: Oid,
        from: Option<Oid>,
        changes: &[(String, Option<Oid>)],
        summary: &mut UpdateSummary,
    ) -> io::Result<()> {
        let shown = |e: io::Error| db_err(format!("{}: {e}", self.cache_file));
        let mut cache = Database::create(&self.cache_file).map_err(|e| shown(db_err(e)))?;
        let incremental = from.is_some();
        let tree_txn = self.db.begin_read().map_err(db_err)?;
        let objects = tree_txn.open_table(OBJECTS).map_err(db_err)?;
        let rows: CacheChanges = if incremental {
            changes.to_vec()
        } else {
            let paths = tree_txn.open_table(PATHS).map_err(db_err)?;
            let mut rows = Vec::new();
            for row in paths.range(MD5_CACHE_DIR..).map_err(db_err)? {
                let (path, entry) = row.map_err(db_err)?;
                if !path.value().starts_with(MD5_CACHE_DIR) {
                    break;
                }
                if let Some(key) = md5_cache_key(path.value())
                    && !row_is_link(entry.value())
                {
                    rows.push((key.to_string(), Some(path_oid(entry.value())?)));
                }
            }
            rows
        };
        let txn = cache.begin_write().map_err(db_err)?;
        {
            if !incremental {
                txn.delete_table(CACHE_ENTRIES).map_err(db_err)?;
            }
            let mut entries = txn.open_table(CACHE_ENTRIES).map_err(db_err)?;
            for (key, oid) in &rows {
                match oid {
                    Some(oid) => {
                        // A blob is stored in the tree as the cache stores an entry.
                        let stored = objects.get(&oid[..]).map_err(db_err)?.ok_or_else(|| {
                            db_err(format!("{key}: blob {} missing", hex::encode(oid)))
                        })?;
                        entries
                            .insert(key.as_str(), stored.value())
                            .map_err(db_err)?;
                        summary.cache_written += 1;
                    }
                    None => {
                        entries.remove(key.as_str()).map_err(db_err)?;
                        summary.cache_removed += 1;
                    }
                }
            }
            let mut meta = txn.open_table(META).map_err(db_err)?;
            meta.insert("tree_commit", hex::encode(commit).as_bytes())
                .map_err(db_err)?;
            meta.insert("schema", CACHE_SCHEMA.as_bytes())
                .map_err(db_err)?;
        }
        txn.commit().map_err(db_err)?;
        while cache.compact().map_err(db_err)? {}
        Ok(())
    }
}

/// The oid of a `paths` value: `[mode u16 BE][oid]`
fn path_oid(entry: &[u8]) -> io::Result<Oid> {
    entry
        .get(2..22)
        .and_then(|o| Oid::try_from(o).ok())
        .ok_or_else(|| db_err("malformed path entry"))
}

/// The received objects encoded for storage, and the md5 of the ebuilds and eclasses among them
struct Prepared {
    stored: HashMap<Oid, Vec<u8>>,
    digests: HashMap<Oid, md5::Digest>,
}

/// [`Prepared`] from `objects`, on every core; each body is dropped once encoded
fn prepare(objects: HashMap<Oid, (ObjectKind, Vec<u8>)>, changes: &[PathChange]) -> Prepared {
    let digested: HashSet<Oid> = changes
        .iter()
        .filter(|c| matches!(derived(&c.path), Derived::Ebuild(_) | Derived::Eclass(_)))
        .filter_map(|c| c.new.filter(|i| i.mode != MODE_LINK).map(|i| i.oid))
        .collect();
    let (encoded, _) = crate::entries::par_map(
        objects.into_iter().collect(),
        || (),
        |(), (oid, (kind, body))| {
            let digest = digested.contains(&oid).then(|| md5::compute(&body));
            (oid, encode(kind, &body), digest)
        },
    );
    let mut prepared = Prepared {
        stored: HashMap::with_capacity(encoded.len()),
        digests: HashMap::new(),
    };
    for (oid, stored, digest) in encoded {
        prepared.stored.insert(oid, stored);
        prepared.digests.extend(digest.map(|d| (oid, d)));
    }
    prepared
}

/// The md5 of a blob: received with the update, or already stored when only its path changed
fn blob_digest(
    prepared: &Prepared,
    objects: &redb::Table<&[u8], &[u8]>,
    oid: &Oid,
) -> io::Result<md5::Digest> {
    if let Some(digest) = prepared.digests.get(oid) {
        return Ok(*digest);
    }
    let stored = objects.get(&oid[..]).map_err(db_err)?.ok_or_else(|| {
        db_err(format!(
            "blob {} neither received nor stored",
            hex::encode(oid)
        ))
    })?;
    Ok(md5::compute(decode(stored.value())?.1))
}

/// md5-cache entries an update changed: `cat/PF` and the new entry's blob, `None` when removed
type CacheChanges = Vec<(String, Option<Oid>)>;

/// The versions of each `cat/PN` an update `[added, removed, modified]`
type Touched = BTreeMap<String, [BTreeSet<Cpv>; 3]>;

/// Indices into a package's `[added, removed, modified]` versions
const ADDED: usize = 0;
const REMOVED: usize = 1;
const MODIFIED: usize = 2;

/// The tree database's part of [`TreeWriter::apply`], and the md5-cache entries it changed
fn apply_tree(
    txn: &WriteTransaction,
    update: &TreeUpdate,
    prepared: &Prepared,
) -> io::Result<(UpdateSummary, CacheChanges)> {
    let mut summary = UpdateSummary::default();
    let mut cache_changes = Vec::new();
    let mut touched = Touched::new();
    {
        let mut objects = txn.open_table(OBJECTS).map_err(db_err)?;
        let mut refcount = txn.open_table(REFCOUNT).map_err(db_err)?;
        let mut paths = txn.open_table(PATHS).map_err(db_err)?;
        let mut ebuilds = txn.open_table(EBUILDS).map_err(db_err)?;
        let mut eclasses = txn.open_table(ECLASSES).map_err(db_err)?;
        let mut links = txn.open_table(LINKS).map_err(db_err)?;
        let mut linked = txn.open_table(LINKED).map_err(db_err)?;

        // The root tree and the commit hold one reference each.
        let mut incs = vec![update.tree, update.commit];
        let mut decs = Vec::new();
        if let (Some(from), Some(old_tree)) = (update.from, stored_tree(txn)?) {
            decs.extend([old_tree, from]);
        }
        // Applied in key order afterwards: git tree order is not the byte order of full paths.
        let mut path_rows: BTreeMap<&str, Option<[u8; 22]>> = BTreeMap::new();
        let mut link_rows: BTreeMap<&str, bool> = BTreeMap::new();
        for change in &update.changes {
            let counted = |item: &Option<TreeItem>| item.filter(|i| i.mode != MODE_GITLINK);
            if counted(&change.old).map(|i| i.oid) != counted(&change.new).map(|i| i.oid) {
                incs.extend(counted(&change.new).map(|i| i.oid));
                decs.extend(counted(&change.old).map(|i| i.oid));
            }
            let file = |item: &Option<TreeItem>| item.filter(|i| !i.is_tree());
            let (old, new) = (file(&change.old), file(&change.new));
            match (old, new) {
                (None, None) => continue,
                (None, Some(_)) => summary.files_added += 1,
                (Some(_), None) => summary.files_removed += 1,
                (Some(_), Some(_)) => summary.files_modified += 1,
            }
            path_rows.insert(
                change.path.as_str(),
                new.map(|item| {
                    let mut row = [0u8; 22];
                    row[..2].copy_from_slice(&item.mode.to_be_bytes());
                    row[2..].copy_from_slice(&item.oid);
                    row
                }),
            );
            let is_link = |item: Option<TreeItem>| item.is_some_and(|i| i.mode == MODE_LINK);
            if is_link(new) || is_link(old) {
                link_rows.insert(change.path.as_str(), is_link(new));
            }
            // A link derives nothing itself: the pass over `links` below handles what it reaches.
            let content = |item: Option<TreeItem>| item.filter(|i| i.mode != MODE_LINK);
            let (old, new) = (content(old), content(new));
            if old.is_none() && new.is_none() {
                continue;
            }
            if let Derived::CacheEntry(key) = derived(&change.path) {
                cache_changes.push((key.to_string(), new.map(|i| i.oid)));
                continue;
            }
            // Read before the object table changes, so a blob that only moved is still found.
            let digest = match new {
                Some(item) if !matches!(derived(&change.path), Derived::Other) => {
                    Some(blob_digest(prepared, &objects, &item.oid)?)
                }
                _ => None,
            };
            let mut tables = DerivedTables {
                ebuilds: &mut ebuilds,
                eclasses: &mut eclasses,
                touched: &mut touched,
            };
            tables.set(&change.path, digest)?;
        }
        for (path, row) in &path_rows {
            match row {
                Some(row) => paths.insert(*path, &row[..]).map(drop),
                None => paths.remove(*path).map(drop),
            }
            .map_err(db_err)?;
        }
        for (path, is_link) in &link_rows {
            match is_link {
                true => links.insert(*path, ()).map(drop),
                false => links.remove(*path).map(drop),
            }
            .map_err(db_err)?;
        }

        // Increments first, so an object that moves keeps its body; key order keeps pages dense.
        incs.sort_unstable();
        decs.sort_unstable();
        for oid in &incs {
            let count = refcount
                .get(&oid[..])
                .map_err(db_err)?
                .map_or(0, |v| v.value());
            if count == 0 && objects.get(&oid[..]).map_err(db_err)?.is_none() {
                let stored = prepared.stored.get(oid).ok_or_else(|| {
                    db_err(format!(
                        "object {} neither received nor stored",
                        hex::encode(oid)
                    ))
                })?;
                objects.insert(&oid[..], &stored[..]).map_err(db_err)?;
                summary.objects_written += 1;
            }
            refcount.insert(&oid[..], count + 1).map_err(db_err)?;
        }
        for oid in &decs {
            let count = refcount
                .get(&oid[..])
                .map_err(db_err)?
                .map(|v| v.value())
                .ok_or_else(|| db_err(format!("no references to {}", hex::encode(oid))))?;
            if count <= 1 {
                refcount.remove(&oid[..]).map_err(db_err)?;
                objects.remove(&oid[..]).map_err(db_err)?;
                summary.objects_deleted += 1;
            } else {
                refcount.insert(&oid[..], count - 1).map_err(db_err)?;
            }
        }

        // After the objects: links resolve in the new tree.
        let now = linked_files(&objects, &links, &update.tree)?;
        let mut before = BTreeMap::new();
        for row in linked.iter().map_err(db_err)? {
            let (path, digest) = row.map_err(db_err)?;
            before.insert(path.value().to_string(), digest.value().to_vec());
        }
        let mut tables = DerivedTables {
            ebuilds: &mut ebuilds,
            eclasses: &mut eclasses,
            touched: &mut touched,
        };
        for path in before.keys().filter(|p| !now.contains_key(*p)) {
            // A file may have replaced the link in this update, and set its rows already.
            let replaced = paths
                .get(path.as_str())
                .map_err(db_err)?
                .is_some_and(|row| !row_is_link(row.value()));
            if !replaced {
                tables.set(path, None)?;
            }
            linked.remove(path.as_str()).map_err(db_err)?;
        }
        for (path, digest) in &now {
            if before.get(path).map(Vec::as_slice) != Some(&digest.0[..]) {
                tables.set(path, Some(*digest))?;
                linked
                    .insert(path.as_str(), &digest.0[..])
                    .map_err(db_err)?;
            }
        }
    }
    update_packages(txn, &touched, &mut summary)?;
    write_log(txn, update, &summary)?;
    let mut meta = txn.open_table(META).map_err(db_err)?;
    for (key, value) in [
        ("head", hex::encode(update.commit)),
        ("tree", hex::encode(update.tree)),
        ("remote", update.remote.clone()),
        ("commit_time", update.commit_time.clone()),
        ("schema", TREE_SCHEMA.to_string()),
    ] {
        meta.insert(key, value.as_bytes()).map_err(db_err)?;
    }
    Ok((summary, cache_changes))
}

/// The `ebuilds` and `eclasses` tables, and the versions an update touched
struct DerivedTables<'t, 'txn> {
    ebuilds: &'t mut redb::Table<'txn, &'static str, &'static [u8]>,
    eclasses: &'t mut redb::Table<'txn, &'static str, &'static [u8]>,
    touched: &'t mut Touched,
}

impl DerivedTables<'_, '_> {
    /// Record the ebuild or eclass at `path` with the md5 of its text, or as gone
    fn set(&mut self, path: &str, digest: Option<md5::Digest>) -> io::Result<()> {
        match derived(path) {
            Derived::Eclass(name) => match digest {
                Some(digest) => self
                    .eclasses
                    .insert(name, format!("{digest:x}").as_bytes())
                    .map(drop),
                None => self.eclasses.remove(name).map(drop),
            }
            .map_err(db_err),
            Derived::Ebuild(cpv) => {
                let key = ebuild_key(&cpv);
                let slot = match digest {
                    Some(digest) => {
                        let existed = self
                            .ebuilds
                            .insert(key.as_str(), &digest.0[..])
                            .map_err(db_err)?
                            .is_some();
                        if existed { MODIFIED } else { ADDED }
                    }
                    None => {
                        self.ebuilds.remove(key.as_str()).map_err(db_err)?;
                        REMOVED
                    }
                };
                let cpn = format!("{}/{}", cpv.cpn.category, cpv.cpn.package);
                self.touched.entry(cpn).or_default()[slot].insert(cpv);
                Ok(())
            }
            Derived::CacheEntry(_) | Derived::Other => Ok(()),
        }
    }
}

/// Every ebuild and eclass reached through a symlink in the tree `root`, by the path that
/// reaches it, with the md5 of its text
fn linked_files(
    objects: &redb::Table<&[u8], &[u8]>,
    links: &redb::Table<&str, ()>,
    root: &Oid,
) -> io::Result<BTreeMap<String, md5::Digest>> {
    let object = |oid: &Oid| -> io::Result<Option<(u8, Vec<u8>)>> {
        match objects.get(&oid[..]).map_err(db_err)? {
            Some(stored) => Ok(Some(decode(stored.value())?)),
            None => Ok(None),
        }
    };
    let mut pending = Vec::new();
    for row in links.iter().map_err(db_err)? {
        pending.push(row.map_err(db_err)?.0.value().to_string());
    }
    let mut out = BTreeMap::new();
    while let Some(path) = pending.pop() {
        // A broken link or a loop derives nothing; reading through it fails the same way.
        let Ok(Some((mode, oid))) = resolve(root, &path, true, object) else {
            continue;
        };
        if mode == MODE_DIR {
            // Ebuilds are three levels deep and eclasses two: nothing deeper derives a row.
            if path.matches('/').count() < 2
                && let Some((KIND_TREE, data)) = object(&oid)?
            {
                pending.extend(
                    tree_entries(&data)
                        .filter_map(|e| std::str::from_utf8(e.name).ok())
                        .map(|name| format!("{path}/{name}")),
                );
            }
        } else if matches!(derived(&path), Derived::Ebuild(_) | Derived::Eclass(_))
            && let Some((KIND_BLOB, text)) = object(&oid)?
        {
            out.insert(path, md5::compute(&text));
        }
    }
    Ok(out)
}

/// The root tree recorded in `meta`, read inside the write transaction
fn stored_tree(txn: &WriteTransaction) -> io::Result<Option<Oid>> {
    let meta = txn.open_table(META).map_err(db_err)?;
    let Some(hex) = meta.get("tree").map_err(db_err)? else {
        return Ok(None);
    };
    let mut oid = [0u8; 20];
    hex::decode_to_slice(hex.value(), &mut oid).map_err(db_err)?;
    Ok(Some(oid))
}

/// The `packages` table (`cat/PN` → its versions, one per line) for the packages `touched`
fn update_packages(
    txn: &WriteTransaction,
    touched: &BTreeMap<String, [BTreeSet<Cpv>; 3]>,
    summary: &mut UpdateSummary,
) -> io::Result<()> {
    let mut packages = txn.open_table(PACKAGES).map_err(db_err)?;
    for (cpn, [added, removed, modified]) in touched {
        // Removed and added in one update, as when a link replaces a file: changed.
        let moved: BTreeSet<Cpv> = added.intersection(removed).cloned().collect();
        let before: BTreeSet<String> = packages
            .get(cpn.as_str())
            .map_err(db_err)?
            .map(|v| v.value().lines().map(String::from).collect())
            .unwrap_or_default();
        let mut after = before.clone();
        for cpv in removed {
            after.remove(&cpv.version.to_string());
        }
        after.extend(added.iter().map(|cpv| cpv.version.to_string()));
        if after.is_empty() {
            packages.remove(cpn.as_str()).map_err(db_err)?;
        } else {
            let lines: Vec<&str> = after.iter().map(String::as_str).collect();
            packages
                .insert(cpn.as_str(), lines.join("\n").as_str())
                .map_err(db_err)?;
        }
        if let Some(cpv) = added.iter().chain(removed).next() {
            match (before.is_empty(), after.is_empty()) {
                (true, false) => summary.packages_added.push(cpv.cpn),
                (false, true) => summary.packages_removed.push(cpv.cpn),
                _ => {}
            }
        }
        summary
            .versions_added
            .extend(added.difference(&moved).cloned());
        summary
            .versions_removed
            .extend(removed.difference(&moved).cloned());
        summary
            .versions_modified
            .extend(modified.union(&moved).cloned());
    }
    Ok(())
}

/// Append the update's record to `sync_log`, keeping the last [`KEEP_LOG`]
fn write_log(
    txn: &WriteTransaction,
    update: &TreeUpdate,
    summary: &UpdateSummary,
) -> io::Result<()> {
    let mut log = txn.open_table(SYNC_LOG).map_err(db_err)?;
    let seq = log
        .last()
        .map_err(db_err)?
        .map_or(1, |(k, _)| k.value() + 1);
    let unix_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    // An import adds every version: its record keeps the count.
    let names = |cpvs: &[Cpv]| match update.from {
        Some(_) => serde_json::json!(cpvs.iter().map(Cpv::to_string).collect::<Vec<_>>()),
        None => serde_json::json!(cpvs.len()),
    };
    let record = serde_json::json!({
        "seq": seq,
        "kind": if update.from.is_some() { "sync" } else { "import" },
        "unix_time": unix_time,
        "remote": update.remote,
        "from": update.from.map(hex::encode),
        "to": hex::encode(update.commit),
        "commit_time": update.commit_time,
        "signed_by": update.signer,
        "files": {
            "added": summary.files_added,
            "modified": summary.files_modified,
            "removed": summary.files_removed,
        },
        "objects": { "written": summary.objects_written, "deleted": summary.objects_deleted },
        "versions": {
            "added": names(&summary.versions_added),
            "removed": names(&summary.versions_removed),
            "modified": names(&summary.versions_modified),
        },
    });
    let bytes = serde_json::to_vec(&record).map_err(db_err)?;
    log.insert(seq, &bytes[..]).map_err(db_err)?;
    if seq > KEEP_LOG {
        log.retain_in(..=seq - KEEP_LOG, |_, _| false)
            .map_err(db_err)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::tree::{EntryKind, TreeSource};

    /// Content-addressed, like git: equal bodies share one object
    fn oid(kind: ObjectKind, body: &[u8]) -> Oid {
        let digest = md5::compute([&[kind.byte()][..], body].concat());
        let mut oid = [0u8; 20];
        oid[..16].copy_from_slice(&digest.0);
        oid
    }

    /// A tree of `files`: every entry by path (directories included), the objects, the root
    struct Fixture {
        entries: BTreeMap<String, TreeItem>,
        objects: HashMap<Oid, (ObjectKind, Vec<u8>)>,
        root: Oid,
    }

    fn fixture(files: &[(&str, &str)]) -> Fixture {
        fn build(prefix: &str, files: &[(&str, &str)], out: &mut Fixture) -> Oid {
            let mut children: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new();
            let mut body = Vec::new();
            for (path, text) in files {
                match path.split_once('/') {
                    Some((dir, rest)) => children.entry(dir).or_default().push((rest, text)),
                    None => {
                        // `-> target` is a symlink.
                        let (mode, text) = match text.strip_prefix("-> ") {
                            Some(target) => (MODE_LINK, target),
                            None => (0o100644, *text),
                        };
                        let item = TreeItem {
                            mode,
                            oid: oid(ObjectKind::Blob, text.as_bytes()),
                        };
                        out.objects
                            .insert(item.oid, (ObjectKind::Blob, text.as_bytes().to_vec()));
                        out.entries.insert(format!("{prefix}{path}"), item);
                        body.extend_from_slice(format!("{mode:o} {path}\0").as_bytes());
                        body.extend_from_slice(&item.oid);
                    }
                }
            }
            for (dir, rest) in children {
                let full = format!("{prefix}{dir}");
                let sub = build(&format!("{full}/"), &rest, out);
                out.entries.insert(
                    full,
                    TreeItem {
                        mode: MODE_TREE,
                        oid: sub,
                    },
                );
                body.extend_from_slice(format!("40000 {dir}\0").as_bytes());
                body.extend_from_slice(&sub);
            }
            let tree = oid(ObjectKind::Tree, &body);
            out.objects.insert(tree, (ObjectKind::Tree, body));
            tree
        }
        let mut out = Fixture {
            entries: BTreeMap::new(),
            objects: HashMap::new(),
            root: [0; 20],
        };
        out.root = build("", files, &mut out);
        out
    }

    /// The update from `old` (`None`: an empty database) to `new`, as a tree diff gives it
    fn update(old: Option<(&Fixture, Oid)>, new: &Fixture, commit: Oid) -> TreeUpdate {
        let empty = BTreeMap::new();
        let before = old.map_or(&empty, |(f, _)| &f.entries);
        let paths: BTreeSet<&String> = before.keys().chain(new.entries.keys()).collect();
        let changes = paths
            .into_iter()
            .filter_map(|path| {
                let (old, new) = (before.get(path).copied(), new.entries.get(path).copied());
                (old != new).then(|| PathChange {
                    path: path.clone(),
                    old,
                    new,
                })
            })
            .collect();
        let mut objects = new.objects.clone();
        objects.insert(commit, (ObjectKind::Commit, b"commit".to_vec()));
        TreeUpdate {
            from: old.map(|(_, c)| c),
            commit,
            tree: new.root,
            commit_time: "0".into(),
            remote: "test".into(),
            signer: None,
            changes,
            objects,
        }
    }

    const ECLASS: (&str, &str) = ("eclass/e.eclass", "e v1");
    const CATEGORIES: (&str, &str) = ("profiles/categories", "app-misc\n");

    fn v1() -> Fixture {
        fixture(&[
            ("app-misc/bar/bar-1.ebuild", "bar 1"),
            ("app-misc/foo/foo-1.ebuild", "foo 1"),
            ECLASS,
            ("licenses/A", "same text"),
            ("licenses/B", "same text"),
            ("metadata/md5-cache/app-misc/bar-1", "bar entry"),
            ("metadata/md5-cache/app-misc/foo-1", "foo 1 entry"),
            CATEGORIES,
        ])
    }

    fn v2() -> Fixture {
        fixture(&[
            ("app-misc/foo/foo-1.ebuild", "foo 1 edited"),
            ("app-misc/foo/foo-2.ebuild", "foo 2"),
            ECLASS,
            ("licenses/A", "same text"),
            ("metadata/md5-cache/app-misc/foo-1", "foo 1 entry edited"),
            ("metadata/md5-cache/app-misc/foo-2", "foo 2 entry"),
            CATEGORIES,
        ])
    }

    fn location() -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let location = Utf8Path::from_path(dir.path()).unwrap().join("repo");
        (dir, location)
    }

    fn read(tree: &dyn TreeSource, path: &str) -> Option<String> {
        let bytes = tree.read(Utf8Path::new(path)).unwrap()?;
        Some(String::from_utf8(bytes).unwrap())
    }

    fn cache_keys(location: &Utf8Path) -> Vec<String> {
        let storage = super::super::storage_at(location).unwrap().unwrap();
        let mut keys: Vec<_> = storage
            .entries
            .raw_entries(false)
            .unwrap()
            .unwrap()
            .into_iter()
            .map(|(cpv, text)| format!("{cpv}={text}"))
            .collect();
        keys.sort();
        keys
    }

    #[test]
    fn an_import_reads_back_through_the_reader() {
        let (_dir, location) = location();
        let tree = v1();
        assert!(TreeWriter::open(&location).unwrap().is_none());
        let summary = TreeWriter::import(&location, update(None, &tree, [1; 20])).unwrap();
        assert_eq!(summary.files_added, 8);
        assert_eq!(summary.versions_added.len(), 2);
        assert_eq!(summary.cache_written, 2);
        let writer = TreeWriter::open(&location).unwrap().unwrap();
        assert_eq!(writer.head().unwrap(), Some(([1; 20], tree.root)));
        drop(writer);

        let storage = super::super::storage_at(&location).unwrap().unwrap();
        assert!(!storage.tree.is_checkout());
        assert_eq!(
            read(&*storage.tree, "app-misc/foo/foo-1.ebuild").unwrap(),
            "foo 1"
        );
        assert_eq!(read(&*storage.tree, "licenses/B").unwrap(), "same text");
        let mut index = storage.tree.ebuild_index().unwrap().unwrap();
        index.sort();
        assert_eq!(
            index,
            ["app-misc/bar/bar-1.ebuild", "app-misc/foo/foo-1.ebuild"]
        );
        assert_eq!(
            cache_keys(&location),
            ["app-misc/bar-1=bar entry", "app-misc/foo-1=foo 1 entry"]
        );
    }

    #[test]
    fn a_sync_applies_the_changes_and_drops_what_nothing_names() {
        let (_dir, location) = location();
        let (old, new) = (v1(), v2());
        TreeWriter::import(&location, update(None, &old, [1; 20])).unwrap();
        let mut writer = TreeWriter::open(&location).unwrap().unwrap();
        let summary = writer
            .apply(update(Some((&old, [1; 20])), &new, [2; 20]))
            .unwrap();

        let names = |cpvs: &[Cpv]| cpvs.iter().map(Cpv::to_string).collect::<Vec<_>>();
        assert_eq!(names(&summary.versions_added), ["app-misc/foo-2"]);
        assert_eq!(names(&summary.versions_modified), ["app-misc/foo-1"]);
        assert_eq!(names(&summary.versions_removed), ["app-misc/bar-1"]);
        assert_eq!(
            summary
                .packages_removed
                .iter()
                .map(Cpn::to_string)
                .collect::<Vec<_>>(),
            ["app-misc/bar"]
        );
        assert!(summary.packages_added.is_empty());
        // Incremental: only the md5-cache entries the sync touched.
        assert_eq!((summary.cache_written, summary.cache_removed), (2, 1));

        let objects = writer.objects().unwrap();
        let gone = oid(ObjectKind::Blob, b"bar 1");
        assert_eq!(
            objects.get(&gone).unwrap(),
            None,
            "no path names it any more"
        );
        let shared = oid(ObjectKind::Blob, b"same text");
        assert!(
            objects.get(&shared).unwrap().is_some(),
            "licenses/A still names it"
        );
        assert_eq!(
            objects.get(&old.root).unwrap(),
            None,
            "the old root tree is dropped"
        );
        drop((objects, writer));

        let storage = super::super::storage_at(&location).unwrap().unwrap();
        assert_eq!(read(&*storage.tree, "licenses/B"), None);
        assert_eq!(read(&*storage.tree, "licenses/A").unwrap(), "same text");
        assert_eq!(read(&*storage.tree, "app-misc/bar/bar-1.ebuild"), None);
        assert_eq!(
            cache_keys(&location),
            [
                "app-misc/foo-1=foo 1 entry edited",
                "app-misc/foo-2=foo 2 entry"
            ]
        );
    }

    #[test]
    fn a_failed_update_leaves_the_database_as_it_was() {
        let (_dir, location) = location();
        let (old, new) = (v1(), v2());
        TreeWriter::import(&location, update(None, &old, [1; 20])).unwrap();
        let mut writer = TreeWriter::open(&location).unwrap().unwrap();

        let stale = update(None, &new, [2; 20]);
        assert!(
            writer.apply(stale).is_err(),
            "computed against another head"
        );

        let mut missing = update(Some((&old, [1; 20])), &new, [2; 20]);
        missing.objects.remove(&oid(ObjectKind::Blob, b"foo 2"));
        assert!(writer.apply(missing).is_err());
        assert_eq!(writer.head().unwrap(), Some(([1; 20], old.root)));
        drop(writer);
        assert_eq!(
            cache_keys(&location),
            ["app-misc/bar-1=bar entry", "app-misc/foo-1=foo 1 entry"]
        );
    }

    #[test]
    fn a_failed_import_leaves_no_database() {
        let (dir, location) = location();
        let mut broken = update(None, &v1(), [1; 20]);
        broken.objects.remove(&oid(ObjectKind::Blob, b"foo 1"));
        assert!(TreeWriter::import(&location, broken).is_err());
        assert!(TreeWriter::open(&location).unwrap().is_none());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn imports_into_one_directory_take_turns_and_create_missing_parents() {
        let (dir, location) = location();
        let held = std::fs::File::open(dir.path()).unwrap();
        held.lock().unwrap();
        let err = TreeWriter::import(&location, update(None, &v1(), [1; 20])).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock, "{err}");
        drop(held);

        TreeWriter::import(&location, update(None, &v1(), [1; 20])).unwrap();
        // One published meanwhile is not replaced by a second import.
        let err = TreeWriter::import(&location, update(None, &v2(), [2; 20])).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{err}");
        let writer = TreeWriter::open(&location).unwrap().unwrap();
        assert_eq!(writer.head().unwrap().unwrap().0, [1; 20]);
        drop(writer);

        let nested = Utf8Path::from_path(dir.path())
            .unwrap()
            .join("db/repos/gentoo");
        TreeWriter::import(&nested, update(None, &v1(), [1; 20])).unwrap();
        assert!(TreeWriter::open(&nested).unwrap().is_some());
        assert!(!nested.exists(), "the location itself stays logical");
    }

    #[test]
    fn a_cache_database_behind_the_tree_is_copied_whole() {
        let (_dir, location) = location();
        let (old, new) = (v1(), v2());
        TreeWriter::import(&location, update(None, &old, [1; 20])).unwrap();
        let mut writer = TreeWriter::open(&location).unwrap().unwrap();
        std::fs::remove_file(super::super::cache_file(&location)).unwrap();
        let summary = writer
            .apply(update(Some((&old, [1; 20])), &new, [2; 20]))
            .unwrap();
        assert_eq!((summary.cache_written, summary.cache_removed), (2, 0));
        drop(writer);
        assert_eq!(
            cache_keys(&location),
            [
                "app-misc/foo-1=foo 1 entry edited",
                "app-misc/foo-2=foo 2 entry"
            ]
        );
    }

    #[test]
    fn a_cache_database_left_behind_catches_up_without_an_update() {
        let (_dir, location) = location();
        let (old, new) = (v1(), v2());
        TreeWriter::import(&location, update(None, &old, [1; 20])).unwrap();
        let cache = super::super::cache_file(&location);
        let stale = std::fs::read(&cache).unwrap();
        let mut writer = TreeWriter::open(&location).unwrap().unwrap();
        writer
            .apply(update(Some((&old, [1; 20])), &new, [2; 20]))
            .unwrap();
        std::fs::write(&cache, stale).unwrap();
        assert!(writer.catch_up_cache().unwrap());
        assert!(!writer.catch_up_cache().unwrap());
        drop(writer);
        assert_eq!(
            cache_keys(&location),
            [
                "app-misc/foo-1=foo 1 entry edited",
                "app-misc/foo-2=foo 2 entry"
            ]
        );
    }

    #[test]
    fn a_locked_or_corrupt_tree_never_falls_back_to_the_checkout() {
        let (_dir, location) = location();
        std::fs::create_dir_all(location.join("metadata")).unwrap();
        std::fs::write(location.join("metadata/layout.conf"), "masters =\n").unwrap();
        let tree = v1();
        TreeWriter::import(&location, update(None, &tree, [1; 20])).unwrap();
        let writer = TreeWriter::open(&location).unwrap().unwrap();
        assert_eq!(
            crate::tree::storage_at(&location).err().map(|e| e.kind()),
            Some(io::ErrorKind::ResourceBusy)
        );
        assert!(crate::tree::tree_at(&location).is_err());
        assert!(
            crate::Repository::builder()
                .in_memory_cache()
                .open(location.clone())
                .is_err()
        );
        drop(writer);
        assert!(
            !crate::tree::storage_at(&location)
                .unwrap()
                .tree
                .is_checkout()
        );
        std::fs::write(super::super::tree_file(&location), "not a redb database").unwrap();
        assert!(crate::tree::storage_at(&location).is_err());
        assert!(crate::tree::tree_at(&location).is_err());
    }

    #[test]
    fn read_only_processes_share_the_tree_and_exclude_a_writer() {
        const CHILD_LOCATION: &str = "EM_REVIEW_REDB_READER_CHILD";
        if let Some(location) = std::env::var_os(CHILD_LOCATION) {
            let location = Utf8PathBuf::from_path_buf(location.into()).unwrap();
            let storage = crate::tree::storage_at(&location).unwrap();
            assert_eq!(
                read(&*storage.tree, "licenses/A").as_deref(),
                Some("same text")
            );
            assert!(TreeWriter::open(&location).is_err());
            return;
        }
        let (_dir, location) = location();
        let tree = v1();
        TreeWriter::import(&location, update(None, &tree, [1; 20])).unwrap();
        let reader = crate::tree::storage_at(&location).unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tree::redb::write::tests::read_only_processes_share_the_tree_and_exclude_a_writer",
            ])
            .env(CHILD_LOCATION, location.as_str())
            .status()
            .unwrap();
        assert!(child.success());
        assert_eq!(
            TreeWriter::open(&location).err().map(|e| e.kind()),
            Some(io::ErrorKind::ResourceBusy)
        );
        drop(reader);
        assert!(TreeWriter::open(&location).unwrap().is_some());
    }

    /// Every row of a `&str`-keyed table of the tree database, read with nothing else open
    fn rows(
        location: &Utf8Path,
        table: redb::TableDefinition<&str, &[u8]>,
    ) -> BTreeMap<String, Vec<u8>> {
        let db = redb::ReadOnlyDatabase::open(super::super::tree_file(location)).unwrap();
        let txn = db.begin_read().unwrap();
        let table = txn.open_table(table).unwrap();
        table
            .iter()
            .unwrap()
            .map(|row| {
                let (k, v) = row.unwrap();
                (k.value().to_string(), v.value().to_vec())
            })
            .collect()
    }

    fn sorted(cpvs: &[Cpv]) -> Vec<String> {
        let mut out: Vec<String> = cpvs.iter().map(Cpv::to_string).collect();
        out.sort();
        out
    }

    #[test]
    fn symlinks_read_and_derive_as_what_they_point_to() {
        let (dir, location) = location();
        let v1 = fixture(&[
            ("app-misc/foo/foo-1.ebuild", "foo 1"),
            ("app-misc/foo/foo-2.ebuild", "-> foo-1.ebuild"),
            ("app-misc/foo/files/p.patch", "-> ../../bar/files/p.patch"),
            ("app-misc/bar/bar-1.ebuild", "bar 1"),
            ("app-misc/bar/files/p.patch", "patch"),
            ("app-misc/out", "-> ../../elsewhere"),
            ("app-misc/loop", "-> loop"),
            ("cross-x/foo", "-> ../app-misc/foo"),
            ("eclass/e.eclass", "e v1"),
            ("eclass/f.eclass", "-> e.eclass"),
        ]);
        TreeWriter::import(&location, update(None, &v1, [1; 20])).unwrap();
        let md5 = |text: &str| md5::compute(text).0.to_vec();
        let ebuilds = rows(&location, EBUILDS);
        assert_eq!(ebuilds["app-misc/foo-2"], md5("foo 1"));
        assert_eq!(ebuilds["cross-x/foo-1"], md5("foo 1"));
        assert_eq!(
            rows(&location, ECLASSES)["f"],
            format!("{:x}", md5::compute("e v1")).into_bytes()
        );

        let storage = super::super::storage_at(&location).unwrap().unwrap();
        let tree = &*storage.tree;
        assert_eq!(read(tree, "app-misc/foo/foo-2.ebuild").unwrap(), "foo 1");
        assert_eq!(read(tree, "cross-x/foo/foo-2.ebuild").unwrap(), "foo 1");
        assert_eq!(read(tree, "app-misc/foo/files/p.patch").unwrap(), "patch");
        assert_eq!(read(tree, "eclass/f.eclass").unwrap(), "e v1");
        assert_eq!(read(tree, "app-misc/out"), None, "nothing outside the tree");
        assert!(tree.read(Utf8Path::new("app-misc/loop")).is_err());
        assert_eq!(
            tree.kind(Utf8Path::new("cross-x/foo")).unwrap(),
            Some(EntryKind::Dir)
        );
        let mut names = tree.list(Utf8Path::new("app-misc")).unwrap().unwrap();
        names.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            names,
            [
                ("bar".into(), EntryKind::Dir),
                ("foo".into(), EntryKind::Dir)
            ],
            "broken links and loops are left out"
        );
        assert_eq!(
            tree.ebuild_index().unwrap().unwrap(),
            [
                "app-misc/bar/bar-1.ebuild",
                "app-misc/foo/foo-1.ebuild",
                "app-misc/foo/foo-2.ebuild",
                "cross-x/foo/foo-1.ebuild",
                "cross-x/foo/foo-2.ebuild",
            ]
            .map(Utf8PathBuf::from)
        );
        let copies = Utf8Path::from_path(dir.path()).unwrap().join("copies");
        let copy = tree
            .checkout(&[Utf8Path::new("app-misc/foo")], Some(&copies))
            .unwrap();
        let patch = copy.join("app-misc/foo/files/p.patch");
        assert!(!patch.symlink_metadata().unwrap().is_symlink());
        assert_eq!(std::fs::read_to_string(patch).unwrap(), "patch");
        drop(storage);

        // The link's target changes, a link becomes a file, a file becomes a link, a link goes.
        let v2 = fixture(&[
            ("app-misc/foo/foo-1.ebuild", "foo 1 edited"),
            ("app-misc/foo/foo-2.ebuild", "foo 2"),
            ("app-misc/foo/files/p.patch", "-> ../../bar/files/p.patch"),
            ("app-misc/bar/bar-1.ebuild", "-> ../foo/foo-1.ebuild"),
            ("app-misc/bar/files/p.patch", "patch"),
            ("eclass/e.eclass", "e v2"),
            ("eclass/f.eclass", "-> e.eclass"),
        ]);
        let mut writer = TreeWriter::open(&location).unwrap().unwrap();
        let summary = writer
            .apply(update(Some((&v1, [1; 20])), &v2, [2; 20]))
            .unwrap();
        drop(writer);
        assert_eq!(
            sorted(&summary.versions_modified),
            ["app-misc/bar-1", "app-misc/foo-1", "app-misc/foo-2"]
        );
        assert_eq!(
            sorted(&summary.versions_removed),
            ["cross-x/foo-1", "cross-x/foo-2"]
        );
        assert!(summary.versions_added.is_empty());
        assert_eq!(
            summary.packages_removed,
            [Cpn::parse("cross-x/foo").unwrap()]
        );
        let ebuilds = rows(&location, EBUILDS);
        assert_eq!(ebuilds["app-misc/bar-1"], md5("foo 1 edited"));
        assert_eq!(ebuilds["app-misc/foo-2"], md5("foo 2"));
        assert!(!ebuilds.contains_key("cross-x/foo-1"));
        assert_eq!(
            rows(&location, ECLASSES)["f"],
            format!("{:x}", md5::compute("e v2")).into_bytes()
        );
        assert_eq!(
            rows(&location, LINKED).into_keys().collect::<Vec<_>>(),
            ["app-misc/bar/bar-1.ebuild", "eclass/f.eclass"]
        );
    }

    /// Links of every shape the filesystem resolves inside the tree, as `fixture` spells them
    const LINK_SHAPES: &[(&str, &str)] = &[
        ("app-misc/foo/foo-1.ebuild", "foo 1"),
        ("app-misc/foo/foo-2.ebuild", "-> foo-1.ebuild"),
        ("app-misc/foo/files/p.patch", "-> ../../bar/files/p.patch"),
        ("app-misc/foo/files/chain", "-> chain2"),
        ("app-misc/foo/files/chain2", "-> ../foo-1.ebuild"),
        ("app-misc/foo/files/dirlink", "-> ../../bar/files/"),
        ("app-misc/bar/bar-1.ebuild", "bar 1"),
        ("app-misc/bar/files/p.patch", "patch"),
        ("app-misc/broken", "-> nothing"),
        ("app-misc/loop", "-> loop"),
        ("app-misc/loopa", "-> loopb"),
        ("app-misc/loopb", "-> loopa"),
        ("cross-x/foo", "-> ../app-misc/foo"),
        ("eclass/e.eclass", "e 1"),
        ("eclass/f.eclass", "-> e.eclass"),
        ("eclass/g.eclass", "-> ./f.eclass"),
    ];

    /// `LINK_SHAPES` written to a real directory, links as links
    fn link_shapes_on_disk(root: &Utf8Path) {
        for (path, text) in LINK_SHAPES {
            let file = root.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            match text.strip_prefix("-> ") {
                Some(target) => std::os::unix::fs::symlink(target, &file).unwrap(),
                None => std::fs::write(&file, text).unwrap(),
            }
        }
    }

    /// What a tree says about `path`: its text, its kind and its children, errors as `Err`
    fn probe(tree: &dyn TreeSource, path: &str) -> String {
        let path = Utf8Path::new(path);
        let read = match tree.read(path) {
            Ok(Some(bytes)) => format!("{:?}", String::from_utf8(bytes).unwrap()),
            Ok(None) => "None".into(),
            Err(_) => "Err".into(),
        };
        let kind = match tree.kind(path) {
            Ok(kind) => format!("{kind:?}"),
            Err(_) => "Err".into(),
        };
        let list = match tree.list(path) {
            Ok(Some(mut names)) => {
                names.sort_by(|a, b| a.0.cmp(&b.0));
                format!("{names:?}")
            }
            Ok(None) => "None".into(),
            Err(_) => "Err".into(),
        };
        format!("{path}: read {read}, kind {kind}, list {list}")
    }

    // Every link that stays inside the tree answers as the checkout's would: through a linked
    // directory, `..` is the physical parent, as the kernel resolves it.
    #[test]
    fn redb_links_answer_as_the_checkout_does() {
        let (dir, location) = location();
        TreeWriter::import(&location, update(None, &fixture(LINK_SHAPES), [1; 20])).unwrap();
        let checkout = Utf8Path::from_path(dir.path()).unwrap().join("checkout");
        link_shapes_on_disk(&checkout);
        let fs = crate::tree::FsTree::new(&checkout);
        let storage = super::super::storage_at(&location).unwrap().unwrap();
        let redb = &*storage.tree;
        for path in [
            "",
            "app-misc",
            "app-misc/foo",
            "app-misc/foo/foo-2.ebuild",
            "app-misc/foo/files",
            "app-misc/foo/files/p.patch",
            "app-misc/foo/files/chain",
            "app-misc/foo/files/dirlink",
            "app-misc/foo/files/dirlink/p.patch",
            "app-misc/foo/files/dirlink/../bar-1.ebuild",
            "app-misc/broken",
            "app-misc/loop",
            "app-misc/loopa",
            "cross-x",
            "cross-x/foo",
            "cross-x/foo/foo-2.ebuild",
            "cross-x/foo/files",
            "cross-x/foo/files/p.patch",
            "cross-x/foo/files/chain",
            "cross-x/foo/files/dirlink/p.patch",
            "eclass",
            "eclass/f.eclass",
            "eclass/g.eclass",
            "eclass/f.eclass/x",
        ] {
            assert_eq!(probe(redb, path), probe(&fs, path));
        }
    }

    // A package whose files/ links back to itself builds from a checkout; its copy keeps those
    // links as links rather than nesting copies until the depth limit fails. An empty link target
    // names nothing, as `symlink(2)` would refuse it, and links leaving the tree stay missing.
    #[test]
    fn a_copy_keeps_links_back_into_itself_and_empty_targets_name_nothing() {
        let (dir, location) = location();
        let files = [
            ("app-misc/foo/foo-1.ebuild", "foo 1"),
            ("app-misc/foo/files/self", "-> ."),
            ("app-misc/foo/files/up", "-> .."),
            ("app-misc/foo/files/p.patch", "patch"),
            ("app-misc/foo/files/empty", "-> "),
            ("app-misc/foo/files/abs", "-> /etc/hostname"),
            ("app-misc/foo/files/escape", "-> ../../../../outside"),
            ("cross-x/foo", "-> ../app-misc/foo"),
        ];
        TreeWriter::import(&location, update(None, &fixture(&files), [1; 20])).unwrap();
        let storage = super::super::storage_at(&location).unwrap().unwrap();
        let tree = &*storage.tree;
        assert_eq!(
            read(tree, "app-misc/foo/files/self/p.patch").unwrap(),
            "patch"
        );
        assert_eq!(
            read(tree, "app-misc/foo/files/up/foo-1.ebuild").unwrap(),
            "foo 1"
        );
        for missing in ["empty", "empty/p.patch", "abs", "escape"] {
            let path = format!("app-misc/foo/files/{missing}");
            assert_eq!(
                probe(tree, &path),
                format!("{path}: read None, kind None, list None")
            );
        }
        let mut names = tree
            .list(Utf8Path::new("app-misc/foo/files"))
            .unwrap()
            .unwrap();
        names.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            names,
            [
                ("p.patch".into(), EntryKind::File),
                ("self".into(), EntryKind::Dir),
                ("up".into(), EntryKind::Dir)
            ]
        );

        let copies = Utf8Path::from_path(dir.path()).unwrap().join("copies");
        for pkg in ["app-misc/foo", "cross-x/foo"] {
            let root = tree.checkout(&[Utf8Path::new(pkg)], Some(&copies)).unwrap();
            let files = root.join(pkg).join("files");
            for (link, target) in [("self", "."), ("up", "..")] {
                let link = files.join(link);
                assert!(link.symlink_metadata().unwrap().is_symlink(), "{link}");
                assert_eq!(std::fs::read_link(&link).unwrap().to_str(), Some(target));
            }
            assert_eq!(
                std::fs::read_to_string(files.join("self/up/foo-1.ebuild")).unwrap(),
                "foo 1"
            );
            assert_eq!(
                std::fs::read_to_string(files.join("p.patch")).unwrap(),
                "patch"
            );
            for missing in ["empty", "abs", "escape"] {
                assert!(files.join(missing).symlink_metadata().is_err(), "{missing}");
            }
        }
    }
}
