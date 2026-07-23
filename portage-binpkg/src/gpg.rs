//! OpenPGP primitives (the `pgp`/rpgp crate) for GPKG signing/verification.
//!
//! No `gpg`/`gpg-agent` subprocess — a native Rust OpenPGP implementation,
//! RFC 9580 compliant, so real `gpg --verify` and gemato's own verification
//! environments (`SystemGPGEnvironment`, and `PGPyEnvironment` — gemato's own
//! from-scratch, non-gpg-binary OpenPGP backend, the direct Python analog of
//! this module) can read what we produce, and we can read what they produce.
//! "gemato compatible" here means standard OpenPGP armor, not anything
//! gemato-specific in the wire format.
//!
//! Scheme (matches real portage's `BINPKG_GPG_*`/gpkg.py, redefined only
//! where it assumed a gpg-agent/keybox we don't have — see the module docs
//! in `crate::gpkg` and `todo/fakeroot-privilege-backends.md` for the
//! config-var mapping):
//! - Signing key: an armored **secret key file** (not a gpg keyring
//!   key-ID — there is no gpg-agent/pinentry here).
//! - Verify keyring: a flat directory of armored **public key files**
//!   (`*.asc`), unioned into an in-memory [`Keyring`] — not a gpg keybox.
//!
//! Known, deliberate v1 simplifications (documented, not silent): signing
//! always uses the secret key's **primary key**, never a dedicated signing
//! subkey (matches the `pgp` crate's own README example); key/signature
//! expiry and revocation are not walked (structural self-signature/subkey-
//! binding validation via [`SignedPublicKey::verify_bindings`] happens on
//! import, but a since-expired or since-revoked key already in the keyring
//! is not rejected at verify time). Both are reasonable follow-ups, not
//! required for the core signing/verification round trip to be correct.

use std::path::Path;

use pgp::composed::{
    ArmorOptions, CleartextSignedMessage, Deserializable, DetachedSignature, SignedPublicKey,
    SignedSecretKey,
};
/// Hash algorithm for a detached member signature (`BINPKG_GPG_SIGNING_DIGEST`).
pub use pgp::crypto::hash::HashAlgorithm;
use pgp::types::{KeyDetails, Password};

use crate::error::{Error, Result};

fn sig_err(context: &str, e: impl std::fmt::Display) -> Error {
    Error::Signature(format!("{context}: {e}"))
}

/// A loaded, decrypted secret key ready to sign.
pub struct SigningKey {
    key: SignedSecretKey,
    password: Password,
    /// Hash algorithm for detached member signatures
    /// (`BINPKG_GPG_SIGNING_DIGEST`, default `SHA512` matching real
    /// portage's own default).
    digest: HashAlgorithm,
}

impl SigningKey {
    /// Load an armored secret-key file (`BINPKG_GPG_SIGNING_KEY`).
    /// `passphrase` unlocks it; pass `""` for an unencrypted key.
    pub fn load(path: &Path, passphrase: &str, digest: HashAlgorithm) -> Result<Self> {
        let (key, _headers) = SignedSecretKey::from_armor_file(path)
            .map_err(|e| sig_err(&format!("loading secret key {}", path.display()), e))?;
        let password: Password = if passphrase.is_empty() {
            Password::empty()
        } else {
            passphrase.into()
        };
        // Fail fast on a wrong passphrase / corrupt key material now,
        // rather than at the first real sign call.
        key.primary_key
            .unlock(&password, |_, _| Ok(()))
            .map_err(|e| sig_err("unlocking secret key", e))?
            .map_err(|e| sig_err("unlocking secret key", e))?;
        Ok(Self {
            key,
            password,
            digest,
        })
    }

    /// The signer's OpenPGP fingerprint (for display / `em maint binpkg`
    /// reporting).
    pub fn fingerprint(&self) -> String {
        hex::encode_upper(self.key.primary_key.fingerprint().as_bytes())
    }
}

/// Clearsign `text` (the plain Manifest body) — a full armored
/// `-----BEGIN PGP SIGNED MESSAGE-----` block (RFC 9580 Cleartext
/// Signature Framework), the same format real portage's `gpg --clear-sign`
/// produces for a GPKG `Manifest` member.
pub fn clearsign(text: &str, key: &SigningKey) -> Result<String> {
    let mut rng = rand::thread_rng();
    let msg = CleartextSignedMessage::sign(&mut rng, text, &key.key.primary_key, &key.password)
        .map_err(|e| sig_err("clearsigning Manifest", e))?;
    msg.to_armored_string(ArmorOptions::default())
        .map_err(|e| sig_err("armoring clearsign message", e))
}

/// A detached, ASCII-armored signature over raw bytes (for a GPKG member's
/// sibling `<name>.sig`), using `key`'s configured digest algorithm.
pub fn sign_detached(data: &[u8], key: &SigningKey) -> Result<String> {
    let rng = rand::thread_rng();
    let sig = DetachedSignature::sign_binary_data(
        rng,
        &key.key.primary_key,
        &key.password,
        key.digest,
        data,
    )
    .map_err(|e| sig_err("signing detached member", e))?;
    sig.to_armored_string(ArmorOptions::default())
        .map_err(|e| sig_err("armoring detached signature", e))
}

/// A parsed, self-signature-validated public-key certificate plus a
/// display summary — what [`parse_public_key`] returns for `em maint
/// binpkg gpg-import`'s printout.
pub struct ImportedKey {
    /// Hex-encoded (uppercase) OpenPGP fingerprint of the primary key.
    pub fingerprint: String,
    /// The certificate's first user ID, if any.
    pub primary_uid: Option<String>,
    /// Number of subkeys the certificate carries.
    pub subkeys: usize,
}

/// Re-armor `key` (e.g. for `em maint binpkg gpg-import` to write the
/// canonical form into the verify-keyring directory, rather than storing
/// the caller's original file byte-for-byte).
pub fn export_public_key(key: &SignedPublicKey) -> Result<Vec<u8>> {
    key.to_armored_bytes(ArmorOptions::default())
        .map_err(|e| sig_err("re-armoring public key", e))
}

/// Parse one armored public-key blob and validate its self-signatures /
/// subkey-binding signatures (`SignedPublicKey::verify_bindings`, the same
/// check gemato's own `PGPyEnvironment.import_key` performs before trusting
/// a key) — rejects a structurally invalid certificate before it ever
/// reaches a keyring.
pub fn parse_public_key(armored: &[u8]) -> Result<(SignedPublicKey, ImportedKey)> {
    let (key, _headers) = SignedPublicKey::from_armor_single(armored)
        .map_err(|e| sig_err("parsing public key", e))?;
    key.verify_bindings()
        .map_err(|e| sig_err("validating key self-signatures", e))?;
    let primary_uid = key
        .details
        .users
        .first()
        .map(|u| String::from_utf8_lossy(u.id.id()).into_owned());
    let info = ImportedKey {
        fingerprint: hex::encode_upper(key.primary_key.fingerprint().as_bytes()),
        primary_uid,
        subkeys: key.public_subkeys.len(),
    };
    Ok((key, info))
}

/// An in-memory union of public-key certificates — the flat-file analogue
/// of a gpg verify keyring (`BINPKG_GPG_VERIFY_GPG_HOME`).
pub struct Keyring {
    certs: Vec<SignedPublicKey>,
}

impl Keyring {
    /// Build a keyring from already-parsed, self-signature-validated certs.
    pub fn new(certs: Vec<SignedPublicKey>) -> Self {
        Self { certs }
    }

    /// Whether this keyring has no certificates at all.
    pub fn is_empty(&self) -> bool {
        self.certs.is_empty()
    }

    /// Number of certificates in this keyring.
    pub fn len(&self) -> usize {
        self.certs.len()
    }
}

/// Load every `*.asc` file directly under `dir` into a [`Keyring`].
/// `Ok(None)` if `dir` doesn't exist at all (distinct from an
/// existing-but-empty directory) — callers use this to distinguish "no
/// verify keyring configured" from "configured but no keys imported yet".
pub fn load_keyring_dir(dir: &Path) -> Result<Option<Keyring>> {
    if !dir.is_dir() {
        return Ok(None);
    }
    let mut certs = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("asc") {
            continue;
        }
        let bytes = std::fs::read(&path)?;
        let (cert, _info) = parse_public_key(&bytes)?;
        certs.push(cert);
    }
    Ok(Some(Keyring::new(certs)))
}

/// Verify a clearsigned Manifest against `keyring`, returning the recovered
/// plaintext DATA lines plus the signer's fingerprint.
pub fn verify_clearsign(armored: &str) -> Result<CleartextSignedMessage> {
    let (msg, _headers) = CleartextSignedMessage::from_string(armored)
        .map_err(|e| sig_err("parsing clearsign Manifest", e))?;
    Ok(msg)
}

/// Verify `msg` (from [`verify_clearsign`]) against every key in `keyring` —
/// each cert's primary key, then each of its subkeys (a signature's
/// issuer-fingerprint subpacket doesn't uniquely identify *which*
/// certificate in a multi-cert keyring signed it without a scan like this;
/// `SignedPublicKey`'s own `VerifyingKey` impl only ever checks its primary
/// key, never its subkeys). Returns the recovered plaintext and the
/// fingerprint of whichever key verified; `Err` if none did.
pub fn verify_against_keyring(
    msg: &CleartextSignedMessage,
    keyring: &Keyring,
) -> Result<(String, String)> {
    for cert in &keyring.certs {
        if msg.verify(&cert.primary_key).is_ok() {
            let fp = hex::encode_upper(cert.primary_key.fingerprint().as_bytes());
            return Ok((msg.signed_text(), fp));
        }
        for sk in &cert.public_subkeys {
            if msg.verify(&sk.key).is_ok() {
                let fp = hex::encode_upper(sk.key.fingerprint().as_bytes());
                return Ok((msg.signed_text(), fp));
            }
        }
    }
    Err(Error::Signature(
        "no key in the verify keyring matches this signature (unknown signer)".into(),
    ))
}

/// Verify a detached, armored signature over `data` against every key in
/// `keyring` (same primary-then-subkeys scan as [`verify_against_keyring`]),
/// returning the signer's fingerprint on success.
pub fn verify_detached(data: &[u8], armored_sig: &str, keyring: &Keyring) -> Result<String> {
    let (sig, _headers) = DetachedSignature::from_string(armored_sig)
        .map_err(|e| sig_err("parsing detached signature", e))?;
    for cert in &keyring.certs {
        if sig.signature.verify(&cert.primary_key, data).is_ok() {
            return Ok(hex::encode_upper(cert.primary_key.fingerprint().as_bytes()));
        }
        for sk in &cert.public_subkeys {
            if sig.signature.verify(&sk.key, data).is_ok() {
                return Ok(hex::encode_upper(sk.key.fingerprint().as_bytes()));
            }
        }
    }
    Err(Error::Signature(
        "no key in the verify keyring matches this detached signature (unknown signer)".into(),
    ))
}

/// Whether `manifest_text` carries an OpenPGP cleartext-signature wrapper
/// (real portage's own detection rule in `gpkg.py`'s `_verify_binpkg`).
pub fn looks_signed(manifest_text: &str) -> bool {
    manifest_text.contains("-----BEGIN PGP SIGNATURE-----")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pgp::composed::{EncryptionCaps, KeyType, SecretKeyParamsBuilder, SubkeyParamsBuilder};
    use pgp::crypto::ecc_curve::ECCCurve;

    /// A throwaway Ed25519 keypair (plus one encryption subkey, to exercise
    /// `ImportedKey::subkeys` reporting), generated fresh per test — never a
    /// checked-in real key. Signing always uses the primary key (see this
    /// module's doc comment), so the subkey here is never used to sign.
    fn gen_test_key(uid: &str) -> (SignedSecretKey, SignedPublicKey) {
        let mut rng = rand::thread_rng();
        let mut params = SecretKeyParamsBuilder::default();
        params
            .key_type(KeyType::Ed25519Legacy)
            .can_certify(true)
            .can_sign(true)
            .primary_user_id(uid.into())
            .subkeys(vec![
                SubkeyParamsBuilder::default()
                    .key_type(KeyType::ECDH(ECCCurve::Curve25519Legacy))
                    .can_encrypt(EncryptionCaps::All)
                    .build()
                    .expect("subkey params"),
            ]);
        let secret_key_params = params.build().expect("secret key params");
        let secret: SignedSecretKey = secret_key_params.generate(&mut rng).expect("generate key");
        let public = secret.to_public_key();
        (secret, public)
    }

    fn armor_secret(key: &SignedSecretKey) -> Vec<u8> {
        key.to_armored_bytes(ArmorOptions::default())
            .expect("armor secret key")
    }

    fn armor_public(key: &SignedPublicKey) -> Vec<u8> {
        key.to_armored_bytes(ArmorOptions::default())
            .expect("armor public key")
    }

    #[test]
    fn clearsign_round_trips_and_verifies() {
        let (secret, public) = gen_test_key("Test Key <test@example.com>");
        let tmp = tempfile::tempdir().unwrap();
        let key_path = tmp.path().join("signing.asc");
        std::fs::write(&key_path, armor_secret(&secret)).unwrap();

        let signing = SigningKey::load(&key_path, "", HashAlgorithm::Sha512).unwrap();
        let armored = clearsign("DATA foo 3 SHA512 abc BLAKE2B def\n", &signing).unwrap();
        assert!(looks_signed(&armored));

        let msg = verify_clearsign(&armored).unwrap();
        let keyring = Keyring::new(vec![public]);
        let (plain, fingerprint) = verify_against_keyring(&msg, &keyring).unwrap();
        assert!(plain.contains("DATA foo 3 SHA512 abc BLAKE2B def"));
        assert_eq!(fingerprint, signing.fingerprint());
    }

    #[test]
    fn clearsign_verify_fails_against_wrong_keyring() {
        let (secret, _public) = gen_test_key("Signer <signer@example.com>");
        let (_other_secret, other_public) = gen_test_key("Bystander <bystander@example.com>");
        let tmp = tempfile::tempdir().unwrap();
        let key_path = tmp.path().join("signing.asc");
        std::fs::write(&key_path, armor_secret(&secret)).unwrap();

        let signing = SigningKey::load(&key_path, "", HashAlgorithm::Sha512).unwrap();
        let armored = clearsign("DATA foo 3 SHA512 abc BLAKE2B def\n", &signing).unwrap();
        let msg = verify_clearsign(&armored).unwrap();

        let keyring = Keyring::new(vec![other_public]);
        assert!(verify_against_keyring(&msg, &keyring).is_err());
    }

    #[test]
    fn detached_signature_round_trips_and_verifies() {
        let (secret, public) = gen_test_key("Test Key <test@example.com>");
        let tmp = tempfile::tempdir().unwrap();
        let key_path = tmp.path().join("signing.asc");
        std::fs::write(&key_path, armor_secret(&secret)).unwrap();

        let signing = SigningKey::load(&key_path, "", HashAlgorithm::Sha512).unwrap();
        let data = b"the image.tar.zst bytes";
        let armored_sig = sign_detached(data, &signing).unwrap();

        let keyring = Keyring::new(vec![public]);
        let fingerprint = verify_detached(data, &armored_sig, &keyring).unwrap();
        assert_eq!(fingerprint, signing.fingerprint());

        // Corrupting even one byte must invalidate the signature.
        let mut corrupted = data.to_vec();
        corrupted[0] ^= 0xff;
        assert!(verify_detached(&corrupted, &armored_sig, &keyring).is_err());
    }

    #[test]
    fn load_keyring_dir_distinguishes_missing_from_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");
        assert!(load_keyring_dir(&missing).unwrap().is_none());

        let empty = tmp.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let ring = load_keyring_dir(&empty).unwrap();
        assert!(ring.is_some());
        assert!(ring.unwrap().is_empty());
    }

    #[test]
    fn load_keyring_dir_imports_asc_files() {
        let (_secret, public) = gen_test_key("Test Key <test@example.com>");
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("keys");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("test.asc"), armor_public(&public)).unwrap();
        // A non-.asc file must be ignored, not choke the loader.
        std::fs::write(dir.join("README"), b"not a key").unwrap();

        let ring = load_keyring_dir(&dir).unwrap().unwrap();
        assert_eq!(ring.len(), 1);
    }

    #[test]
    fn parse_public_key_reports_fingerprint_and_subkeys() {
        let (_secret, public) = gen_test_key("Test Key <test@example.com>");
        let armored = armor_public(&public);
        let (_key, info) = parse_public_key(&armored).unwrap();
        assert_eq!(info.subkeys, 1);
        assert!(info.primary_uid.unwrap().contains("test@example.com"));
        assert_eq!(info.fingerprint.len(), 40); // SHA1-length v4 fingerprint, hex
    }
}
