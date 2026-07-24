//! `em maint binpkg` — local `PKGDIR` maintenance on top of the `Packages`
//! index/reader substrate: check indexed containers against the files on
//! disk, list what's indexed, and collapse leftover multi-`BUILD_ID`
//! containers for the same cpv down to the newest one.
//!
//! No real-portage `emaint` module covers this ground (its own `emaint
//! binhost` only regenerates the index) — this is an em-only extension,
//! built to a scope this repo can actually verify:
//!
//! - [`verify`]: for each indexed container, recompute size/MD5/SHA1 and
//!   compare against the index's recorded values — the same check
//!   `_emerge/BinpkgVerifier.py` performs before reusing a binpkg (size
//!   first, then digests), just run in bulk instead of per-merge.
//! - [`list_index`]: a plain read of the index (cpv, build-id, size, path).
//! - [`prune`]: em keeps at most one container per cpv in its own reuse
//!   model — this collapses any leftover extra `BUILD_ID`s down to the
//!   newest one and regenerates the index, rather than reimplementing
//!   gentoolkit's `eclean-pkg` (which also prunes by installed-set/age/size
//!   and isn't available to verify parity against).
//!
//! This module returns structured reports and does not print anything
//! itself — formatting/printing is the CLI's job.

use std::collections::BTreeMap;
use std::path::PathBuf;

use camino::{Utf8Path, Utf8PathBuf};

use crate::error::{Error, Result};
use crate::gpg::Keyring;
use crate::gpkg::VerifyPolicy;
use crate::index::parse_index_blocks;
use crate::regen::index_pkgdir;
use crate::scan::{checksum, find_gpkg_containers, parse_build_id_from_name};

/// One `Packages` index entry, with the digest/size fields [`verify`]/
/// [`list_index`] need that [`crate::index::BinpkgEntry`] doesn't carry.
/// Kept separate from `BinpkgEntry` rather than unified: `BinpkgEntry` is on
/// the reuse hot path and deliberately doesn't carry these display-only
/// digest/size fields, and this type doesn't need `BinpkgEntry`'s parsed
/// `USE`/`IUSE` sets.
#[derive(Debug, Clone)]
pub struct IndexRow {
    /// `category/PF`.
    pub cpv: String,
    /// Path relative to `PKGDIR`.
    pub path: String,
    /// Recorded MD5 hex digest, if the index has one.
    pub md5: Option<String>,
    /// Recorded SHA1 hex digest, if the index has one.
    pub sha1: Option<String>,
    /// Recorded container size in bytes, if the index has one.
    pub size: Option<u64>,
    /// Recorded `BUILD_ID`, if the index has one.
    pub build_id: Option<u32>,
    /// Recorded `CHOST` ("" if the index omitted it).
    pub chost: String,
    /// Recorded `CFLAGS` ("" if absent) — display only.
    pub cflags: String,
    /// Derived build-env key (see [`crate::index::build_env_key`]); "" means
    /// unkeyed (no ISA-relevant flags recorded).
    pub build_env_key: String,
}

fn parse_index_rows(text: &str) -> Vec<IndexRow> {
    parse_index_blocks(text)
        .into_iter()
        .map(|fields| IndexRow {
            cpv: fields.get("CPV").copied().unwrap_or("").to_string(),
            path: fields.get("PATH").copied().unwrap_or("").to_string(),
            md5: fields.get("MD5").map(|s| s.to_string()),
            sha1: fields.get("SHA1").map(|s| s.to_string()),
            size: fields.get("SIZE").and_then(|s| s.parse().ok()),
            build_id: fields.get("BUILD_ID").and_then(|s| s.parse().ok()),
            chost: fields.get("CHOST").copied().unwrap_or("").to_string(),
            cflags: fields.get("CFLAGS").copied().unwrap_or("").to_string(),
            build_env_key: crate::index::build_env_key_from_fields(&fields),
        })
        .collect()
}

fn read_index(pkgdir: &Utf8Path) -> Result<Vec<IndexRow>> {
    let index_path = pkgdir.join("Packages");
    if !index_path.is_file() {
        return Err(Error::NoIndex(index_path.into_std_path_buf()));
    }
    let text = std::fs::read_to_string(index_path.as_std_path())?;
    Ok(parse_index_rows(&text))
}

/// Read the local `Packages` index (cpv, build-id, size, CHOST, build-env
/// key, CFLAGS, path per entry).
pub fn list_index(pkgdir: &Utf8Path) -> Result<Vec<IndexRow>> {
    read_index(pkgdir)
}

/// Signature status of a container, checked independently of its
/// index-recorded digest/size — "does the file match the index" and "is
/// its internal Manifest validly signed" are genuinely separate axes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureStatus {
    /// No OpenPGP signature at all, and one was required
    /// (`--require-signature`).
    Unsigned,
    /// Signed, but no verify keyring was given — presence-only, not
    /// cryptographically checked.
    Unverified,
    /// Signed and cryptographically verified against the given keyring.
    Valid,
    /// Signed, but the signature does not verify against the given keyring.
    Invalid,
}

/// One container whose recorded digest/size didn't match the file on disk,
/// whose container file is missing entirely, or whose signature failed a
/// required check.
#[derive(Debug, Clone)]
pub struct VerifyProblem {
    /// `category/PF`.
    pub cpv: String,
    /// Path relative to `PKGDIR`.
    pub path: String,
    /// The container file itself is missing (no digest check was possible).
    pub missing: bool,
    /// `(actual, expected)` when the recorded size didn't match.
    pub size_mismatch: Option<(u64, u64)>,
    /// `(actual, expected)` when the recorded MD5 didn't match.
    pub md5_mismatch: Option<(String, String)>,
    /// `(actual, expected)` when the recorded SHA1 didn't match.
    pub sha1_mismatch: Option<(String, String)>,
    /// Where the corrupt container was renamed to, if `verify` was run with
    /// `fix: true`.
    pub quarantined_to: Option<Utf8PathBuf>,
    /// Signature status, when a signature check was requested at all
    /// (`require_signature` or a keyring was given). `None` means no
    /// signature check was requested for this run.
    pub signature: Option<SignatureStatus>,
}

/// Outcome of [`verify`].
#[derive(Debug, Clone, Default)]
pub struct VerifyReport {
    /// Number of containers whose size/MD5/SHA1 all matched the index.
    pub ok: usize,
    /// Total number of indexed entries examined.
    pub total: usize,
    /// Missing or corrupt entries (see [`VerifyProblem`]).
    pub problems: Vec<VerifyProblem>,
    /// The regenerated package count, if `fix: true` and there was anything
    /// to fix (`None` when nothing needed reindexing).
    pub reindexed: Option<usize>,
}

impl VerifyReport {
    /// Whether every indexed container matched its recorded digest/size.
    pub fn is_clean(&self) -> bool {
        self.problems.is_empty()
    }

    /// Count of entries whose container file is missing outright.
    pub fn missing_count(&self) -> usize {
        self.problems.iter().filter(|p| p.missing).count()
    }

    /// Count of entries present on disk but with a digest/size mismatch.
    pub fn corrupt_count(&self) -> usize {
        self.problems
            .iter()
            .filter(|p| {
                !p.missing
                    && (p.size_mismatch.is_some()
                        || p.md5_mismatch.is_some()
                        || p.sha1_mismatch.is_some())
            })
            .count()
    }

    /// Count of entries with a signature problem (missing when required, or
    /// failing cryptographic verification).
    pub fn signature_problems(&self) -> usize {
        self.problems
            .iter()
            .filter(|p| {
                matches!(
                    p.signature,
                    Some(SignatureStatus::Unsigned) | Some(SignatureStatus::Invalid)
                )
            })
            .count()
    }
}

/// Check each indexed container's size/MD5/SHA1 against the file on disk,
/// and (when `require_signature` or `keyring` is given) its OpenPGP
/// signature. `fix`: quarantine corrupt containers (rename to `.corrupt`)
/// and regenerate the index afterward, so missing/corrupt entries are
/// dropped. Signature problems are reported but never trigger
/// quarantine/`fix` on their own — a validly-built, correctly-indexed
/// container with a bad signature is a trust problem, not a corrupt one.
pub fn verify(
    pkgdir: &Utf8Path,
    chost: &str,
    fix: bool,
    require_signature: bool,
    keyring: Option<&Keyring>,
) -> Result<VerifyReport> {
    let rows = read_index(pkgdir)?;
    let check_signature = require_signature || keyring.is_some();
    let policy = VerifyPolicy {
        require_signature,
        keyring,
    };

    let mut ok = 0usize;
    let mut problems = Vec::new();

    for row in &rows {
        if row.path.is_empty() {
            continue;
        }
        let full = pkgdir.join(&row.path);
        if !full.exists() {
            problems.push(VerifyProblem {
                cpv: row.cpv.clone(),
                path: row.path.clone(),
                missing: true,
                size_mismatch: None,
                md5_mismatch: None,
                sha1_mismatch: None,
                quarantined_to: None,
                signature: None,
            });
            continue;
        }

        let (actual_md5, actual_sha1, actual_size, _mtime) = checksum(full.as_std_path())?;

        let size_ok = row.size.is_none_or(|s| s == actual_size);
        let md5_ok = row.md5.as_deref().is_none_or(|m| m == actual_md5);
        let sha1_ok = row.sha1.as_deref().is_none_or(|s| s == actual_sha1);
        let digest_ok = size_ok && md5_ok && sha1_ok;

        let signature = if check_signature {
            match crate::gpkg::verify_container_signature(full.as_std_path(), &policy) {
                Ok(report) if !report.signed => Some(SignatureStatus::Unsigned),
                Ok(report) => Some(match report.signature_valid {
                    Some(true) => SignatureStatus::Valid,
                    Some(false) => SignatureStatus::Invalid,
                    None => SignatureStatus::Unverified,
                }),
                Err(Error::SignatureRequired(_)) => Some(SignatureStatus::Unsigned),
                Err(_) => Some(SignatureStatus::Invalid),
            }
        } else {
            None
        };
        // `Unsigned` is only a real problem when a signature was actually
        // *required* — a keyring alone (no `require_signature`) means
        // "verify it if present," not "every container must be signed."
        // `Invalid` is always a problem regardless: a container that
        // claims to be signed but doesn't verify is never fine to reuse.
        let signature_ok = match signature {
            Some(SignatureStatus::Unsigned) => !require_signature,
            Some(SignatureStatus::Invalid) => false,
            _ => true,
        };

        if digest_ok && signature_ok {
            ok += 1;
            continue;
        }

        let quarantined_to = if fix && !digest_ok {
            let quarantined = Utf8PathBuf::from(format!("{full}.corrupt"));
            std::fs::rename(full.as_std_path(), quarantined.as_std_path())?;
            Some(quarantined)
        } else {
            None
        };

        problems.push(VerifyProblem {
            cpv: row.cpv.clone(),
            path: row.path.clone(),
            missing: false,
            // `.then(|| ..)`, not `.then_some(..)`: the recorded value only
            // exists when the corresponding `*_ok` is false — eager evaluation
            // would unwrap a None on rows the index recorded without that field.
            size_mismatch: (!size_ok).then(|| {
                (
                    actual_size,
                    row.size.expect("recorded size exists on mismatch"),
                )
            }),
            md5_mismatch: (!md5_ok).then(|| {
                (
                    actual_md5.clone(),
                    row.md5.clone().expect("recorded md5 exists on mismatch"),
                )
            }),
            sha1_mismatch: (!sha1_ok).then(|| {
                (
                    actual_sha1.clone(),
                    row.sha1.clone().expect("recorded sha1 exists on mismatch"),
                )
            }),
            quarantined_to,
            signature,
        });
    }

    let reindexed = if fix && problems.iter().any(|p| p.quarantined_to.is_some()) {
        Some(index_pkgdir(pkgdir, chost)?.0)
    } else {
        None
    };

    Ok(VerifyReport {
        ok,
        total: rows.len(),
        problems,
        reindexed,
    })
}

/// One container involved in a [`prune`] decision.
#[derive(Debug, Clone)]
pub struct PruneEntry {
    /// `category/PF`.
    pub cpv: String,
    /// This container's `BUILD_ID` (`0` for the implicit single-instance form).
    pub build_id: u32,
    /// Path relative to `PKGDIR`.
    pub rel: String,
}

/// Outcome of [`prune`]. Only cpvs that actually had more than one container
/// are represented — an untouched cpv (already down to one instance) isn't
/// reported at all.
#[derive(Debug, Clone, Default)]
pub struct PruneReport {
    /// The newest-`BUILD_ID` container kept for each cpv that had extras.
    pub kept: Vec<PruneEntry>,
    /// Older containers removed (or, under `dry_run`, that would be removed).
    pub removed: Vec<PruneEntry>,
    /// The regenerated package count, if anything was actually removed
    /// (`None` under `dry_run`, or when there was nothing to prune).
    pub reindexed: Option<usize>,
}

/// Keep only the newest `BUILD_ID` container per (cpv, chost, build_env_key),
/// delete the rest, and regenerate the index. `dry_run`: report what would
/// be deleted without touching anything.
pub fn prune(pkgdir: &Utf8Path, chost: &str, dry_run: bool) -> Result<PruneReport> {
    if !pkgdir.exists() {
        return Err(Error::NoPkgdir(pkgdir.as_std_path().to_path_buf()));
    }

    let mut files: Vec<(String, PathBuf)> = Vec::new();
    find_gpkg_containers(pkgdir.as_std_path(), pkgdir.as_std_path(), &mut files)?;

    // Group container files by (cpv, chost, build_env_key), each carrying its resolved build-id.
    // This allows multiple variants with different CHOST or build_env_key to coexist.
    type Identity = (String, String, String);
    type IdentityVariant = (u32, String, PathBuf);
    let mut by_identity: BTreeMap<Identity, Vec<IdentityVariant>> = BTreeMap::new();
    for (rel, full) in &files {
        let meta = match crate::read_metadata(full) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let Some(cpv) = container_cpv_from_meta(&meta) else {
            continue;
        };
        let chost_val = meta.get("CHOST").cloned().unwrap_or_default();
        let cflags = meta.get("CFLAGS").cloned().unwrap_or_default();
        let cxxflags = meta.get("CXXFLAGS").cloned().unwrap_or_default();
        let ldflags = meta.get("LDFLAGS").cloned().unwrap_or_default();
        let rustflags = meta.get("RUSTFLAGS").cloned().unwrap_or_default();
        let build_env_key = crate::index::build_env_key(&cflags, &cxxflags, &ldflags, &rustflags);
        let build_id = container_build_id(&meta, rel);

        by_identity
            .entry((cpv, chost_val, build_env_key))
            .or_default()
            .push((build_id, rel.clone(), full.clone()));
    }

    let mut kept = Vec::new();
    let mut removed = Vec::new();
    for ((cpv, _chost_val, _build_env_key), mut variants) in by_identity {
        if variants.len() < 2 {
            continue;
        }
        variants.sort_by_key(|(build_id, ..)| *build_id);
        let (kept_id, kept_rel, _) = variants.last().unwrap().clone();
        kept.push(PruneEntry {
            cpv: cpv.clone(),
            build_id: kept_id,
            rel: kept_rel,
        });
        for (build_id, rel, full) in &variants[..variants.len() - 1] {
            if !dry_run {
                std::fs::remove_file(full)?;
            }
            removed.push(PruneEntry {
                cpv: cpv.clone(),
                build_id: *build_id,
                rel: rel.clone(),
            });
        }
    }

    let reindexed = if !dry_run && !removed.is_empty() {
        Some(index_pkgdir(pkgdir, chost)?.0)
    } else {
        None
    };

    Ok(PruneReport {
        kept,
        removed,
        reindexed,
    })
}

/// Extract CPV from metadata (same as container_cpv but from already-read metadata).
fn container_cpv_from_meta(meta: &BTreeMap<String, String>) -> Option<String> {
    let cat = meta.get("CATEGORY")?;
    let pf = meta.get("PF")?;
    if cat.is_empty() || pf.is_empty() {
        return None;
    }
    Some(format!("{cat}/{pf}"))
}

/// A container's `BUILD_ID` from its already-read metadata: prefer the
/// metadata's own field, else parse it from the `<PF>-<BUILD_ID>.gpkg.tar`
/// filename, else `0` (the implicit single-instance case — sorts below any
/// explicit build id, so it's always pruned in favor of a numbered one
/// sharing the same cpv).
fn container_build_id(meta: &BTreeMap<String, String>, rel: &str) -> u32 {
    meta.get("BUILD_ID")
        .and_then(|s| s.parse().ok())
        .or_else(|| parse_build_id_from_name(rel))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::{GpkgInput, write_gpkg};

    #[test]
    fn parse_index_rows_reads_digest_and_size_fields() {
        let text = "VERSION: 0\n\nCPV: app-test/foo-1.0\nPATH: app-test/foo-1.0-1.gpkg.tar\nMD5: abc\nSHA1: def\nSIZE: 42\nBUILD_ID: 1\nCHOST: aarch64-unknown-linux-gnu\nCFLAGS: -O2 -march=armv8-a\n\n";
        let rows = parse_index_rows(text);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cpv, "app-test/foo-1.0");
        assert_eq!(rows[0].path, "app-test/foo-1.0-1.gpkg.tar");
        assert_eq!(rows[0].md5.as_deref(), Some("abc"));
        assert_eq!(rows[0].sha1.as_deref(), Some("def"));
        assert_eq!(rows[0].size, Some(42));
        assert_eq!(rows[0].build_id, Some(1));
        assert_eq!(rows[0].chost, "aarch64-unknown-linux-gnu");
        assert_eq!(rows[0].cflags, "-O2 -march=armv8-a");
        assert!(!rows[0].build_env_key.is_empty());
    }

    #[test]
    fn parse_index_rows_empty_build_env_key_for_generic_flags() {
        let text =
            "CPV: app-test/foo-1.0\nPATH: app-test/foo-1.0-1.gpkg.tar\nCFLAGS: -O2 -pipe\n\n";
        let rows = parse_index_rows(text);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].build_env_key, "");
    }

    #[test]
    fn parse_index_rows_skips_the_header_block() {
        let text =
            "VERSION: 0\nPACKAGES: 1\n\nCPV: app-test/foo-1.0\nPATH: app-test/foo-1.0.gpkg.tar\n\n";
        let rows = parse_index_rows(text);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].build_id.is_none());
    }

    /// Seed `pkgdir/<cat>/<pf>-<build_id>.gpkg.tar`, a real container with
    /// enough metadata for `container_cpv`/`container_build_id` and for
    /// `index_pkgdir`'s own indexer to pick it up.
    fn seed_container(work: &Path, pkgdir: &Utf8Path, cat: &str, pf: &str, build_id: u32) {
        let image = work.join(format!("image-{pf}-{build_id}"));
        std::fs::create_dir_all(image.join("usr/bin")).unwrap();
        std::fs::write(image.join("usr/bin/hello"), b"hi\n").unwrap();

        let meta = work.join(format!("vdb-{pf}-{build_id}"));
        std::fs::create_dir_all(&meta).unwrap();
        for (k, v) in [
            ("PF", pf),
            ("CATEGORY", cat),
            ("SLOT", "0"),
            ("EAPI", "8"),
            ("repository", "gentoo"),
            ("BUILD_ID", &build_id.to_string()),
            ("CHOST", "x86_64-pc-linux-gnu"),
            ("CFLAGS", "-O2 -march=x86-64-v3"),
        ] {
            std::fs::write(meta.join(k), format!("{v}\n")).unwrap();
        }

        let container = pkgdir.join(format!("{cat}/{pf}-{build_id}.gpkg.tar"));
        std::fs::create_dir_all(container.parent().unwrap()).unwrap();
        write_gpkg(
            &GpkgInput {
                image_dir: &image,
                metadata_dir: &meta,
                basename: pf,
                signing: None,
            },
            container.as_std_path(),
        )
        .unwrap();
    }

    #[test]
    fn verify_reports_ok_for_an_untouched_container() {
        let tmp = tempfile::tempdir().unwrap();
        let pkgdir = camino::Utf8Path::from_path(tmp.path()).unwrap();
        seed_container(tmp.path(), pkgdir, "app-test", "foo-1.0", 1);
        index_pkgdir(pkgdir, "x86_64-pc-linux-gnu").unwrap();

        let report = verify(pkgdir, "x86_64-pc-linux-gnu", false, false, None).unwrap();
        assert!(report.is_clean());
        assert_eq!(report.ok, 1);
        assert_eq!(report.total, 1);
    }

    #[test]
    fn verify_detects_a_corrupted_container() {
        let tmp = tempfile::tempdir().unwrap();
        let pkgdir = camino::Utf8Path::from_path(tmp.path()).unwrap();
        seed_container(tmp.path(), pkgdir, "app-test", "foo-1.0", 1);
        index_pkgdir(pkgdir, "x86_64-pc-linux-gnu").unwrap();

        let container = pkgdir.join("app-test/foo-1.0-1.gpkg.tar");
        let mut bytes = std::fs::read(&container).unwrap();
        bytes.push(0xff);
        std::fs::write(&container, bytes).unwrap();

        let report = verify(pkgdir, "x86_64-pc-linux-gnu", false, false, None).unwrap();
        assert!(!report.is_clean());
        assert_eq!(report.corrupt_count(), 1);
        assert_eq!(report.missing_count(), 0);
        assert!(container.exists(), "not quarantined without fix");
        assert!(report.problems[0].quarantined_to.is_none());
    }

    /// A row the index recorded without SIZE must not panic when another
    /// field flags the container as a problem (the eager-`then_some`
    /// regression): the report carries the md5 mismatch, size stays `None`.
    #[test]
    fn verify_handles_a_size_less_index_row_with_an_md5_mismatch() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pkgdir = camino::Utf8Path::from_path(tmp.path()).expect("utf8 tempdir");
        seed_container(tmp.path(), pkgdir, "app-test", "foo-1.0", 1);
        index_pkgdir(pkgdir, "x86_64-pc-linux-gnu").expect("index");

        let index_path = pkgdir.join("Packages");
        let stripped: String = std::fs::read_to_string(&index_path)
            .expect("read index")
            .lines()
            .filter(|l| !l.starts_with("SIZE: "))
            .map(|l| format!("{l}\n"))
            .collect();
        std::fs::write(&index_path, stripped).expect("write index");

        let container = pkgdir.join("app-test/foo-1.0-1.gpkg.tar");
        let mut bytes = std::fs::read(&container).expect("read container");
        bytes.push(0xff);
        std::fs::write(&container, bytes).expect("corrupt container");

        let report = verify(pkgdir, "x86_64-pc-linux-gnu", false, false, None).expect("verify");
        assert!(!report.is_clean());
        assert_eq!(report.corrupt_count(), 1);
        assert!(report.problems[0].size_mismatch.is_none());
        assert!(report.problems[0].md5_mismatch.is_some());
    }

    #[test]
    fn verify_with_fix_quarantines_and_reindexes() {
        let tmp = tempfile::tempdir().unwrap();
        let pkgdir = camino::Utf8Path::from_path(tmp.path()).unwrap();
        seed_container(tmp.path(), pkgdir, "app-test", "foo-1.0", 1);
        index_pkgdir(pkgdir, "x86_64-pc-linux-gnu").unwrap();

        let container = pkgdir.join("app-test/foo-1.0-1.gpkg.tar");
        let mut bytes = std::fs::read(&container).unwrap();
        bytes.push(0xff);
        std::fs::write(&container, bytes).unwrap();

        let report = verify(pkgdir, "x86_64-pc-linux-gnu", true, false, None).unwrap();

        assert!(!container.exists());
        assert!(pkgdir.join("app-test/foo-1.0-1.gpkg.tar.corrupt").exists());
        assert_eq!(report.reindexed, Some(0));
        let idx = std::fs::read_to_string(pkgdir.join("Packages")).unwrap();
        assert!(!idx.contains("CPV: app-test/foo-1.0"));
    }

    #[test]
    fn verify_reports_a_missing_container() {
        let tmp = tempfile::tempdir().unwrap();
        let pkgdir = camino::Utf8Path::from_path(tmp.path()).unwrap();
        seed_container(tmp.path(), pkgdir, "app-test", "foo-1.0", 1);
        index_pkgdir(pkgdir, "x86_64-pc-linux-gnu").unwrap();
        std::fs::remove_file(pkgdir.join("app-test/foo-1.0-1.gpkg.tar")).unwrap();

        let report = verify(pkgdir, "x86_64-pc-linux-gnu", false, false, None).unwrap();
        assert_eq!(report.missing_count(), 1);
        assert_eq!(report.corrupt_count(), 0);
    }

    #[test]
    fn prune_keeps_only_the_newest_build_id() {
        let tmp = tempfile::tempdir().unwrap();
        let pkgdir = camino::Utf8Path::from_path(tmp.path()).unwrap();
        seed_container(tmp.path(), pkgdir, "app-test", "foo-1.0", 1);
        seed_container(tmp.path(), pkgdir, "app-test", "foo-1.0", 2);

        let report = prune(pkgdir, "x86_64-pc-linux-gnu", false).unwrap();

        assert!(!pkgdir.join("app-test/foo-1.0-1.gpkg.tar").exists());
        assert!(pkgdir.join("app-test/foo-1.0-2.gpkg.tar").exists());
        assert_eq!(report.kept.len(), 1);
        assert_eq!(report.kept[0].build_id, 2);
        assert_eq!(report.removed.len(), 1);
        assert_eq!(report.removed[0].build_id, 1);
        assert_eq!(report.reindexed, Some(1));
        let idx = std::fs::read_to_string(pkgdir.join("Packages")).unwrap();
        assert!(idx.contains("PATH: app-test/foo-1.0-2.gpkg.tar"));
    }

    #[test]
    fn prune_dry_run_deletes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let pkgdir = camino::Utf8Path::from_path(tmp.path()).unwrap();
        seed_container(tmp.path(), pkgdir, "app-test", "foo-1.0", 1);
        seed_container(tmp.path(), pkgdir, "app-test", "foo-1.0", 2);

        let report = prune(pkgdir, "x86_64-pc-linux-gnu", true).unwrap();

        assert!(pkgdir.join("app-test/foo-1.0-1.gpkg.tar").exists());
        assert!(pkgdir.join("app-test/foo-1.0-2.gpkg.tar").exists());
        assert_eq!(report.removed.len(), 1);
        assert_eq!(report.reindexed, None);
    }

    #[test]
    fn list_index_reads_a_real_index() {
        let tmp = tempfile::tempdir().unwrap();
        let pkgdir = camino::Utf8Path::from_path(tmp.path()).unwrap();
        seed_container(tmp.path(), pkgdir, "app-test", "foo-1.0", 1);
        index_pkgdir(pkgdir, "x86_64-pc-linux-gnu").unwrap();

        let rows = list_index(pkgdir).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cpv, "app-test/foo-1.0");
        assert_eq!(rows[0].chost, "x86_64-pc-linux-gnu");
        assert!(
            !rows[0].build_env_key.is_empty(),
            "seed_container's -march=x86-64-v3 must round-trip through index_pkgdir into a keyed entry"
        );
    }
}
