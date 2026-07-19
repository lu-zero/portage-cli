//! Regenerate the binhost `Packages` index from `PKGDIR`'s containers —
//! `em maint binhost` (and `em maint binpkg prune`, which deletes stale
//! containers first and then calls this to bring the index back in sync).
//!
//! Walks `PKGDIR` for `*.gpkg.tar`, reads each container's VDB-style metadata
//! via [`crate::read_metadata`], and writes the `Packages` index
//! portage/emerge consume over `PORTAGE_BINHOST` / `--getbinpkg`. The format is
//! portage's `binarytree._populate_local` / `PackageIndex.write`:
//!
//! - a **header** block (sorted `KEY: VALUE` lines, blank-line separated), then
//! - one block per package (sorted by CPV, `KEY: VALUE` for non-empty values,
//!   blank line between entries).
//!
//! Per-package entry = the gpkg's metadata (with `DESCRIPTION`→`DESC` and
//! `repository`→`REPO` translations) plus the container file's `MD5`/`SHA1`
//! checksums, `SIZE`, `MTIME`, relative `PATH`, `CPV` and `BUILD_ID`.
//!
//! Header profile-derived keys (`USE`, `USE_EXPAND`, `ACCEPT_KEYWORDS`, …) and
//! USE-pre-evaluated dep strings are intentionally minimal here: portage
//! evaluates dep conditionals on read using each entry's `USE`, and a sparse
//! header is valid (its `_pkgindex_version_supported` only requires `VERSION`).
//! `REPO_REVISIONS` (the repo git-revision-at-build) is omitted — em does not
//! yet track sync history at build time.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use camino::Utf8Path;

use crate::error::Result;
use crate::scan::{checksum, find_gpkg_containers, parse_build_id_from_name};

/// Walk `pkgdir` for `*.gpkg.tar`, build per-package entries, and write the
/// `Packages` index. Returns `(indexed, skipped)`.
pub fn index_pkgdir(pkgdir: &Utf8Path, chost: &str) -> Result<(usize, usize)> {
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    find_gpkg_containers(pkgdir.as_std_path(), pkgdir.as_std_path(), &mut files)?;
    files.sort();

    let mut header = BTreeMap::new();
    header.insert("VERSION".to_string(), "0".to_string());
    header.insert("CHOST".to_string(), chost.to_string());
    header.insert("repository".to_string(), String::new());

    let mut entries: Vec<(String, BTreeMap<String, String>)> = Vec::new();
    let mut skipped = 0usize;
    for (rel, full) in &files {
        match build_entry(rel, full) {
            Ok((cpv, fields)) => entries.push((cpv, fields)),
            Err(e) => {
                eprintln!("warning: skipping {}: {e:#}", full.display());
                skipped += 1;
            }
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    write_index(pkgdir, &header, &entries)?;
    Ok((entries.len(), skipped))
}

/// Build one index entry from a container: metadata fields + file checksums.
///
/// Returns `(cpv, fields)` where `fields` keys are already in their
/// index-translated form (`DESC`, `REPO`).
fn build_entry(rel: &str, full: &Path) -> Result<(String, BTreeMap<String, String>)> {
    let meta = crate::read_metadata(full)?;

    let cat = meta.get("CATEGORY").map(String::as_str).unwrap_or("");
    let pf = meta.get("PF").map(String::as_str).unwrap_or("");
    if cat.is_empty() || pf.is_empty() {
        return Err(crate::error::Error::Corrupt(
            "missing CATEGORY/PF in metadata".to_string(),
        ));
    }
    let cpv = format!("{cat}/{pf}");

    let (md5, sha1, size, mtime) = checksum(full)?;

    let mut f = BTreeMap::new();
    f.insert("CPV".to_string(), cpv.clone());
    f.insert("MD5".to_string(), md5);
    f.insert("SHA1".to_string(), sha1);
    f.insert("SIZE".to_string(), size.to_string());
    f.insert("MTIME".to_string(), mtime.to_string());
    f.insert("PATH".to_string(), rel.to_string());

    if let Some(bid) = meta.get("BUILD_ID") {
        f.insert("BUILD_ID".to_string(), bid.clone());
    } else if let Some(bid) = parse_build_id_from_name(rel) {
        f.insert("BUILD_ID".to_string(), bid.to_string());
    }

    // Identity metadata fields (skip empty); the two portage translations are
    // applied so the file matches portage's own index.
    copy_field(&meta, &mut f, "BUILD_TIME");
    copy_field(&meta, &mut f, "SLOT");
    copy_field(&meta, &mut f, "EAPI");
    copy_field(&meta, &mut f, "USE");
    copy_field(&meta, &mut f, "IUSE");
    copy_field(&meta, &mut f, "KEYWORDS");
    copy_field(&meta, &mut f, "LICENSE");
    copy_field(&meta, &mut f, "RESTRICT");
    copy_field(&meta, &mut f, "PROPERTIES");
    copy_field(&meta, &mut f, "DEFINED_PHASES");
    copy_field(&meta, &mut f, "DEPEND");
    copy_field(&meta, &mut f, "RDEPEND");
    copy_field(&meta, &mut f, "BDEPEND");
    copy_field(&meta, &mut f, "PDEPEND");
    copy_field(&meta, &mut f, "IDEPEND");
    copy_field(&meta, &mut f, "CHOST");
    // Build-env provenance (em extension). Portage's binarytree drops unknown
    // per-package keys on read, but writing them is harmless and lets em gate
    // reuse later (e.g. RVV / `-march` via CFLAGS). Sourced from the GPKG's
    // VDB-shaped metadata (`MergeSpec` already records these on merge).
    copy_field(&meta, &mut f, "CFLAGS");
    copy_field(&meta, &mut f, "CXXFLAGS");
    copy_field(&meta, &mut f, "LDFLAGS");
    copy_field(&meta, &mut f, "RUSTFLAGS");
    copy_field(&meta, &mut f, "CBUILD");
    copy_field(&meta, &mut f, "PROVIDES");
    copy_field(&meta, &mut f, "REQUIRES");

    // Translated fields (metadata name → index name).
    if let Some(v) = meta.get("DESCRIPTION").filter(|s| !s.is_empty()) {
        f.insert("DESC".to_string(), v.clone());
    }
    if let Some(v) = meta.get("repository").filter(|s| !s.is_empty()) {
        f.insert("REPO".to_string(), v.clone());
    }

    Ok((cpv, f))
}

fn copy_field(meta: &BTreeMap<String, String>, out: &mut BTreeMap<String, String>, key: &str) {
    if let Some(v) = meta.get(key).filter(|s| !s.is_empty()) {
        out.insert(key.to_string(), v.clone());
    }
}

/// Write the `Packages` index: header block, then one block per entry.
fn write_index(
    pkgdir: &Utf8Path,
    header: &BTreeMap<String, String>,
    entries: &[(String, BTreeMap<String, String>)],
) -> Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut text = String::new();
    for (k, v) in header {
        if v.is_empty() {
            continue;
        }
        text.push_str(k);
        text.push_str(": ");
        text.push_str(v);
        text.push('\n');
    }
    text.push_str(&format!("PACKAGES: {}\n", entries.len()));
    text.push_str(&format!("TIMESTAMP: {now}\n"));
    text.push('\n');

    for (_cpv, fields) in entries {
        for (k, v) in fields {
            if v.is_empty() {
                continue;
            }
            text.push_str(k);
            text.push_str(": ");
            text.push_str(v);
            text.push('\n');
        }
        text.push('\n');
    }

    std::fs::write(pkgdir.join("Packages"), text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::write_gpkg;

    /// Seed a PKGDIR with a gpkg via the writer, then build the index and
    /// verify the `Packages` file has the expected header + one sorted entry
    /// with checksums and the translated DESC/REPO keys.
    #[test]
    fn index_roundtrips_a_written_gpkg() {
        let tmp = tempfile::tempdir().unwrap();
        let pkgdir = camino::Utf8Path::from_path(tmp.path()).unwrap();

        let image = tmp.path().join("image");
        std::fs::create_dir_all(image.join("usr/bin")).unwrap();
        std::fs::write(image.join("usr/bin/hello"), b"hi\n").unwrap();

        let meta = tmp.path().join("vdb/foo-1.0");
        std::fs::create_dir_all(&meta).unwrap();
        for (k, v) in [
            ("PF", "foo-1.0"),
            ("CATEGORY", "app-test"),
            ("SLOT", "0"),
            ("EAPI", "8"),
            ("DESCRIPTION", "a test package"),
            ("repository", "gentoo"),
            ("BUILD_TIME", "1700000000"),
            ("SIZE", "3"),
            ("CFLAGS", "-O2 -pipe -mcpu=ampere1a"),
            ("CXXFLAGS", "-O2 -pipe -mcpu=ampere1a"),
            ("RUSTFLAGS", "-C target-cpu=neoverse-n1"),
        ] {
            std::fs::write(meta.join(k), format!("{v}\n")).unwrap();
        }

        let container = pkgdir.join("app-test/foo-1.0-1.gpkg.tar");
        std::fs::create_dir_all(container.parent().unwrap()).unwrap();
        write_gpkg(
            &crate::GpkgInput {
                image_dir: &image,
                metadata_dir: &meta,
                basename: "foo-1.0",
            },
            container.as_std_path(),
        )
        .unwrap();

        let (count, skipped) = index_pkgdir(pkgdir, "aarch64-unknown-linux-gnu").unwrap();
        assert_eq!(count, 1);
        assert_eq!(skipped, 0);

        let idx = std::fs::read_to_string(pkgdir.join("Packages")).unwrap();
        assert!(idx.contains("VERSION: 0"));
        assert!(idx.contains("CHOST: aarch64-unknown-linux-gnu"));
        assert!(idx.contains("PACKAGES: 1"));
        // Header sorts before entries; CHOST < VERSION alphabetically, so the
        // file leads with CHOST (portage sorts header keys the same way).
        assert!(idx.starts_with("CHOST: aarch64-unknown-linux-gnu\n"));

        assert!(idx.contains("CPV: app-test/foo-1.0"));
        assert!(idx.contains("PATH: app-test/foo-1.0-1.gpkg.tar"));
        assert!(idx.contains("BUILD_ID: 1"));
        assert!(idx.contains("DESC: a test package"));
        assert!(idx.contains("REPO: gentoo"));
        assert!(idx.contains("CFLAGS: -O2 -pipe -mcpu=ampere1a"));
        assert!(idx.contains("CXXFLAGS: -O2 -pipe -mcpu=ampere1a"));
        assert!(idx.contains("RUSTFLAGS: -C target-cpu=neoverse-n1"));
        assert!(idx.contains("MD5: "));
        assert!(idx.contains("SHA1: "));
        assert!(idx.contains("SIZE: "));
        assert!(idx.contains("MTIME: "));

        // Parser surface: provenance available without affecting reuse.
        let parsed = crate::BinpkgIndex::open(pkgdir.as_std_path()).unwrap();
        let e = parsed.get("app-test/foo-1.0").unwrap();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].cflags, "-O2 -pipe -mcpu=ampere1a");
        assert_eq!(e[0].cxxflags, "-O2 -pipe -mcpu=ampere1a");
        assert_eq!(e[0].rustflags, "-C target-cpu=neoverse-n1");
        // Proves the key now differs from a no-RUSTFLAGS build of the same
        // CFLAGS/CXXFLAGS (the consumer-side hash math itself is already
        // covered by `build_env_key_*` in index.rs).
        assert!(!e[0].build_env_key.is_empty());
        assert_ne!(
            e[0].build_env_key,
            crate::index::build_env_key(
                "-O2 -pipe -mcpu=ampere1a",
                "-O2 -pipe -mcpu=ampere1a",
                "",
                ""
            )
        );
    }
}
