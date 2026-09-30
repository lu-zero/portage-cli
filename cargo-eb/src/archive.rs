//! Reproducible `.tar.xz` distfiles: sorted entries, fixed metadata.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// A `.tar.xz` being written with deterministic headers.
pub(crate) struct TarXz(tar::Builder<xz2::write::XzEncoder<fs::File>>);

impl TarXz {
    /// Start writing `out`, creating its parent directory.
    pub(crate) fn create(out: &Path) -> Result<Self> {
        if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let file = fs::File::create(out).with_context(|| format!("creating {}", out.display()))?;
        let mut tar = tar::Builder::new(xz2::write::XzEncoder::new(file, 9));
        tar.mode(tar::HeaderMode::Deterministic);
        Ok(Self(tar))
    }

    /// Add `src`'s contents under `dest`, following symlinks. `skip(dir, name)` drops
    /// the entry `name` found while reading `dir`.
    pub(crate) fn append_tree(
        &mut self,
        src: &Path,
        dest: &Path,
        skip: &dyn Fn(&Path, &OsStr) -> bool,
    ) -> Result<()> {
        self.append_dir(src, dest, skip, &mut HashSet::new())
    }

    fn append_dir(
        &mut self,
        src: &Path,
        dest: &Path,
        skip: &dyn Fn(&Path, &OsStr) -> bool,
        ancestors: &mut HashSet<PathBuf>,
    ) -> Result<()> {
        let real = fs::canonicalize(src).with_context(|| format!("resolving {}", src.display()))?;
        if !ancestors.insert(real.clone()) {
            bail!("symlink loop at {}", src.display());
        }
        let mut names = fs::read_dir(&real)
            .with_context(|| format!("reading {}", real.display()))?
            .map(|e| e.map(|e| e.file_name()))
            .collect::<std::io::Result<Vec<_>>>()?;
        names.sort();
        for name in names {
            if skip(&real, &name) {
                continue;
            }
            let path = real.join(&name);
            let dest = dest.join(&name);
            if path.is_dir() {
                self.0.append_dir(&dest, &path)?;
                self.append_dir(&path, &dest, skip, ancestors)?;
            } else {
                self.0
                    .append_path_with_name(&path, &dest)
                    .with_context(|| format!("adding {}", path.display()))?;
            }
        }
        ancestors.remove(&real);
        Ok(())
    }

    /// Add a regular file holding `data` at `dest`.
    pub(crate) fn append_data(&mut self, dest: &Path, data: &[u8]) -> Result<()> {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        self.0.append_data(&mut header, dest, data)?;
        Ok(())
    }

    /// Flush the archive and the xz stream.
    pub(crate) fn finish(self) -> Result<()> {
        self.0
            .into_inner()?
            .finish()
            .context("finishing xz stream")?;
        Ok(())
    }
}

/// Build products and local state that do not belong in a source distfile:
/// `.git` anywhere, `target` next to a `Cargo.toml`, and root-level cargo state.
fn skip_in_snapshot(root: &Path, dir: &Path, name: &OsStr) -> bool {
    match name.to_str() {
        Some(".git") => true,
        Some("target") => dir.join("Cargo.toml").is_file(),
        Some(".cargo" | "cargo_home") => dir == root,
        _ => false,
    }
}

/// Pack `root` as `{prefix}/…` into an xz tarball, following symlinks so an
/// isolated workspace of member links becomes a real tree.
pub fn pack_source_tarball(root: &Path, prefix: &str, out: &Path) -> Result<()> {
    let root = fs::canonicalize(root).with_context(|| format!("resolving {}", root.display()))?;
    let mut tar = TarXz::create(out)?;
    tar.append_tree(&root, Path::new(prefix), &|dir, name| {
        skip_in_snapshot(&root, dir, name)
    })?;
    tar.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(out: &Path) -> Vec<String> {
        let dec = xz2::read::XzDecoder::new(fs::File::open(out).unwrap());
        tar::Archive::new(dec)
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn pack_follows_symlink_and_skips_build_dirs_only() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        fs::create_dir_all(real.join("target")).unwrap();
        fs::create_dir_all(real.join("src/target")).unwrap();
        fs::write(real.join("Cargo.toml"), "[package]\nname = \"t\"\n").unwrap();
        fs::write(real.join("target/junk"), "no").unwrap();
        fs::write(real.join("src/target/mod.rs"), "").unwrap();
        let root = tmp.path().join("root");
        fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(&real, root.join("pkg")).unwrap();
        let out = tmp.path().join("src.tar.xz");
        pack_source_tarball(&root, "t-1.0.0", &out).unwrap();
        let names = names(&out);
        assert!(
            names.contains(&"t-1.0.0/pkg/Cargo.toml".into()),
            "{names:?}"
        );
        assert!(
            names.contains(&"t-1.0.0/pkg/src/target/mod.rs".into()),
            "{names:?}"
        );
        assert!(!names.iter().any(|n| n.contains("junk")), "{names:?}");
    }

    #[test]
    fn pack_rejects_symlink_loop() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(&root, root.join("self")).unwrap();
        let err = pack_source_tarball(&root, "t", &tmp.path().join("o.tar.xz")).unwrap_err();
        assert!(err.to_string().contains("symlink loop"), "{err}");
    }
}
