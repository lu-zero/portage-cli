//! Walking `PKGDIR` for `*.gpkg.tar` containers, and the per-container
//! checksums/build-id parsing the index and maintenance operations need.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use sha1::Digest as _;
use sha1::Sha1;

use crate::error::{Error, Result};

/// Recursively enumerate `*.gpkg.tar` container files under `root`
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

/// Container file MD5+SHA1 (lowercase hex), byte size, and mtime (unix secs)
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

/// Build id only when the basename is `{pf}-{id}.gpkg.tar`.
///
/// A version that is itself an integer (`awk-1`, `linux-firmware-20250101`)
/// must stay the single-instance form. Splitting on the last `-` treats that
/// integer as a build id and can make prune delete the real rebuild.
fn build_id_after_pf(rel: &str, pf: &str) -> Option<u32> {
    let base = std::path::Path::new(rel).file_name()?.to_str()?;
    let stem = base.strip_suffix(".gpkg.tar")?;
    let rest = stem.strip_prefix(pf)?.strip_prefix('-')?;
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
}

/// Parse the trailing `-<n>` build-id from a container basename, portage's
/// `<PF>-<BUILD_ID>.gpkg.tar` layout.
///
/// `None` for the single-instance `<PF>.gpkg.tar` form. This splits on the
/// last `-` only; callers that know `PF` should not use it for versions whose
/// last component is an integer.
pub fn parse_build_id_from_name(rel: &str) -> Option<u32> {
    let base = std::path::Path::new(rel).file_name()?.to_str()?;
    let stem = base.strip_suffix(".gpkg.tar")?;
    let (rest, id) = stem.rsplit_once('-')?;
    if rest.is_empty() {
        return None;
    }
    id.parse::<u32>().ok()
}

/// Identity and build provenance shared by everything that reads one container
///
/// The one place `CATEGORY`/`PF`, the recorded flag sets, and `BUILD_ID` are
/// resolved, so the index scan, the index writer, and `maint binpkg prune`
/// cannot drift apart on precedence.
#[derive(Debug, Clone)]
pub(crate) struct ContainerFacts {
    /// `<CATEGORY>/<PF>` — the container's CPV
    pub cpv: String,
    /// Recorded `CHOST` (empty when absent)
    pub chost: String,
    /// Recorded build-time `CFLAGS` (empty when absent)
    pub cflags: String,
    /// Recorded build-time `CXXFLAGS` (empty when absent)
    pub cxxflags: String,
    /// Recorded build-time `LDFLAGS` (empty when absent)
    pub ldflags: String,
    /// Recorded build-time `RUSTFLAGS` (empty when absent)
    pub rustflags: String,
    /// `BUILD_ID`: the metadata field, else the filename suffix, else absent
    /// for the implicit single-instance form
    pub build_id: Option<u32>,
}

impl ContainerFacts {
    /// Read the facts out of one container's [`crate::read_metadata`] map
    ///
    /// `rel` is the PKGDIR-relative container path, read only for the
    /// `<PF>-<BUILD_ID>.gpkg.tar` fallback. A recorded `BUILD_ID` that is not a
    /// number is not representable in the index, so the filename is consulted
    /// instead.
    pub(crate) fn from_metadata(meta: &BTreeMap<String, String>, rel: &str) -> Result<Self> {
        let cat = meta.get("CATEGORY").map(String::as_str).unwrap_or("");
        let pf = meta.get("PF").map(String::as_str).unwrap_or("");
        if cat.is_empty() || pf.is_empty() {
            return Err(Error::Corrupt(
                "missing CATEGORY/PF in metadata".to_string(),
            ));
        }
        let field = |key: &str| meta.get(key).cloned().unwrap_or_default();
        Ok(Self {
            cpv: format!("{cat}/{pf}"),
            chost: field("CHOST"),
            cflags: field("CFLAGS"),
            cxxflags: field("CXXFLAGS"),
            ldflags: field("LDFLAGS"),
            rustflags: field("RUSTFLAGS"),
            build_id: meta
                .get("BUILD_ID")
                .and_then(|s| s.parse().ok())
                .or_else(|| build_id_after_pf(rel, pf)),
        })
    }

    /// The build-environment key over the recorded flag sets
    pub(crate) fn build_env_key(&self) -> String {
        crate::index::build_env_key(&self.cflags, &self.cxxflags, &self.ldflags, &self.rustflags)
    }

    /// The build id, with the implicit single-instance form as `0`
    ///
    /// Sorts below any explicit build id, so a single-instance container is
    /// always pruned in favor of a numbered one sharing the same cpv.
    pub(crate) fn build_id_or_zero(&self) -> u32 {
        self.build_id.unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

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

    #[test]
    fn facts_take_the_build_id_from_metadata_before_the_filename() {
        let facts = ContainerFacts::from_metadata(
            &meta(&[
                ("CATEGORY", "app-test"),
                ("PF", "foo-1.0"),
                ("BUILD_ID", "2"),
            ]),
            "app-test/foo-1.0-7.gpkg.tar",
        )
        .unwrap();
        assert_eq!(facts.cpv, "app-test/foo-1.0");
        assert_eq!(facts.build_id, Some(2));
    }

    // A container written before `BUILD_ID` reached the GPKG metadata (or by a
    // hand-built one) still records its instance in the filename — the index
    // scan has to see it too, or it picks the oldest instance for reuse.
    #[test]
    fn facts_fall_back_to_the_filename_build_id() {
        let facts = ContainerFacts::from_metadata(
            &meta(&[("CATEGORY", "app-test"), ("PF", "foo-1.0")]),
            "app-test/foo-1.0-7.gpkg.tar",
        )
        .unwrap();
        assert_eq!(facts.build_id, Some(7));
        assert_eq!(facts.build_id_or_zero(), 7);
    }

    #[test]
    fn facts_report_no_build_id_for_the_single_instance_form() {
        let facts = ContainerFacts::from_metadata(
            &meta(&[("CATEGORY", "app-test"), ("PF", "foo-1.0")]),
            "app-test/foo-1.0.gpkg.tar",
        )
        .unwrap();
        assert_eq!(facts.build_id, None);
        assert_eq!(facts.build_id_or_zero(), 0);
    }

    #[test]
    fn facts_do_not_treat_an_integer_version_as_a_build_id() {
        for (pf, rel) in [
            ("awk-1", "virtual/awk-1.gpkg.tar"),
            (
                "linux-firmware-20250101",
                "sys-kernel/linux-firmware-20250101.gpkg.tar",
            ),
        ] {
            let facts =
                ContainerFacts::from_metadata(&meta(&[("CATEGORY", "virtual"), ("PF", pf)]), rel)
                    .unwrap();
            assert_eq!(facts.build_id, None, "{rel}");
        }
        let rebuilt = ContainerFacts::from_metadata(
            &meta(&[("CATEGORY", "virtual"), ("PF", "awk-1")]),
            "virtual/awk-1-2.gpkg.tar",
        )
        .unwrap();
        assert_eq!(rebuilt.build_id, Some(2));
    }

    #[test]
    fn facts_reject_metadata_without_a_category_or_pf() {
        let err = ContainerFacts::from_metadata(
            &meta(&[("CATEGORY", "app-test")]),
            "app-test/foo-1.0.gpkg.tar",
        )
        .unwrap_err();
        assert!(matches!(err, Error::Corrupt(_)), "got {err:?}");
    }
}
