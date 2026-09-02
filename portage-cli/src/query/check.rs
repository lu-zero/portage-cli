//! `em query check` — verify checksums and mtimes of installed package files

use anyhow::{Context, Result};
use portage_vdb::{ContentsKind, InstalledPackage, Vdb};

use crate::vdb::find_packages;

pub fn run(vdb: &Vdb, atoms: &[String]) -> Result<()> {
    for raw in atoms {
        let matched = find_packages(vdb, raw);
        if matched.is_empty() {
            crate::style::warn_line!("no installed package matches '{raw}'");
            continue;
        }
        for pkg in matched {
            check_package(&pkg)?;
        }
    }
    Ok(())
}

fn check_package(pkg: &InstalledPackage) -> Result<()> {
    let entries = pkg.contents().with_context(|| format!("{pkg}"))?;

    let mut ok: u32 = 0;
    let mut fail: u32 = 0;

    for entry in &entries {
        match &entry.kind {
            ContentsKind::Obj => {
                let path = &entry.path;
                match std::fs::read(path.as_std_path()) {
                    Ok(data) => {
                        let digest = format!("{:x}", md5::compute(&data));
                        if entry.md5.as_deref() == Some(digest.as_str()) {
                            ok += 1;
                        } else {
                            eprintln!("  !!! md5  FAIL  {path}");
                            fail += 1;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        eprintln!("  !!! obj  MISS  {path}");
                        fail += 1;
                    }
                    Err(e) => {
                        eprintln!("  !!! obj  ERR   {path}: {e}");
                        fail += 1;
                    }
                }
            }
            ContentsKind::Sym => {
                let path = &entry.path;
                match path.as_std_path().symlink_metadata() {
                    Ok(meta) => {
                        if let Some(expected) = entry.mtime {
                            let actual = meta
                                .modified()
                                .ok()
                                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                .map(|d| d.as_secs());
                            if actual != Some(expected) {
                                eprintln!("  !!! sym  MTIME {path}");
                                fail += 1;
                            } else {
                                ok += 1;
                            }
                        } else {
                            ok += 1;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        eprintln!("  !!! sym  MISS  {path}");
                        fail += 1;
                    }
                    Err(e) => {
                        eprintln!("  !!! sym  ERR   {path}: {e}");
                        fail += 1;
                    }
                }
            }
            ContentsKind::Dir | ContentsKind::Fifo | ContentsKind::Dev => {}
        }
    }

    if fail == 0 {
        println!("{pkg}: {ok} files OK");
    } else {
        println!("{pkg}: {ok} files OK, {fail} failures");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn make_vdb_pkg(
        dir: &std::path::Path,
        cat: &str,
        pf: &str,
        fields: &[(&str, &str)],
    ) -> portage_vdb::Vdb {
        let pkg_dir = dir.join(cat).join(pf);
        fs::create_dir_all(&pkg_dir).unwrap();
        for (name, val) in fields {
            fs::write(pkg_dir.join(name), val).unwrap();
        }
        let root: camino::Utf8PathBuf = dir.to_path_buf().try_into().unwrap();
        portage_vdb::Vdb::open(root).unwrap()
    }

    #[test]
    fn check_passes_for_correct_obj() {
        let tmp = tempdir().unwrap();

        let file_dir = tmp.path().join("actual");
        fs::create_dir_all(&file_dir).unwrap();
        let file_path = file_dir.join("hello.txt");
        fs::write(&file_path, b"hello\n").unwrap();

        let digest = format!("{:x}", md5::compute(b"hello\n"));
        let mtime = file_path
            .metadata()
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let contents = format!("obj {} {} {}\n", file_path.display(), digest, mtime);
        let vdb = make_vdb_pkg(
            tmp.path(),
            "app-shells",
            "bash-5.3",
            &[("CONTENTS", &contents)],
        );

        let result = run(&vdb, &["app-shells/bash-5.3".to_string()]);
        assert!(result.is_ok());
    }
}
