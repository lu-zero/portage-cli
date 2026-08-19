//! Walking `PKGDIR` for `*.gpkg.tar` containers, and the per-container
//! checksums/build-id parsing the index and maintenance operations need.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use sha1::Digest as _;
use sha1::Sha1;

use crate::error::Result;

/// Recursively enumerate `*.gpkg.tar` container files under `root`.
///
/// Returns `(rel_path, full)` pairs.
pub fn find_gpkg_containers(
    dir: &Path,
    root: &Path,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        if ft.is_dir() {
            find_gpkg_containers(&entry.path(), root, out)?;
        } else if ft.is_file() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".gpkg.tar") {
                let full = entry.path();
                // `root` is always an ancestor of `full` here (`entry.path()`
                // is built by walking down from it), so this can't fail.
                let rel = full
                    .strip_prefix(root)
                    .expect("full is always under root")
                    .to_string_lossy()
                    .into_owned();
                out.push((rel, full));
            }
        }
    }
    Ok(())
}

/// Container file MD5+SHA1 (lowercase hex), byte size, and mtime (unix secs).
///
/// Shared by index regeneration (`build_entry`) and `em maint binpkg
/// verify`, which recomputes these to compare against the index's recorded
/// values.
pub fn checksum(path: &Path) -> Result<(String, String, u64, u64)> {
    let mut file = std::fs::File::open(path)?;
    let mut md5 = md5::Context::new();
    let mut sha1 = Sha1::new();
    let mut buf = [0u8; 65536];
    let mut size = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        md5.write_all(&buf[..n])?;
        sha1.update(&buf[..n]);
        size += n as u64;
    }
    let md5 = format!("{:x}", md5.finalize());
    let sha1 = hex::encode(sha1.finalize());

    let mtime = std::fs::metadata(path)?
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    Ok((md5, sha1, size, mtime))
}

/// Parse the trailing `-<n>` build-id from a container basename, portage's
/// `<PF>-<BUILD_ID>.gpkg.tar` layout.
///
/// `None` for the single-instance `<PF>.gpkg.tar` form.
pub fn parse_build_id_from_name(rel: &str) -> Option<u32> {
    let base = std::path::Path::new(rel).file_name()?.to_str()?;
    let stem = base.strip_suffix(".gpkg.tar")?;
    let (rest, id) = stem.rsplit_once('-')?;
    if rest.is_empty() {
        return None;
    }
    id.parse::<u32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_build_id_from_name_reads_the_trailing_number() {
        assert_eq!(
            parse_build_id_from_name("app-test/foo-1.0-3.gpkg.tar"),
            Some(3)
        );
    }

    #[test]
    fn parse_build_id_from_name_none_for_single_instance() {
        assert_eq!(parse_build_id_from_name("app-test/foo-1.0.gpkg.tar"), None);
    }
}
