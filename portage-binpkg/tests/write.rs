use std::process::Command;

use portage_binpkg::{GpkgInput, write_gpkg};

fn tar_list(args: &[&std::ffi::OsStr]) -> String {
    let out = Command::new("tar").args(args).output().unwrap();
    assert!(out.status.success(), "tar {args:?} failed");
    String::from_utf8(out.stdout).unwrap()
}

/// Empty/missing `${D}` (virtual packages under EPREFIX never create `ED`)
/// must still produce a valid GPKG — not `tar: Cannot open`.
#[test]
fn writes_gpkg_with_missing_image_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let image = tmp.path().join("image/root/xp"); // nested ED path, never created
    let meta = tmp.path().join("vdb");
    std::fs::create_dir_all(&meta).unwrap();
    std::fs::write(meta.join("PF"), "virtual-acl-0\n").unwrap();
    std::fs::write(meta.join("CATEGORY"), "virtual\n").unwrap();
    std::fs::write(meta.join("SLOT"), "0\n").unwrap();
    std::fs::write(meta.join("CONTENTS"), "").unwrap();

    let out = tmp.path().join("virtual-acl-0-1.gpkg.tar");
    write_gpkg(
        &GpkgInput {
            image_dir: &image,
            metadata_dir: &meta,
            basename: "virtual-acl-0",
            signing: None,
        },
        &out,
    )
    .expect("empty/missing image must not fail --buildpkg");
    assert!(out.is_file());
    assert!(image.is_dir(), "tar_tree should create empty image dir");
}

#[test]
fn writes_a_valid_gpkg_container() {
    let tmp = tempfile::tempdir().unwrap();

    // Synthetic ${D}.
    let image = tmp.path().join("image");
    std::fs::create_dir_all(image.join("usr/bin")).unwrap();
    std::fs::write(image.join("usr/bin/hello"), b"#!/bin/sh\necho hi\n").unwrap();

    // Synthetic VDB metadata dir.
    let meta = tmp.path().join("vdb");
    std::fs::create_dir_all(&meta).unwrap();
    for (f, v) in [
        ("PF", "hello-1.0"),
        ("CATEGORY", "app-misc"),
        ("SLOT", "0"),
        ("CONTENTS", "obj /usr/bin/hello d41d8cd 123\n"),
    ] {
        std::fs::write(meta.join(f), format!("{v}\n")).unwrap();
    }

    let out = tmp.path().join("hello-1.0-1.gpkg.tar");
    write_gpkg(
        &GpkgInput {
            image_dir: &image,
            metadata_dir: &meta,
            basename: "hello-1.0",
            signing: None,
        },
        &out,
    )
    .unwrap();

    // Members present, in the required order.
    let names: Vec<String> = tar_list(&["-tf".as_ref(), out.as_os_str()])
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        names,
        [
            "hello-1.0/gpkg-1",
            "hello-1.0/metadata.tar.zst",
            "hello-1.0/image.tar.zst",
            "hello-1.0/Manifest",
        ]
    );

    // Every member owned 0/0.
    let verbose = tar_list(&["-tvf".as_ref(), out.as_os_str()]);
    assert!(
        verbose
            .lines()
            .all(|l| l.contains("0/0") || l.contains("root/root")),
        "members not all 0/0:\n{verbose}"
    );

    // Unpack the container and inspect the inner tars.
    let x = tmp.path().join("x");
    std::fs::create_dir_all(&x).unwrap();
    assert!(
        Command::new("tar")
            .args([
                "-xf".as_ref(),
                out.as_os_str(),
                "-C".as_ref(),
                x.as_os_str()
            ])
            .status()
            .unwrap()
            .success()
    );

    let img = tar_list(&[
        "--zstd".as_ref(),
        "-tf".as_ref(),
        x.join("hello-1.0/image.tar.zst").as_os_str(),
    ]);
    assert!(img.contains("image/usr/bin/hello"), "image.tar:\n{img}");

    let md = tar_list(&[
        "--zstd".as_ref(),
        "-tf".as_ref(),
        x.join("hello-1.0/metadata.tar.zst").as_os_str(),
    ]);
    assert!(md.contains("metadata/PF"), "metadata.tar:\n{md}");
    assert!(md.contains("metadata/CONTENTS"), "metadata.tar:\n{md}");

    // Manifest: one DATA line per member (excludes itself), gpkg-1 is the 0-byte
    // file with the well-known empty SHA512.
    let manifest = std::fs::read_to_string(x.join("hello-1.0/Manifest")).unwrap();
    assert_eq!(manifest.lines().count(), 3, "manifest:\n{manifest}");
    assert!(
        manifest.starts_with(
            "DATA gpkg-1 0 SHA512 cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e BLAKE2B"
        ),
        "manifest:\n{manifest}"
    );
    assert!(
        manifest.contains("DATA image.tar.zst "),
        "manifest:\n{manifest}"
    );
}
