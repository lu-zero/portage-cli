//! Crate archive download + checksum verification.
//!
//! Downloading is delegated to `portage_distfiles::Fetcher` — the same
//! async, resumable, atomic-write HTTP fetcher `em` itself uses for
//! distfiles — instead of a hand-rolled, no-network local-cache scan.
//! Verification is a separate pass against each `FileCrate`'s `Cargo.lock`
//! sha256 checksum, mirroring pycargoebuild's own split between
//! `fetch_crates()` (no checksum awareness) and `verify_crates()`
//! (`pycargoebuild/fetch.py:87-113`, `__main__.py:315-323`): a
//! `portage_repo::ManifestEntry` can't stand in for this because it
//! hard-checks file *size* before hashing, and `Cargo.lock` gives us a
//! checksum with no matching size.

use std::path::Path;

use anyhow::{Context, Result, bail};
use camino::Utf8Path;
use portage_distfiles::{DistDigests, Distfile, FetchConfig, Fetcher};

use crate::cargo::{Crate, FileCrate};

/// Download every crate archive into `distdir` (skips files already present).
pub async fn fetch_crates(crates: &[Crate], distdir: &Utf8Path) -> Result<()> {
    let fetcher = Fetcher::new(distdir.to_path_buf(), FetchConfig::default());
    let distfiles: Vec<Distfile> = crates
        .iter()
        .map(|c| Distfile {
            filename: c.filename(),
            urls: vec![c.download_url()],
            restriction: None,
        })
        .collect();
    let digests = DistDigests::new();
    for (df, result) in fetcher.fetch_all_digests(&distfiles, &digests).await {
        result.with_context(|| format!("fetching {}", df.filename))?;
    }
    Ok(())
}

/// Verify every fetched [`FileCrate`] against its `Cargo.lock` sha256
/// checksum. Git-archive crates have no lock checksum to check, same as
/// upstream. A mismatch is reported, not retried — remove the file to
/// force a fresh download on the next run.
pub fn verify_crates(crates: &[Crate], distdir: &Path) -> Result<()> {
    for krate in crates {
        let Crate::File(FileCrate {
            name,
            version,
            checksum,
        }) = krate
        else {
            continue;
        };
        if checksum.is_empty() {
            continue;
        }
        let path = distdir.join(krate.filename());
        let actual = sha256_file(&path).with_context(|| format!("reading {}", path.display()))?;
        if &actual != checksum {
            bail!(
                "checksum mismatch for {name}-{version} ({}):\n current: {actual}\nexpected: {checksum}\nRemove the file to try downloading again.",
                path.display(),
            );
        }
    }
    Ok(())
}

fn sha256_file(p: &Path) -> Result<String> {
    use sha2::Digest;
    let data = std::fs::read(p)?;
    let mut hasher = sha2::Sha256::new();
    hasher.update(&data);
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cargo::FileCrate;

    #[test]
    fn verify_crates_catches_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let krate = Crate::File(FileCrate {
            name: "foo".into(),
            version: "1.0.0".into(),
            checksum: "0".repeat(64),
        });
        std::fs::write(tmp.path().join("foo-1.0.0.crate"), b"not the real content").unwrap();
        let err = verify_crates(std::slice::from_ref(&krate), tmp.path()).unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"));
    }

    #[test]
    fn verify_crates_skips_empty_checksum() {
        let tmp = tempfile::tempdir().unwrap();
        let krate = Crate::File(FileCrate {
            name: "foo".into(),
            version: "1.0.0".into(),
            checksum: String::new(),
        });
        // No file written at all — an empty checksum must not even try to
        // read it.
        verify_crates(std::slice::from_ref(&krate), tmp.path()).unwrap();
    }
}
