//! GPKG (GLEP 78) binary-package container writer.
//!
//! A GPKG is a **plain (uncompressed) tar** whose members, all owned `0/0`, are —
//! **in this exact order**:
//!
//! 1. `<basename>/gpkg-1` — a 0-byte format marker (must be first),
//! 2. `<basename>/metadata.tar.<c>` — the VDB-style metadata under `metadata/`,
//! 3. `<basename>/image.tar.<c>` — the installed image (`${D}`) under `image/`,
//! 4. `<basename>/Manifest` — `DATA <member> <size> SHA512 .. BLAKE2B ..` per
//!    member (must be last).
//!
//! `<basename>` is the package `PF` (e.g. `gentoo-functions-1.7.6`). The two inner
//! tars are produced with GNU `tar` (`--numeric-owner`, pax `--xattrs` for the
//! image so file capabilities/ACLs and device nodes survive) and compressed with
//! zstd — the Portage default. Requires `tar` and `zstd` on `PATH`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use blake2::Blake2b512;
use sha2::{Digest, Sha512};

use crate::error::{Error, Result};
use crate::gpg;

/// The GLEP 78 format-marker filename (and version).
const GPKG_VERSION: &str = "gpkg-1";
const METADATA_TAR: &str = "metadata.tar.zst";
const IMAGE_TAR: &str = "image.tar.zst";

/// Inputs for [`write_gpkg`].
pub struct GpkgInput<'a> {
    /// The installed image directory (`${D}`); its contents are packed under
    /// `image/` with ownership/xattrs preserved.
    pub image_dir: &'a Path,
    /// The VDB-style metadata directory (the package's `var/db/pkg/<cat>/<pf>`);
    /// its contents are packed under `metadata/`.
    pub metadata_dir: &'a Path,
    /// The package basename — `PF`, e.g. `gentoo-functions-1.7.6`.
    pub basename: &'a str,
    /// `FEATURES=binpkg-signing`: when `Some`, `metadata.tar.<c>` and
    /// `image.tar.<c>` each get a sibling detached `.sig` member, and the
    /// `Manifest` member is wrapped whole in an OpenPGP cleartext-signature
    /// block — matching real portage's own gpkg signing scheme (see this
    /// crate's `gpg` module doc).
    pub signing: Option<&'a gpg::SigningKey>,
}

/// Build a GPKG from `input` and write it to `out_path`
/// (conventionally `<PKGDIR>/<category>/<PF>-<BUILD_ID>.gpkg.tar`).
///
/// The owner/mode/xattr metadata of `image_dir` is read as it sits on disk, so the
/// caller is responsible for running this where that metadata is correct — inside
/// the privilege session (real root, sudo, or the userns box) for an unprivileged
/// build.
pub fn write_gpkg(input: &GpkgInput, out_path: &Path) -> Result<()> {
    let staging = tempfile::Builder::new().prefix("em-gpkg-").tempdir()?;
    let pkg_dir = staging.path().join(input.basename);
    std::fs::create_dir_all(&pkg_dir)?;

    // 1. the 0-byte format marker.
    let gpkg1 = pkg_dir.join(GPKG_VERSION);
    std::fs::File::create(&gpkg1)?;

    // 2. metadata.tar.zst — the VDB field files under `metadata/`, *flat with no
    //    directory entry*: portage's `get_metadata` does `extractfile(m).read()`
    //    on every member, which is `None` for a dir.
    let metadata = pkg_dir.join(METADATA_TAR);
    tar_metadata(input.metadata_dir, &metadata)?;

    // 3. image.tar.zst — `${D}` under `image/`, with xattrs (caps/ACLs/devnodes).
    let image = pkg_dir.join(IMAGE_TAR);
    tar_tree(input.image_dir, "image", &image, true)?;

    // Manifest members + the outer container's member list, built together
    // so a signed run's `.sig` siblings land in both at the same point —
    // gpkg-1 -> metadata.tar[.sig] -> image.tar[.sig] -> Manifest (must be
    // last), matching real portage's own gpkg.py assembly order.
    let b = input.basename;
    let mut manifest_members: Vec<(String, PathBuf)> = Vec::new();
    let mut container_members: Vec<String> = Vec::new();

    manifest_members.push((GPKG_VERSION.to_string(), gpkg1));
    container_members.push(format!("{b}/{GPKG_VERSION}"));

    manifest_members.push((METADATA_TAR.to_string(), metadata.clone()));
    container_members.push(format!("{b}/{METADATA_TAR}"));
    if let Some(signing) = input.signing {
        let name = format!("{METADATA_TAR}.sig");
        let sig_path = pkg_dir.join(&name);
        let armored = gpg::sign_detached(&std::fs::read(&metadata)?, signing)
            .map_err(|e| Error::Signature(format!("signing {METADATA_TAR}: {e}")))?;
        std::fs::write(&sig_path, armored)?;
        manifest_members.push((name.clone(), sig_path));
        container_members.push(format!("{b}/{name}"));
    }

    manifest_members.push((IMAGE_TAR.to_string(), image.clone()));
    container_members.push(format!("{b}/{IMAGE_TAR}"));
    if let Some(signing) = input.signing {
        let name = format!("{IMAGE_TAR}.sig");
        let sig_path = pkg_dir.join(&name);
        let armored = gpg::sign_detached(&std::fs::read(&image)?, signing)
            .map_err(|e| Error::Signature(format!("signing {IMAGE_TAR}: {e}")))?;
        std::fs::write(&sig_path, armored)?;
        manifest_members.push((name.clone(), sig_path));
        container_members.push(format!("{b}/{name}"));
    }

    // Manifest — checksums of every member above (Manifest excludes itself).
    // When signing, the whole plain-text Manifest is replaced with an
    // OpenPGP cleartext-signature block wrapping the same DATA lines (real
    // portage's `gpg --clear-sign`, RFC 9580 Cleartext Signature Framework —
    // the exact format Gentoo repo-tree `Manifest` files use).
    let manifest = pkg_dir.join("Manifest");
    write_manifest(&manifest, &manifest_members)?;
    if let Some(signing) = input.signing {
        let plain = std::fs::read_to_string(&manifest)?;
        let signed = gpg::clearsign(&plain, signing)
            .map_err(|e| Error::Signature(format!("clearsigning Manifest: {e}")))?;
        std::fs::write(&manifest, signed)?;
    }
    container_members.push(format!("{b}/Manifest"));

    // Container: a plain tar, members added in the required order (gpkg-1 first,
    // Manifest last), forced to 0/0.
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut cmd = Command::new("tar");
    cmd.arg("--numeric-owner")
        .arg("--owner=0")
        .arg("--group=0")
        .arg("--format=ustar")
        .arg("-C")
        .arg(staging.path())
        .arg("-cf")
        .arg(out_path);
    cmd.args(&container_members);
    run("tar", &mut cmd)
}

/// `tar --zstd` the whole *tree* under `dir` into `out`, renaming the root
/// to `prefix` (so members are `prefix/...`, directory entries included).
///
/// With `xattrs`, file capabilities, ACLs and device nodes are preserved
/// (pax format).
fn tar_tree(dir: &Path, prefix: &str, out: &Path, xattrs: bool) -> Result<()> {
    // Empty / missing image is valid (virtuals, symlink-only packages under
    // EPREFIX never create `ED`). `tar -C` requires the directory to exist.
    if !dir.exists() {
        std::fs::create_dir_all(dir)?;
    }
    let mut cmd = Command::new("tar");
    cmd.arg("--zstd")
        .arg("--numeric-owner")
        .arg("--format=pax")
        // Rename the `.`-rooted members to `<prefix>/…`.
        .arg(format!("--transform=s,^\\.,{prefix},"));
    if xattrs {
        cmd.arg("--xattrs").arg("--xattrs-include=*");
    }
    cmd.arg("-C").arg(dir).arg("-cf").arg(out).arg(".");
    run("tar", &mut cmd)
}

/// `tar --zstd` the *files* directly in `dir` (the VDB is flat) into `out`, each
/// member as `metadata/<name>` — with **no** `metadata/` directory entry, since
/// portage reads every metadata member with `extractfile().read()`.
fn tar_metadata(dir: &Path, out: &Path) -> Result<()> {
    let mut names: Vec<std::ffi::OsString> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name())
        .collect();
    names.sort();
    let mut cmd = Command::new("tar");
    cmd.arg("--zstd")
        .arg("--numeric-owner")
        .arg("--format=pax")
        .arg("--no-recursion")
        .arg("--transform=s,^,metadata/,")
        .arg("-C")
        .arg(dir)
        .arg("-cf")
        .arg(out);
    cmd.args(&names);
    run("tar", &mut cmd)
}

/// One parsed `DATA <name> <size> SHA512 <hex> BLAKE2B <hex>` Manifest line.
struct ManifestEntry {
    name: String,
    size: u64,
    sha512: Option<String>,
    blake2b: Option<String>,
}

/// Parse every `DATA` line out of a GLEP 74 Manifest body (plain text —
/// callers recover the plaintext from a clearsign wrapper first if needed).
///
/// Non-`DATA` lines (blank lines, any future record type) are skipped
/// rather than rejected, matching real portage's own tolerant Manifest
/// parser.
fn parse_manifest_entries(text: &str) -> Vec<ManifestEntry> {
    let mut entries = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 || parts[0] != "DATA" {
            continue;
        }
        let Ok(size) = parts[2].parse::<u64>() else {
            continue;
        };
        let sha512 = parts
            .iter()
            .position(|&p| p == "SHA512")
            .and_then(|i| parts.get(i + 1))
            .map(|s| s.to_string());
        let blake2b = parts
            .iter()
            .position(|&p| p == "BLAKE2B")
            .and_then(|i| parts.get(i + 1))
            .map(|s| s.to_string());
        entries.push(ManifestEntry {
            name: parts[1].to_string(),
            size,
            sha512,
            blake2b,
        });
    }
    entries
}

/// Check `data` against `entry`'s recorded size/hashes.
fn check_entry(entry: &ManifestEntry, data: &[u8]) -> Result<()> {
    if data.len() as u64 != entry.size {
        return Err(Error::Corrupt(format!(
            "size mismatch for {}: Manifest {}, file {}",
            entry.name,
            entry.size,
            data.len()
        )));
    }
    if let Some(expect) = &entry.sha512 {
        let got = hex::encode(Sha512::digest(data));
        if got != *expect {
            return Err(Error::Corrupt(format!(
                "SHA512 mismatch for {}",
                entry.name
            )));
        }
    }
    if let Some(expect) = &entry.blake2b {
        let got = hex::encode(Blake2b512::digest(data));
        if got != *expect {
            return Err(Error::Corrupt(format!(
                "BLAKE2B mismatch for {}",
                entry.name
            )));
        }
    }
    Ok(())
}

/// Verify `file` against a GLEP 74 `DATA <name> <size> SHA512 …` Manifest line.
///
/// `member_name` is the path as stored in the Manifest (e.g. `pkg/image.tar.zst`).
fn verify_data_member(manifest: &Path, member_name: &Path, file: &Path) -> Result<()> {
    let text = std::fs::read_to_string(manifest)?;
    let want_name = member_name
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let entries = parse_manifest_entries(&text);
    let entry = entries
        .iter()
        .find(|e| Path::new(&e.name).file_name().and_then(|f| f.to_str()) == Some(want_name));
    let Some(entry) = entry else {
        // Older/hand-built packages may lack a Manifest entry; warn is too
        // noisy for a library — require it for integrity of written GPKGs.
        return Err(Error::Corrupt(format!(
            "Manifest has no DATA line for {want_name}"
        )));
    };
    let data = std::fs::read(file)?;
    check_entry(entry, &data)
}

/// Write a GLEP 74-style Manifest with one `DATA` line per member.
fn write_manifest(out: &Path, members: &[(String, PathBuf)]) -> Result<()> {
    let mut text = String::new();
    for (name, path) in members {
        let data = std::fs::read(path)?;
        let sha512 = hex::encode(Sha512::digest(&data));
        let blake2b = hex::encode(Blake2b512::digest(&data));
        text.push_str(&format!(
            "DATA {name} {} SHA512 {sha512} BLAKE2B {blake2b}\n",
            data.len()
        ));
    }
    std::fs::write(out, text)?;
    Ok(())
}

/// Policy for verifying a GPKG container's signature before trusting it.
///
/// Two independent knobs, deliberately not collapsed into one flag — the
/// direct encoding of real portage's two independent toggles
/// (`FEATURES=binpkg-request-signature` vs. `binrepos.conf`'s
/// `verify-signature=yes`).
#[derive(Default)]
pub struct VerifyPolicy<'a> {
    /// `FEATURES=binpkg-request-signature`: presence-only — reject a
    /// container whose Manifest carries no clearsign wrapper at all.
    pub require_signature: bool,
    /// A loaded keyring. When `Some` and the Manifest *is* signed, the
    /// clearsign wrapper (and any per-member `.sig`) is cryptographically
    /// checked — independent of `require_signature`.
    pub keyring: Option<&'a gpg::Keyring>,
}

/// Result of [`verify_container_signature`].
///
/// `signature_valid`/per-member entries are `None` when not checked
/// (unsigned container, or no keyring configured) rather than an error —
/// callers decide what "not checked" means for their own policy
/// (`extract_image` hard-fails on `Some(false)`, `em maint binpkg verify`
/// just reports it).
pub struct SignatureReport {
    /// Whether the Manifest carries an OpenPGP cleartext-signature wrapper.
    pub signed: bool,
    /// `Some(true/false)` once cryptographically checked against a keyring.
    pub signature_valid: Option<bool>,
    /// Fingerprint of the key that verified the Manifest signature.
    pub signer_fingerprint: Option<String>,
    /// Per detached-`.sig` member: `(member name, Some(valid))`.
    pub member_signatures: Vec<(String, Option<bool>)>,
}

/// List a container's members (paths relative to the archive root).
fn list_container(container: &Path) -> Result<Vec<String>> {
    let listing = String::from_utf8_lossy(&capture(
        "tar",
        Command::new("tar").arg("-tf").arg(container),
    )?)
    .into_owned();
    Ok(listing
        .lines()
        .map(|l| l.trim_end_matches('/').to_string())
        .collect())
}

/// Extract member `name` from `container` into `dest_dir`, returning its path.
fn extract_member(container: &Path, name: &str, dest_dir: &Path) -> Result<PathBuf> {
    run(
        "tar",
        Command::new("tar")
            .arg("-xf")
            .arg(container)
            .arg("-C")
            .arg(dest_dir)
            .arg(name),
    )?;
    Ok(dest_dir.join(name))
}

/// Verify a GPKG container's signature per `policy`, without extracting the
/// image — the single choke point both `extract_image` (hard enforcement)
/// and `em maint binpkg verify` (reporting) call into.
pub fn verify_container_signature(
    container: &Path,
    policy: &VerifyPolicy,
) -> Result<SignatureReport> {
    let staging = tempfile::Builder::new().prefix("em-gpkg-sig-").tempdir()?;
    let root = staging.path();
    let members = list_container(container)?;
    let manifest_member = members
        .iter()
        .find(|m| Path::new(m).file_name().and_then(|n| n.to_str()) == Some("Manifest"))
        .ok_or_else(|| Error::Corrupt(format!("no Manifest member in {}", container.display())))?;
    let manifest_path = extract_member(container, manifest_member, root)?;
    let manifest_text = std::fs::read_to_string(&manifest_path)?;

    if !gpg::looks_signed(&manifest_text) {
        if policy.require_signature {
            return Err(Error::SignatureRequired(container.to_path_buf()));
        }
        return Ok(SignatureReport {
            signed: false,
            signature_valid: None,
            signer_fingerprint: None,
            member_signatures: Vec::new(),
        });
    }

    let Some(keyring) = policy.keyring else {
        return Ok(SignatureReport {
            signed: true,
            signature_valid: None,
            signer_fingerprint: None,
            member_signatures: Vec::new(),
        });
    };

    let msg = gpg::verify_clearsign(&manifest_text)?;
    let (plain, signature_valid, signer_fingerprint) =
        match gpg::verify_against_keyring(&msg, keyring) {
            Ok((plain, fp)) => (plain, true, Some(fp)),
            Err(_) => (msg.signed_text(), false, None),
        };

    // Cross-check every member's recorded size/hash against the *recovered*
    // plaintext DATA lines (not the raw, possibly-clearsign-wrapped file) —
    // matches real portage's own `_verify_binpkg` sequence.
    let entries = parse_manifest_entries(&plain);
    for member in &members {
        let base = Path::new(member).file_name().and_then(|n| n.to_str());
        let Some(name) = base else { continue };
        if name == "Manifest" || name.ends_with(".sig") {
            continue;
        }
        if let Some(entry) = entries.iter().find(|e| e.name == name) {
            let path = extract_member(container, member, root)?;
            let data = std::fs::read(&path)?;
            check_entry(entry, &data)?;
        }
    }

    // Per-member detached signatures (metadata.tar.<c>.sig, image.tar.<c>.sig).
    let mut member_signatures = Vec::new();
    for member in &members {
        let Some(base) = Path::new(member).file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(data_name) = base.strip_suffix(".sig") else {
            continue;
        };
        let data_member = members
            .iter()
            .find(|m| Path::new(m).file_name().and_then(|n| n.to_str()) == Some(data_name));
        let Some(data_member) = data_member else {
            member_signatures.push((base.to_string(), Some(false)));
            continue;
        };
        let sig_path = extract_member(container, member, root)?;
        let data_path = extract_member(container, data_member, root)?;
        let armored_sig = std::fs::read_to_string(&sig_path)?;
        let data = std::fs::read(&data_path)?;
        let ok = gpg::verify_detached(&data, &armored_sig, keyring).is_ok();
        member_signatures.push((base.to_string(), Some(ok)));
    }

    Ok(SignatureReport {
        signed: true,
        signature_valid: Some(signature_valid),
        signer_fingerprint,
        member_signatures,
    })
}

/// `tar -tf` the container, returning its member listing (one path per line).
fn container_member_listing(container: &Path) -> Result<String> {
    Ok(String::from_utf8_lossy(&capture(
        "tar",
        Command::new("tar").arg("-tf").arg(container),
    )?)
    .into_owned())
}

/// The first container member whose basename starts with `prefix`
/// (e.g. `image.tar` / `metadata.tar`), as GNU tar lists it (trailing
/// slash trimmed).
///
/// `Corrupt` if none is present.
fn find_container_member<'a>(listing: &'a str, prefix: &str, container: &Path) -> Result<&'a str> {
    listing
        .lines()
        .map(|l| l.trim_end_matches('/'))
        .find(|m| {
            Path::new(m)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .starts_with(prefix)
        })
        .ok_or_else(|| Error::Corrupt(format!("no {prefix}.* member in {}", container.display())))
}

/// Extract the GPKG container's installed image into `dest` (e.g. `${D}`
/// or a merge `work_root/image`), stripping the inner `image/` prefix so
/// members land at `dest/<path>` (e.g. `dest/usr/bin/foo`).
///
/// Used by the `-k`/`--usepkg` consumer to merge a pre-built package
/// without compiling. Requires `tar` and `zstd` on `PATH`.
///
/// `policy` is checked first (via [`verify_container_signature`]) — a
/// signature-required-but-missing or fails-to-verify container is rejected
/// before any extraction happens. `VerifyPolicy::default()` reproduces the
/// old unconditional (no signature checking at all) behavior.
pub fn extract_image(container: &Path, dest: &Path, policy: VerifyPolicy) -> Result<()> {
    let report = verify_container_signature(container, &policy)?;
    if report.signature_valid == Some(false) {
        return Err(Error::Signature(format!(
            "Manifest signature does not verify against the configured keyring: {}",
            container.display()
        )));
    }
    for (name, ok) in &report.member_signatures {
        if *ok == Some(false) {
            return Err(Error::Signature(format!(
                "signature on {name} does not verify against the configured keyring: {}",
                container.display()
            )));
        }
    }

    let staging = tempfile::Builder::new().prefix("em-gpkg-img-").tempdir()?;
    let root = staging.path().to_path_buf();

    // Locate the inner `image.tar.<c>` member.
    let listing = container_member_listing(container)?;
    let member = find_container_member(&listing, "image.tar", container)?;
    let compressed = root.join(member);
    // Also pull Manifest so we can verify the image member before trust.
    let manifest_member = listing
        .lines()
        .map(|l| l.trim_end_matches('/'))
        .find(|m| Path::new(m).file_name().and_then(|n| n.to_str()) == Some("Manifest"));
    let mut extract_args = vec![member.to_string()];
    if let Some(m) = manifest_member {
        extract_args.push(m.to_string());
    }
    let mut tar_xf = Command::new("tar");
    tar_xf
        .arg("-xf")
        .arg(container)
        .arg("-C")
        .arg(&root)
        .args(&extract_args);
    run("tar", &mut tar_xf)?;

    if let Some(m) = manifest_member {
        let manifest_path = root.join(m);
        verify_data_member(&manifest_path, Path::new(member), &compressed)?;
    }

    // Decompress to image.tar.
    let image_tar = root.join("image.tar");
    let bytes = match compressed.extension().and_then(|e| e.to_str()) {
        Some("zst") => capture("zstd", Command::new("zstd").arg("-dc").arg(&compressed))?,
        Some("gz") => capture("gzip", Command::new("gzip").arg("-dc").arg(&compressed))?,
        _ => std::fs::read(&compressed)?,
    };
    std::fs::write(&image_tar, bytes)?;

    // Reject absolute members and `..` path components (classic tar slip)
    // before writing anything under `dest`.
    let image_listing = String::from_utf8_lossy(&capture(
        "tar",
        Command::new("tar").arg("-tf").arg(&image_tar),
    )?)
    .into_owned();
    for member in image_listing.lines() {
        let m = member.trim();
        if m.is_empty() {
            continue;
        }
        let p = Path::new(m);
        if p.is_absolute() || m.starts_with('/') {
            return Err(Error::Corrupt(format!("absolute path in GPKG image: {m}")));
        }
        if p.components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(Error::Corrupt(format!("path traversal in GPKG image: {m}")));
        }
    }

    // Extract with the `image/` prefix stripped, preserving owners/mode/xattrs.
    std::fs::create_dir_all(dest)?;
    run(
        "tar",
        Command::new("tar")
            .arg("--no-same-owner")
            .arg("--xattrs")
            .arg("--xattrs-include=*")
            .arg("--strip-components=1")
            .arg("-xf")
            .arg(&image_tar)
            .arg("-C")
            .arg(dest),
    )?;

    // Belt-and-braces: refuse to leave extracted trees that escaped dest
    // (e.g. via symlink races). Walk the result and check every path.
    validate_tree_under(dest)?;
    Ok(())
}

/// Ensure every path under `root` stays within it (no symlink-escape after extract).
fn validate_tree_under(root: &Path) -> Result<()> {
    let root_canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                // Symlinks are never followed by this walk (only real
                // directories are pushed onto `stack`), so a symlink cannot
                // escape `root` via traversal. Absolute targets are legitimate
                // in Portage images (e.g. `/lib64/ld-linux...`); the real
                // tar-slip vector — `..`/absolute *member paths* — was already
                // rejected by the listing check before extraction.
                continue;
            }
            if meta.is_dir() {
                stack.push(path);
            } else if let Ok(canon) = std::fs::canonicalize(&path)
                && !canon.starts_with(&root_canon)
            {
                return Err(Error::Corrupt(format!(
                    "extracted file escaped {}: {}",
                    root.display(),
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

fn run(tool: &'static str, cmd: &mut Command) -> Result<()> {
    let status = cmd.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::Tool {
            tool,
            code: status.code().unwrap_or(-1),
        })
    }
}

/// Run a command and capture its stdout, failing on a non-zero exit.
fn capture(tool: &'static str, cmd: &mut Command) -> Result<Vec<u8>> {
    let out = cmd.output()?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(Error::Tool {
            tool,
            code: out.status.code().unwrap_or(-1),
        })
    }
}

/// Read the flat VDB-style metadata from a GPKG container's inner
/// `metadata.tar.<c>`.
///
/// Returns a map of *field name → value* for every text member under
/// `metadata/` (binary or non-field members — `environment.bz2`, the copied
/// `<PF>.ebuild` — are skipped). Requires `tar` and `zstd` on `PATH`. This is
/// what [`write_gpkg`] packs into `<basename>/metadata.tar.zst` and what the
/// binhost `Packages` index and the `-k` consumer read back.
pub fn read_metadata(container: &Path) -> Result<BTreeMap<String, String>> {
    let staging = tempfile::Builder::new().prefix("em-gpkg-read-").tempdir()?;
    let root = staging.path().to_path_buf();

    // 1. Locate the inner `metadata.tar.<c>` member in the container. GNU tar
    //    lists members relative to the archive root (`<basename>/metadata.tar.zst`).
    let listing = container_member_listing(container)?;
    let member = find_container_member(&listing, "metadata.tar", container)?;
    let compressed = root.join(member);

    // 2. Extract just that one member.
    run(
        "tar",
        Command::new("tar")
            .arg("-xf")
            .arg(container)
            .arg("-C")
            .arg(&root)
            .arg(member),
    )?;

    // 3. Decompress it to `metadata.tar`. `metadata.tar` is uncompressed for
    //    GPKG, but accept a `.zst`/`.gz` suffix in case BINPKG_COMPRESS differs.
    let metadata_tar: PathBuf = root.join("metadata.tar");
    let bytes = match compressed.extension().and_then(|e| e.to_str()) {
        Some("zst") => capture("zstd", Command::new("zstd").arg("-dc").arg(&compressed))?,
        Some("gz") => capture("gzip", Command::new("gzip").arg("-dc").arg(&compressed))?,
        _ => std::fs::read(&compressed)?,
    };
    std::fs::write(&metadata_tar, bytes)?;

    // 4. Extract `metadata/*` (flat: the writer emits files with no dir entry).
    run(
        "tar",
        Command::new("tar")
            .arg("--no-same-owner")
            .arg("-xf")
            .arg(&metadata_tar)
            .arg("-C")
            .arg(&root),
    )?;

    // 5. Read each field file. Skip binary/large non-field members.
    let mut map = BTreeMap::new();
    let meta_dir = root.join("metadata");
    for entry in std::fs::read_dir(&meta_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "environment.bz2" || name.ends_with(".ebuild") {
            continue;
        }
        let content = std::fs::read_to_string(entry.path())?;
        map.insert(name, content.trim_end_matches('\n').to_string());
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // Build a fake `${D}` + VDB-style metadata dir, pack a gpkg, read the
    // metadata back — verifying the field files survive the round trip.
    #[test]
    fn write_then_read_metadata_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // ${D}: image with a single file and a setuid binary + dir.
        let image = root.join("image");
        fs::create_dir_all(image.join("usr/bin")).unwrap();
        fs::write(image.join("usr/bin/hello"), b"#!/bin/sh\necho hi\n").unwrap();
        fs::write(image.join("usr/bin/mount"), b"\xff\xfe\x00").unwrap();

        // VDB-style metadata dir (flat field files).
        let meta = root.join("vdb/foo-1.0");
        fs::create_dir_all(&meta).unwrap();
        let fields = [
            ("PF", "foo-1.0"),
            ("CATEGORY", "app-test"),
            ("SLOT", "0"),
            ("EAPI", "8"),
            ("USE", "nls -debug"),
            ("DESCRIPTION", "a test package"),
            ("repository", "gentoo"),
            ("BUILD_ID", "1"),
            ("BUILD_TIME", "1700000000"),
            ("SIZE", "42"),
            ("DEPEND", ">=sys-libs/glibc-2.38"),
        ];
        for (k, v) in fields {
            fs::write(meta.join(k), format!("{v}\n")).unwrap();
        }
        // A binary + a copied ebuild must be skipped by the reader.
        fs::write(meta.join("environment.bz2"), b"not real bzip").unwrap();
        fs::write(meta.join("foo-1.0.ebuild"), b"# ebuild body").unwrap();

        let container = root.join("app-test/foo-1.0-1.gpkg.tar");
        fs::create_dir_all(container.parent().unwrap()).unwrap();
        write_gpkg(
            &GpkgInput {
                image_dir: &image,
                metadata_dir: &meta,
                basename: "foo-1.0",
                signing: None,
            },
            &container,
        )
        .unwrap();

        let out = read_metadata(&container).unwrap();
        assert_eq!(out.get("PF").map(String::as_str), Some("foo-1.0"));
        assert_eq!(out.get("CATEGORY").map(String::as_str), Some("app-test"));
        assert_eq!(out.get("SLOT").map(String::as_str), Some("0"));
        assert_eq!(out.get("USE").map(String::as_str), Some("nls -debug"));
        assert_eq!(
            out.get("DEPEND").map(String::as_str),
            Some(">=sys-libs/glibc-2.38")
        );
        assert_eq!(out.get("repository").map(String::as_str), Some("gentoo"));
        assert_eq!(out.get("BUILD_ID").map(String::as_str), Some("1"));
        // The skipped members must not appear.
        assert!(!out.contains_key("environment.bz2"));
        assert!(!out.contains_key("foo-1.0.ebuild"));
    }

    // `extract_image` recovers the image tree with the `image/` prefix stripped
    // and the file contents intact.
    #[test]
    fn extract_image_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        let image = root.join("image");
        fs::create_dir_all(image.join("usr/bin")).unwrap();
        fs::write(image.join("usr/bin/hello"), b"#!/bin/sh\necho hi\n").unwrap();
        fs::create_dir_all(image.join("etc")).unwrap();
        fs::write(image.join("etc/foo.conf"), b"key=value\n").unwrap();

        let meta = root.join("vdb/foo-1.0");
        fs::create_dir_all(&meta).unwrap();
        for (k, v) in [("PF", "foo-1.0"), ("CATEGORY", "app-test"), ("SLOT", "0")] {
            fs::write(meta.join(k), format!("{v}\n")).unwrap();
        }

        let container = root.join("app-test/foo-1.0-1.gpkg.tar");
        fs::create_dir_all(container.parent().unwrap()).unwrap();
        write_gpkg(
            &GpkgInput {
                image_dir: &image,
                metadata_dir: &meta,
                basename: "foo-1.0",
                signing: None,
            },
            &container,
        )
        .unwrap();

        let dest = root.join("merged");
        extract_image(&container, &dest, VerifyPolicy::default()).unwrap();

        // The `image/` prefix is stripped: members land at dest/<path>.
        assert_eq!(
            std::fs::read_to_string(dest.join("usr/bin/hello")).unwrap(),
            "#!/bin/sh\necho hi\n"
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("etc/foo.conf")).unwrap(),
            "key=value\n"
        );
    }

    // Build a fake `${D}` + VDB-style metadata dir under `root`, returning
    // `(image_dir, metadata_dir)` — the shared fixture for the signed
    // round-trip tests below.
    fn build_fixture(root: &Path) -> (PathBuf, PathBuf) {
        let image = root.join("image");
        fs::create_dir_all(image.join("usr/bin")).unwrap();
        fs::write(image.join("usr/bin/hello"), b"#!/bin/sh\necho hi\n").unwrap();
        let meta = root.join("vdb/foo-1.0");
        fs::create_dir_all(&meta).unwrap();
        for (k, v) in [("PF", "foo-1.0"), ("CATEGORY", "app-test"), ("SLOT", "0")] {
            fs::write(meta.join(k), format!("{v}\n")).unwrap();
        }
        (image, meta)
    }

    fn gen_signing_key(root: &Path) -> (gpg::SigningKey, gpg::Keyring) {
        use pgp::composed::{ArmorOptions, KeyType, SecretKeyParamsBuilder};
        use pgp::composed::{SignedPublicKey, SignedSecretKey};

        let mut rng = rand::thread_rng();
        let mut params = SecretKeyParamsBuilder::default();
        params
            .key_type(KeyType::Ed25519Legacy)
            .can_certify(true)
            .can_sign(true)
            .primary_user_id("Test Signer <test@example.com>".into());
        let secret: SignedSecretKey = params
            .build()
            .expect("params")
            .generate(&mut rng)
            .expect("generate key");
        let public: SignedPublicKey = secret.to_public_key();

        let key_path = root.join("signing.asc");
        fs::write(
            &key_path,
            secret.to_armored_bytes(ArmorOptions::default()).unwrap(),
        )
        .unwrap();
        let signing = gpg::SigningKey::load(&key_path, "", gpg::HashAlgorithm::Sha512).unwrap();
        let keyring = gpg::Keyring::new(vec![public]);
        (signing, keyring)
    }

    // A signed GPKG round-trips through `extract_image` when the caller's
    // keyring contains the signing key — the success path for
    // `FEATURES=binpkg-signing` + `binrepos.conf`'s `verify-signature=yes`.
    #[test]
    fn signed_gpkg_extracts_with_the_right_keyring() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let (image, meta) = build_fixture(root);
        let (signing, keyring) = gen_signing_key(root);

        let container = root.join("app-test/foo-1.0-1.gpkg.tar");
        fs::create_dir_all(container.parent().unwrap()).unwrap();
        write_gpkg(
            &GpkgInput {
                image_dir: &image,
                metadata_dir: &meta,
                basename: "foo-1.0",
                signing: Some(&signing),
            },
            &container,
        )
        .unwrap();

        let report = verify_container_signature(
            &container,
            &VerifyPolicy {
                require_signature: true,
                keyring: Some(&keyring),
            },
        )
        .unwrap();
        assert!(report.signed);
        assert_eq!(report.signature_valid, Some(true));
        assert_eq!(
            report.signer_fingerprint.as_deref(),
            Some(signing.fingerprint().as_str())
        );
        assert_eq!(report.member_signatures.len(), 2); // metadata.tar.zst.sig + image.tar.zst.sig
        assert!(
            report
                .member_signatures
                .iter()
                .all(|(_, ok)| *ok == Some(true))
        );

        let dest = root.join("merged");
        extract_image(
            &container,
            &dest,
            VerifyPolicy {
                require_signature: true,
                keyring: Some(&keyring),
            },
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(dest.join("usr/bin/hello")).unwrap(),
            "#!/bin/sh\necho hi\n"
        );
    }

    // A signed GPKG fails cryptographic verification against a keyring that
    // doesn't contain the signing cert — the "unknown signer" case.
    #[test]
    fn signed_gpkg_fails_verification_against_wrong_keyring() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let (image, meta) = build_fixture(root);
        let (signing, _keyring) = gen_signing_key(root);
        let other_root = root.join("other");
        fs::create_dir_all(&other_root).unwrap();
        let (_other_signing, wrong_keyring) = gen_signing_key(&other_root);

        let container = root.join("app-test/foo-1.0-1.gpkg.tar");
        fs::create_dir_all(container.parent().unwrap()).unwrap();
        write_gpkg(
            &GpkgInput {
                image_dir: &image,
                metadata_dir: &meta,
                basename: "foo-1.0",
                signing: Some(&signing),
            },
            &container,
        )
        .unwrap();

        let dest = root.join("merged");
        let err = extract_image(
            &container,
            &dest,
            VerifyPolicy {
                require_signature: false,
                keyring: Some(&wrong_keyring),
            },
        )
        .unwrap_err();
        assert!(matches!(err, Error::Signature(_)));
    }

    // `FEATURES=binpkg-request-signature` (`require_signature: true`)
    // rejects an unsigned container outright, before any extraction.
    #[test]
    fn unsigned_gpkg_rejected_when_signature_required() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let (image, meta) = build_fixture(root);

        let container = root.join("app-test/foo-1.0-1.gpkg.tar");
        fs::create_dir_all(container.parent().unwrap()).unwrap();
        write_gpkg(
            &GpkgInput {
                image_dir: &image,
                metadata_dir: &meta,
                basename: "foo-1.0",
                signing: None,
            },
            &container,
        )
        .unwrap();

        let dest = root.join("merged");
        let err = extract_image(
            &container,
            &dest,
            VerifyPolicy {
                require_signature: true,
                keyring: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, Error::SignatureRequired(_)));
    }

    // An unsigned container still round-trips under the default policy —
    // regression guard for the `GpkgInput`/`extract_image` signature
    // changes across this whole module.
    #[test]
    fn unsigned_gpkg_still_round_trips_under_default_policy() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let (image, meta) = build_fixture(root);

        let container = root.join("app-test/foo-1.0-1.gpkg.tar");
        fs::create_dir_all(container.parent().unwrap()).unwrap();
        write_gpkg(
            &GpkgInput {
                image_dir: &image,
                metadata_dir: &meta,
                basename: "foo-1.0",
                signing: None,
            },
            &container,
        )
        .unwrap();

        let report = verify_container_signature(&container, &VerifyPolicy::default()).unwrap();
        assert!(!report.signed);
        assert_eq!(report.signature_valid, None);

        let dest = root.join("merged");
        extract_image(&container, &dest, VerifyPolicy::default()).unwrap();
        assert_eq!(
            fs::read_to_string(dest.join("usr/bin/hello")).unwrap(),
            "#!/bin/sh\necho hi\n"
        );
    }
}
