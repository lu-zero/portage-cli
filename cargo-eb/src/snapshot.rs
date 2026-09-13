//! Source snapshot tarball of a package (or its isolated workspace).

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

fn skip_name(name: &str) -> bool {
    matches!(name, "target" | ".git" | "cargo_home" | "dist" | ".cargo")
}

fn append_filtered(
    tar: &mut tar::Builder<xz2::write::XzEncoder<fs::File>>,
    src: &Path,
    dest: &Path,
) -> Result<()> {
    let src = if src.is_symlink() {
        fs::canonicalize(src).with_context(|| format!("canonicalize {}", src.display()))?
    } else {
        src.to_path_buf()
    };
    for entry in fs::read_dir(&src).with_context(|| format!("read {}", src.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if skip_name(&name_str) {
            continue;
        }
        let path = entry.path();
        let dest_child = dest.join(&name);
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() || meta.is_symlink() {
            let resolved = if meta.is_symlink() {
                fs::canonicalize(&path)
                    .with_context(|| format!("canonicalize {}", path.display()))?
            } else {
                path.clone()
            };
            if resolved.is_dir() {
                append_filtered(tar, &resolved, &dest_child)?;
            } else {
                tar.append_path_with_name(&resolved, &dest_child)
                    .with_context(|| format!("append {}", resolved.display()))?;
            }
        } else {
            tar.append_path_with_name(&path, &dest_child)
                .with_context(|| format!("append {}", path.display()))?;
        }
    }
    Ok(())
}

/// Pack `root` as `{prefix}/…` into an xz tarball. Symlinks are followed so an
/// isolated workspace of member links becomes a real tree.
pub fn pack_source_tarball(root: &Path, prefix: &str, out: &Path) -> Result<()> {
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).ok();
    }
    let out_file = fs::File::create(out).with_context(|| format!("create {}", out.display()))?;
    let enc = xz2::write::XzEncoder::new(out_file, 9);
    let mut tar = tar::Builder::new(enc);
    tar.mode(tar::HeaderMode::Deterministic);
    append_filtered(&mut tar, root, Path::new(prefix))?;
    let enc = tar.into_inner()?;
    enc.finish().context("xz finish")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_follows_symlink_and_skips_target() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        fs::create_dir(&real).unwrap();
        fs::write(real.join("Cargo.toml"), "[package]\nname = \"t\"\n").unwrap();
        fs::create_dir(real.join("target")).unwrap();
        fs::write(real.join("target").join("junk"), "no").unwrap();
        let root = tmp.path().join("root");
        fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(&real, root.join("pkg")).unwrap();
        let out = tmp.path().join("src.tar.xz");
        pack_source_tarball(&root, "t-1.0.0", &out).unwrap();
        let f = fs::File::open(&out).unwrap();
        let dec = xz2::read::XzDecoder::new(f);
        let mut archive = tar::Archive::new(dec);
        let names: Vec<String> = archive
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.iter().any(|n| n == "t-1.0.0/pkg/Cargo.toml"),
            "{names:?}"
        );
        assert!(names.iter().all(|n| !n.contains("/target/")), "{names:?}");
    }
}
