use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use camino::{Utf8Path, Utf8PathBuf};
use humansize::{BINARY, format_size};
use portage_vdb::Vdb;

pub fn query_belongs(vdb: &Vdb, files: &[String]) {
    for file_str in files {
        let path = Utf8Path::new(file_str);
        if let Some(pkg) = vdb.owner(path) {
            println!("{}", pkg);
            continue;
        }
        let resolved = resolve_path(file_str);
        if resolved.as_path() != path
            && let Some(pkg) = vdb.owner(&resolved)
        {
            println!("{}", pkg);
            continue;
        }
        eprintln!("no package owns '{}'", file_str);
    }
}

pub fn query_files(vdb: &Vdb, atoms: &[String]) {
    for raw in atoms {
        let matched = find_packages(vdb, raw);
        if matched.is_empty() {
            eprintln!("no installed package matches '{}'", raw);
            continue;
        }
        for pkg in matched {
            match pkg.contents() {
                Ok(entries) => {
                    for entry in entries {
                        if !matches!(entry.kind, portage_vdb::ContentsKind::Dir) {
                            println!("{}\t{}", pkg, entry.path);
                        }
                    }
                }
                Err(e) => eprintln!("{}: {}", pkg, e),
            }
        }
    }
}

pub fn query_size(vdb: &Vdb, atoms: &[String]) -> Result<()> {
    for raw in atoms {
        let matched = find_packages(vdb, raw);
        if matched.is_empty() {
            eprintln!("no installed package matches '{}'", raw);
            continue;
        }
        for pkg in matched {
            print_pkg_size(&pkg)?;
        }
    }
    Ok(())
}

fn print_pkg_size(pkg: &portage_vdb::InstalledPackage) -> Result<()> {
    let size_str = match pkg.size() {
        Ok(Some(bytes)) => format_size(bytes, BINARY),
        Ok(None) => "size unknown".to_string(),
        Err(e) => return Err(anyhow!("{}: {}", pkg, e)),
    };

    let built_str = match pkg.build_time() {
        Ok(Some(ts)) => {
            let t = UNIX_EPOCH + Duration::from_secs(ts);
            format!("  built {}", humantime::format_rfc3339_seconds(t))
        }
        _ => String::new(),
    };

    println!("{}: {}{}", pkg, size_str, built_str);
    Ok(())
}

/// Find installed packages matching `pattern`
///
/// Accepts full Portage atoms (`=cat/pkg-1.2`, `>=cat/pkg-1`, `cat/pkg:2`,
/// …) via [`portage_atom::Dep::matches_cpv`], plus bare package-name / PF
/// convenience forms (`bash`, `bash-5.2`) that are not valid deps on their
/// own. Used by `-C`/`--unmerge` and the query applets.
pub(crate) fn find_packages(vdb: &Vdb, pattern: &str) -> Vec<portage_vdb::InstalledPackage> {
    if let Ok(dep) = portage_atom::Dep::parse(pattern) {
        return vdb
            .packages()
            .into_iter()
            .filter(|p| dep.matches_cpv(p.cpv(), p.slot().ok().as_ref()))
            .collect();
    }
    // Bare package name or PF (`bash`, `bash-5.2`) — not a valid Dep (needs
    // category/), but emerge accepts them as a convenience.
    vdb.packages()
        .into_iter()
        .filter(|p| p.cpn().package.as_ref() == pattern || p.pf() == pattern)
        .collect()
}

/// Open the VDB for a CLI invocation (explicit `--vdb` or the merge root default)
pub(crate) fn open_cli_vdb(globals: &crate::cli::Cli) -> Result<Vdb> {
    let vdb_path = globals.vdb().unwrap_or_else(|| {
        format!(
            "{}/var/db/pkg",
            globals.roots().merge_root().as_str().trim_end_matches('/')
        )
    });
    Vdb::open(vdb_path.as_str()).with_context(|| format!("failed to open VDB at {vdb_path}"))
}

fn resolve_path(path_str: &str) -> Utf8PathBuf {
    match std::fs::canonicalize(path_str) {
        Ok(resolved) => {
            Utf8PathBuf::from_path_buf(resolved).unwrap_or_else(|_| Utf8PathBuf::from(path_str))
        }
        Err(_) => Utf8PathBuf::from(path_str),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_pkg(vdb_root: &std::path::Path, cat: &str, pf: &str, slot: &str) {
        let dir = vdb_root.join(cat).join(pf);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SLOT"), slot).unwrap();
        std::fs::write(dir.join("EAPI"), "8").unwrap();
    }

    fn open(tmp: &std::path::Path) -> Vdb {
        let root = Utf8PathBuf::try_from(tmp.to_owned()).unwrap();
        Vdb::open(root.join("var/db/pkg")).unwrap()
    }

    #[test]
    fn find_packages_matches_versioned_and_slotted_atoms() {
        let tmp = tempfile::tempdir().unwrap();
        let vdb_root = tmp.path().join("var/db/pkg");
        write_pkg(&vdb_root, "app-misc", "foo-1.0", "0");
        write_pkg(&vdb_root, "app-misc", "foo-2.0", "0");
        write_pkg(&vdb_root, "app-misc", "bar-1.0", "2");

        let vdb = open(tmp.path());

        // Old string-heuristic path treated "=app-misc/foo-1.0" as category "=app-misc".
        let exact = find_packages(&vdb, "=app-misc/foo-1.0");
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].cpv().to_string(), "app-misc/foo-1.0");

        let ge = find_packages(&vdb, ">=app-misc/foo-1.5");
        assert_eq!(ge.len(), 1);
        assert_eq!(ge[0].cpv().to_string(), "app-misc/foo-2.0");

        let slotted = find_packages(&vdb, "app-misc/bar:2");
        assert_eq!(slotted.len(), 1);
        assert_eq!(slotted[0].cpv().to_string(), "app-misc/bar-1.0");

        // Bare name convenience still works.
        let bare = find_packages(&vdb, "foo");
        assert_eq!(bare.len(), 2);
    }

    #[test]
    fn slot_atom_matches_an_installed_package_carrying_a_subslot() {
        // `:0` selects the *slot*; a package installed as `0/1` is in slot 0
        // and must match (`qlist -I 'app-arch/zstd:0'` and `equery list` both
        // return the subslotted package). Matching against the raw `SLOT`
        // file compares `"0"` to `"0/1"` and finds nothing — which is what
        // `-C app-arch/zstd:0` and `em query files` used to do for the 104 of
        // this host's 723 installed packages that carry a subslot.
        let tmp = tempfile::tempdir().unwrap();
        let vdb_root = tmp.path().join("var/db/pkg");
        write_pkg(&vdb_root, "app-arch", "zstd-1.5.7", "0/1");
        let vdb = open(tmp.path());

        let by_slot = find_packages(&vdb, "app-arch/zstd:0");
        assert_eq!(
            by_slot.len(),
            1,
            "a `:0` atom must match an installed SLOT of `0/1`"
        );

        // The subslot is still addressable, and a wrong one still misses.
        assert_eq!(find_packages(&vdb, "app-arch/zstd:0/1").len(), 1);
        assert!(find_packages(&vdb, "app-arch/zstd:1").is_empty());
    }

    #[test]
    fn a_wrong_subslot_does_not_match() {
        // `:0/2` names a sub-slot the installed package does not have, so it
        // must miss even though the slot matches — `qlist -I 'app-arch/zstd:0/2'`
        // and `equery list` both return nothing against an installed `0/1`.
        let tmp = tempfile::tempdir().unwrap();
        let vdb_root = tmp.path().join("var/db/pkg");
        write_pkg(&vdb_root, "app-arch", "zstd-1.5.7", "0/1");
        let vdb = open(tmp.path());

        assert!(
            find_packages(&vdb, "app-arch/zstd:0/2").is_empty(),
            "a sub-slot constraint must be honoured when the candidate has one"
        );
        assert_eq!(find_packages(&vdb, "app-arch/zstd:0/1").len(), 1);
    }
}
