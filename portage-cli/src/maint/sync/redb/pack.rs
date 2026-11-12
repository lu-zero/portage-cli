//! A received pack completed, indexed and resolved in memory
//!
//! Thin deltas are completed with bases from the database, the completed pack is indexed into a
//! `Vec` and every object is decoded once; gix hashes each one, so a corrupt pack fails here.

use std::collections::HashMap;
use std::io;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Result, anyhow, bail};
use gix::odb::pack::{self, data};
use portage_repo::tree::redb::{ObjectKind, Oid};

/// Every object of a pack, by id
pub(super) type Objects = HashMap<Oid, (ObjectKind, Vec<u8>)>;

/// Every holder leaves the data consistent between calls, so a panicked one poisons nothing
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The rewritten pack, shared with the indexer that reads it back
struct SharedVec(Arc<Mutex<io::Cursor<Vec<u8>>>>);

impl io::Write for SharedVec {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        lock(&self.0).write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl io::Read for SharedVec {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        lock(&self.0).read(buf)
    }
}

impl io::Seek for SharedVec {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        lock(&self.0).seek(pos)
    }
}

fn slice_entry(range: data::EntryRange, d: &Arc<Vec<u8>>) -> Option<&[u8]> {
    d.get(range.start as usize..range.end as usize)
}

pub(super) fn kind(kind: gix::objs::Kind) -> ObjectKind {
    match kind {
        gix::objs::Kind::Commit => ObjectKind::Commit,
        gix::objs::Kind::Tree => ObjectKind::Tree,
        gix::objs::Kind::Blob => ObjectKind::Blob,
        gix::objs::Kind::Tag => ObjectKind::Tag,
    }
}

/// Every object of `pack`, its thin deltas completed from `bases`
pub(super) fn resolve(pack: &[u8], bases: impl gix::objs::Find) -> Result<Objects> {
    let hash = gix::hash::Kind::Sha1;
    let interrupt = AtomicBool::new(false);
    if pack.len() < 12 {
        bail!("a pack of {} bytes has no header", pack.len());
    }
    let entries = data::input::BytesToEntriesIter::new_from_header(
        io::BufReader::new(pack),
        data::input::Mode::Verify,
        data::input::EntryDataMode::KeepAndCrc32,
        hash,
    )
    .map_err(|e| anyhow!("reading the pack header: {e}"))?;
    let version = entries.version();
    let completed_entries = data::input::LookupRefDeltaObjectsIter::new(
        entries,
        bases,
        gix::zlib::Compression::BEST_SPEED,
    );
    let completed = Arc::new(Mutex::new(io::Cursor::new(Vec::with_capacity(
        pack.len() + 1024,
    ))));
    let mut rewritten = data::input::EntriesToBytesIter::new(
        completed_entries,
        SharedVec(Arc::clone(&completed)),
        version,
        hash,
    );
    let mut index = Vec::new();
    let kept: Arc<Mutex<Option<Arc<Vec<u8>>>>> = Arc::new(Mutex::new(None));
    let (for_resolver, kept_in) = (Arc::clone(&completed), Arc::clone(&kept));
    let outcome = pack::index::write_data_iter_to_stream(
        pack::index::Version::V2,
        move || {
            let data = Arc::new(std::mem::take(lock(&for_resolver).get_mut()));
            *lock(&kept_in) = Some(Arc::clone(&data));
            Ok((
                slice_entry as fn(data::EntryRange, &Arc<Vec<u8>>) -> Option<&[u8]>,
                data,
            ))
        },
        &mut rewritten,
        None,
        &mut gix::progress::Discard,
        &mut index,
        &interrupt,
        hash,
        None,
        version,
    )
    .map_err(|e| anyhow!("indexing the pack: {e}"))?;
    drop(rewritten);

    let bytes = lock(&kept)
        .take()
        .ok_or_else(|| anyhow!("the indexer kept no pack"))?;
    let bytes = Arc::try_unwrap(bytes).map_err(|_| anyhow!("the pack is still shared"))?;
    let index = pack::index::File::from_data(index, "mem.idx".into(), hash)
        .map_err(|e| anyhow!("loading the pack index: {e}"))?;
    let data = data::File::from_data(bytes, "mem.pack".into(), hash)
        .map_err(|e| anyhow!("loading the pack: {e}"))?;
    let objects: Mutex<Objects> = Mutex::new(HashMap::with_capacity(outcome.num_objects as usize));
    // `traverse_with_index` reopens the pack by path; `traverse_with_lookup` works on the `Vec`.
    index
        .traverse_with_lookup(
            |kind_: gix::objs::Kind,
             body: &[u8],
             entry: &pack::index::Entry,
             _: &dyn gix::progress::Progress| {
                let mut oid = [0u8; 20];
                oid.copy_from_slice(entry.oid.as_bytes());
                lock(&objects).insert(oid, (kind(kind_), body.to_vec()));
                Ok(())
            },
            &data,
            &mut gix::progress::Discard,
            &interrupt,
            pack::index::traverse::with_lookup::Options {
                check: pack::index::traverse::SafetyCheck::All,
                thread_limit: None,
                make_pack_lookup_cache: || {
                    pack::cache::lru::MemoryCappedHashmap::new(64 * 1024 * 1024)
                },
            },
        )
        .map_err(|e| anyhow!("resolving the pack: {e}"))?;
    Ok(objects
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner))
}
