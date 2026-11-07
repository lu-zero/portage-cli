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
    CACHE_ENTRIES, CACHE_SCHEMA, EBUILDS, ECLASSES, MD5_CACHE_DIR, META, OBJECTS, ObjectKind, Oid,
    PACKAGES, PATHS, REFCOUNT, SYNC_LOG, TREE_SCHEMA, check_schema, db_err, decode, encode,
    md5_cache_key,
};

/// How many sync records the tree database keeps
const KEEP_LOG: u64 = 50;

const MODE_TREE: u16 = 0o040000;
/// A submodule: names a commit of another repository, never stored here
const MODE_GITLINK: u16 = 0o160000;

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
        let db = Database::open(&file).map_err(|e| db_err(format!("{file}: {e}")))?;
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
    pub fn import(location: &Utf8Path, update: TreeUpdate) -> io::Result<UpdateSummary> {
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
        self.update_cache(&update, &cache_changes, &mut summary)?;
        Ok(summary)
    }

    /// Bring the md5-cache database to `update.commit`: only the changed entries when it was at
    /// `update.from`, otherwise every entry copied from the tree
    fn update_cache(
        &self,
        update: &TreeUpdate,
        changes: &CacheChanges,
        summary: &mut UpdateSummary,
    ) -> io::Result<()> {
        let shown = |e: io::Error| db_err(format!("{}: {e}", self.cache_file));
        let mut cache = Database::create(&self.cache_file).map_err(|e| shown(db_err(e)))?;
        let at = {
            let txn = cache.begin_read().map_err(db_err)?;
            match txn.open_table(META) {
                Ok(meta) => meta
                    .get("tree_commit")
                    .map_err(db_err)?
                    .map(|v| String::from_utf8_lossy(v.value()).into_owned()),
                Err(TableError::TableDoesNotExist(_)) => None,
                Err(e) => return Err(db_err(e)),
            }
        };
        let incremental = update.from.is_some() && at == update.from.map(hex::encode);
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
                if let Some(key) = md5_cache_key(path.value()) {
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
            meta.insert("tree_commit", hex::encode(update.commit).as_bytes())
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
        .filter_map(|c| c.new.map(|i| i.oid))
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
    let mut touched: BTreeMap<String, [BTreeSet<Cpv>; 3]> = BTreeMap::new();
    {
        let mut objects = txn.open_table(OBJECTS).map_err(db_err)?;
        let mut refcount = txn.open_table(REFCOUNT).map_err(db_err)?;
        let mut paths = txn.open_table(PATHS).map_err(db_err)?;
        let mut ebuilds = txn.open_table(EBUILDS).map_err(db_err)?;
        let mut eclasses = txn.open_table(ECLASSES).map_err(db_err)?;

        // The root tree and the commit hold one reference each.
        let mut incs = vec![update.tree, update.commit];
        let mut decs = Vec::new();
        if let (Some(from), Some(old_tree)) = (update.from, stored_tree(txn)?) {
            decs.extend([old_tree, from]);
        }
        // Applied in key order afterwards: git tree order is not the byte order of full paths.
        let mut path_rows: BTreeMap<&str, Option<[u8; 22]>> = BTreeMap::new();
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
            match derived(&change.path) {
                Derived::CacheEntry(key) => {
                    cache_changes.push((key.to_string(), new.map(|i| i.oid)));
                }
                // Read before the object table changes, so a blob that only moved is still found.
                Derived::Eclass(name) => match new {
                    Some(item) => {
                        let digest = format!("{:x}", blob_digest(prepared, &objects, &item.oid)?);
                        eclasses.insert(name, digest.as_bytes()).map_err(db_err)?;
                    }
                    None => {
                        eclasses.remove(name).map_err(db_err)?;
                    }
                },
                Derived::Ebuild(cpv) => {
                    let key = ebuild_key(&cpv);
                    let slot = match new {
                        Some(item) => {
                            let digest = blob_digest(prepared, &objects, &item.oid)?;
                            ebuilds
                                .insert(key.as_str(), &digest.0[..])
                                .map_err(db_err)?;
                            if old.is_some() { MODIFIED } else { ADDED }
                        }
                        None => {
                            ebuilds.remove(key.as_str()).map_err(db_err)?;
                            REMOVED
                        }
                    };
                    let cpn = format!("{}/{}", cpv.cpn.category, cpv.cpn.package);
                    touched.entry(cpn).or_default()[slot].insert(cpv);
                }
                Derived::Other => {}
            }
        }
        for (path, row) in &path_rows {
            match row {
                Some(row) => paths.insert(*path, &row[..]).map(drop),
                None => paths.remove(*path).map(drop),
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
        summary.versions_added.extend(added.iter().cloned());
        summary.versions_removed.extend(removed.iter().cloned());
        summary.versions_modified.extend(modified.iter().cloned());
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
    use crate::tree::TreeSource;

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
                        let item = TreeItem {
                            mode: 0o100644,
                            oid: oid(ObjectKind::Blob, text.as_bytes()),
                        };
                        out.objects
                            .insert(item.oid, (ObjectKind::Blob, text.as_bytes().to_vec()));
                        out.entries.insert(format!("{prefix}{path}"), item);
                        body.extend_from_slice(format!("100644 {path}\0").as_bytes());
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
        let storage = super::super::storage_at(location).unwrap();
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

        let storage = super::super::storage_at(&location).unwrap();
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

        let storage = super::super::storage_at(&location).unwrap();
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
}
