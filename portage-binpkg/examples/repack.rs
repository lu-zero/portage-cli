//! Build a GPKG from an unpacked image dir + metadata dir.
//!
//! Usage: `repack <image_dir> <metadata_dir> <basename> <out.gpkg.tar>`
//! (`image_dir`/`metadata_dir` hold the bare contents that go under
//! `image/` / `metadata/`). Handy for validating the writer against a real
//! Portage gpkg: unpack one, then re-pack it and feed it back to portage.

use std::path::Path;

use portage_binpkg::{GpkgInput, write_gpkg};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 5 {
        eprintln!("usage: repack <image_dir> <metadata_dir> <basename> <out.gpkg.tar>");
        std::process::exit(2);
    }
    write_gpkg(
        &GpkgInput {
            image_dir: Path::new(&a[1]),
            metadata_dir: Path::new(&a[2]),
            basename: &a[3],
            signing: None,
        },
        Path::new(&a[4]),
    )
    .expect("write_gpkg");
}
