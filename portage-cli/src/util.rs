//! Small filesystem helpers shared across applets.

use std::io::Write;

use anyhow::{Context, Result};
use camino::Utf8Path;
use tempfile::NamedTempFile;

/// Look up a numeric id by name in the contents of a colon-separated
/// `passwd`/`group` database. Both put the id in field 2 (`name:pw:uid:…`,
/// `name:pw:gid:…`), so one parser serves both.
///
/// Text in, so the caller decides which database and stays testable. Note this
/// bypasses NSS — fine for the local system accounts it is asked about, and
/// every caller has a defined fallback when a name does not resolve.
pub(crate) fn id_by_name_in(db: &str, name: &str) -> Option<u32> {
    db.lines().find_map(|line| {
        let mut cols = line.split(':');
        (cols.next() == Some(name))
            .then(|| cols.nth(1))
            .flatten()
            .and_then(|id| id.parse().ok())
    })
}

/// Write `contents` to `path` only if it does not already exist (idempotent
/// scaffolding for `em setup` / `em crossdev`, which must not clobber a file the
/// user or a previous run wrote).
pub(crate) fn write_if_absent(path: &Utf8Path, contents: &str) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    std::fs::write(path, contents).with_context(|| format!("writing {path}"))
}

/// Atomically replace `path` with `contents`.
///
/// Creates a temporary file in the same directory as `path` (so rename stays
/// on one filesystem), writes `contents`, then [`NamedTempFile::persist`]s it
/// over the destination. A crash mid-write cannot leave a truncated final
/// file; a failed attempt cleans up the temp on drop.
///
/// Creates the parent directory if it does not exist.
pub(crate) fn write_atomic(path: &Utf8Path, contents: impl AsRef<[u8]>) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_str().is_empty())
        .unwrap_or_else(|| Utf8Path::new("."));
    std::fs::create_dir_all(parent).with_context(|| format!("creating {parent}"))?;
    let mut tmp = NamedTempFile::new_in(parent.as_std_path())
        .with_context(|| format!("tempfile in {parent}"))?;
    tmp.write_all(contents.as_ref())
        .with_context(|| format!("writing temp for {path}"))?;
    tmp.persist(path.as_std_path())
        .map_err(|e| e.error)
        .with_context(|| format!("persisting {path}"))?;
    Ok(())
}
