//! `sync-git-verify-commit-signature`: the fetched tip must carry an OpenPGP signature from a key in
//! `sync-openpgp-key-path`, checked before the checkout moves, as Portage's `verify_head` does
//!
//! The check runs in process against the keyring file, with no gpg home to set up: being in the
//! configured keyring is the trust decision, so a good signature from an unrevoked, unexpired key
//! passes and everything else fails.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use portage_binpkg::gpg::{self, Keyring};
use portage_repo::RepoEntry;

use super::info_line;

/// The keyring a repository's tip commits must be signed from
pub(crate) struct CommitVerifier {
    keyring: Keyring,
    key_path: PathBuf,
}

impl CommitVerifier {
    /// The verifier `entry` asks for, or `None` when it does not verify signatures
    pub(super) fn for_repo(entry: &RepoEntry) -> Result<Option<Self>> {
        if !entry.sync_git_verify_commit_signature {
            return Ok(None);
        }
        let Some(key_path) = entry.sync_openpgp_key_path.clone() else {
            bail!(
                "sync-git-verify-commit-signature is set for '{}' but sync-openpgp-key-path is not",
                entry.name
            );
        };
        let keyring = gpg::load_keyring_file(&key_path)
            .with_context(|| format!("verification impossible: keyring {}", key_path.display()))?;
        Ok(Some(Self { keyring, key_path }))
    }

    /// The Gentoo release keyring from the test data, which signs none of the fixture commits
    #[cfg(test)]
    pub(super) fn gentoo_for_tests() -> Self {
        let key_path = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/data/gentoo-release.asc"
        ));
        let keyring = gpg::load_keyring_file(&key_path).unwrap();
        Self { keyring, key_path }
    }

    /// Check `raw`, the object of the tip commit `rev`, before anything moves to it
    pub(super) fn check(&self, raw: &[u8], rev: &str, quiet: bool) -> Result<()> {
        let Some((signature, signed)) = split_signature(raw) else {
            bail!("No valid signature found: no signature (git revision: {rev})");
        };
        let signer = gpg::verify_detached_valid(&signed, &signature, &self.keyring)
            .map_err(|e| anyhow::anyhow!("No valid signature found: {e} (git revision: {rev})"))?;
        if !quiet {
            info_line(&format!(
                "Trusted signature found on top commit (signing key {signer}, from {})",
                self.key_path.display()
            ));
        }
        Ok(())
    }
}

/// The armored `gpgsig` header of a raw commit, and the bytes it signs: the commit without it
fn split_signature(raw: &[u8]) -> Option<(String, Vec<u8>)> {
    let mut signature: Option<Vec<u8>> = None;
    let mut signed = Vec::with_capacity(raw.len());
    let mut in_headers = true;
    let mut in_signature = false;
    for line in raw.split_inclusive(|&b| b == b'\n') {
        if in_headers {
            if line == b"\n" {
                in_headers = false;
            } else if in_signature && line.first() == Some(&b' ') {
                signature.as_mut()?.extend_from_slice(&line[1..]);
                continue;
            } else if let Some(value) = line.strip_prefix(b"gpgsig ") {
                signature = Some(value.to_vec());
                in_signature = true;
                continue;
            }
            in_signature = false;
        }
        signed.extend_from_slice(line);
    }
    let signature = String::from_utf8(signature?).ok()?;
    Some((signature, signed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_signature_header_is_cut_out_of_the_signed_bytes() {
        let raw = b"tree 1\nparent 2\nauthor a\ncommitter c\ngpgsig -----BEGIN PGP SIGNATURE-----\n \n abc\n -----END PGP SIGNATURE-----\n\nmsg\n gpgsig in the body stays\n";
        let (sig, signed) = split_signature(raw).unwrap();
        assert_eq!(
            sig,
            "-----BEGIN PGP SIGNATURE-----\n\nabc\n-----END PGP SIGNATURE-----\n"
        );
        assert_eq!(
            signed,
            b"tree 1\nparent 2\nauthor a\ncommitter c\n\nmsg\n gpgsig in the body stays\n"
        );
        assert!(split_signature(b"tree 1\n\nmsg\n").is_none());
    }

    const COMMIT: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/gentoo-mirror-b23219da.commit"
    ));

    fn keyring() -> Keyring {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/gentoo-release.asc");
        gpg::load_keyring_file(std::path::Path::new(path)).unwrap()
    }

    fn at(unix: u64) -> std::time::SystemTime {
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(unix)
    }

    // A real gentoo-mirror commit, signed by the repomirrorci subkey, against the keyring Portage
    // ships; that key carries other keys' certifications too.
    #[test]
    fn a_gentoo_mirror_commit_verifies_against_the_gentoo_keyring() {
        let (sig, signed) = split_signature(COMMIT).unwrap();
        let when = at(1_761_323_456); // the commit's own day
        assert_eq!(
            gpg::verify_detached_valid_at(&signed, &sig, &keyring(), when).unwrap(),
            "F748E9B3C47E393CC24C8FAF7C2AC09CD98F2EDF"
        );
        let mut tampered = signed.clone();
        tampered[0] ^= 1;
        assert!(gpg::verify_detached_valid_at(&tampered, &sig, &keyring(), when).is_err());
        // The key expires on 2028-07-01: gpg's `Y`, which Portage rejects.
        let err =
            gpg::verify_detached_valid_at(&signed, &sig, &keyring(), at(1_846_022_400 + 86_400))
                .unwrap_err()
                .to_string();
        assert!(err.contains("expired key"), "{err}");
    }
}
