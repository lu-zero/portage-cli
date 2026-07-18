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

    // 4. Manifest — checksums of the three members above (Manifest excludes itself).
    let manifest = pkg_dir.join("Manifest");
    write_manifest(
        &manifest,
        &[
            (GPKG_VERSION, &gpkg1),
            (METADATA_TAR, &metadata),
            (IMAGE_TAR, &image),
        ],
    )?;

    // Container: a plain tar, members added in the required order (gpkg-1 first,
    // Manifest last), forced to 0/0.
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let b = input.basename;
    run(
        "tar",
        Command::new("tar")
            .arg("--numeric-owner")
            .arg("--owner=0")
            .arg("--group=0")
            .arg("--format=ustar")
            .arg("-C")
            .arg(staging.path())
            .arg("-cf")
            .arg(out_path)
            .arg(format!("{b}/{GPKG_VERSION}"))
            .arg(format!("{b}/{METADATA_TAR}"))
            .arg(format!("{b}/{IMAGE_TAR}"))
            .arg(format!("{b}/Manifest")),
    )
}

/// `tar --zstd` the whole *tree* under `dir` into `out`, renaming the root to
/// `prefix` (so members are `prefix/...`, directory entries included). With
/// `xattrs`, file capabilities, ACLs and device nodes are preserved (pax format).
fn tar_tree(dir: &Path, prefix: &str, out: &Path, xattrs: bool) -> Result<()> {
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

/// Write a GLEP 74-style Manifest with one `DATA` line per member.
fn write_manifest(out: &Path, members: &[(&str, &std::path::PathBuf)]) -> Result<()> {
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

/// Extract the GPKG container's installed image into `dest` (e.g. `${D}` or a
/// merge `work_root/image`), stripping the inner `image/` prefix so members land
/// at `dest/<path>` (e.g. `dest/usr/bin/foo`). Used by the `-k`/`--usepkg`
/// consumer to merge a pre-built package without compiling. Requires `tar` and
/// `zstd` on `PATH`.
pub fn extract_image(container: &Path, dest: &Path) -> Result<()> {
    let staging = tempfile::Builder::new().prefix("em-gpkg-img-").tempdir()?;
    let root = staging.path().to_path_buf();

    // Locate the inner `image.tar.<c>` member.
    let listing = String::from_utf8_lossy(&capture(
        "tar",
        Command::new("tar").arg("-tf").arg(container),
    )?)
    .into_owned();
    let member = listing
        .lines()
        .map(|l| l.trim_end_matches('/'))
        .find(|m| {
            let b = Path::new(m)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            b.starts_with("image.tar")
        })
        .ok_or_else(|| {
            Error::Corrupt(format!("no image.tar.* member in {}", container.display()))
        })?;
    let compressed = root.join(member);
    run(
        "tar",
        Command::new("tar")
            .arg("-xf")
            .arg(container)
            .arg("-C")
            .arg(&root)
            .arg(member),
    )?;

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
        let read = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for entry in read.flatten() {
            let path = entry.path();
            // Use the path as-is (no follow) for the containment check of the
            // directory entry itself; canonicalize only non-symlinks so a
            // symlink pointing outside is still allowed *as a symlink* (Portage
            // images contain absolute target symlinks) but its *location* must
            // stay under root.
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.file_type().is_symlink() {
                // Symlink *path* must live under root; target may be absolute.
                if !path.starts_with(root) && !path.starts_with(&root_canon) {
                    return Err(Error::Corrupt(format!(
                        "extracted symlink escaped {}: {}",
                        root.display(),
                        path.display()
                    )));
                }
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
    let listing = String::from_utf8_lossy(&capture(
        "tar",
        Command::new("tar").arg("-tf").arg(container),
    )?)
    .into_owned();
    let member = listing
        .lines()
        .map(|l| l.trim_end_matches('/'))
        .find(|m| {
            let b = Path::new(m)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            b.starts_with("metadata.tar")
        })
        .ok_or_else(|| {
            Error::Corrupt(format!(
                "no metadata.tar.* member in {}",
                container.display()
            ))
        })?;
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

    /// Build a fake `${D}` + VDB-style metadata dir, pack a gpkg, read the
    /// metadata back — verifying the field files survive the round trip.
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

    /// `extract_image` recovers the image tree with the `image/` prefix stripped
    /// and the file contents intact.
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
            },
            &container,
        )
        .unwrap();

        let dest = root.join("merged");
        extract_image(&container, &dest).unwrap();

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
}
