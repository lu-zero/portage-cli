use std::cmp::Ordering;

use anyhow::Result;
use portage_atom::{Cpv, Dep, Operator};
use portage_repo::RepoSet;
use portage_vdb::Vdb;

pub fn run(
    set: &RepoSet,
    vdb: Option<&Vdb>,
    mode: super::ResolveMode,
    atoms: &[String],
) -> Result<()> {
    // `set.ebuilds()`, not `set.entries()`: which only ever needs a cpv and
    // a file path (picked by version comparison, which the filename alone
    // already gives), never cache *content* — `entries()` would pay a full
    // bulk parse of every cache file in every repo for data this command
    // never looks at. Shadowed by cpv (`RepoSet::ebuilds`'s own contract),
    // so a cpv present in more than one repo is the highest-priority
    // repo's file — the exact file the merge would actually build.
    let ebuilds: Vec<_> = set.ebuilds()?.collect();

    for raw in atoms {
        let dep = super::resolve_atom(set, vdb, mode, raw)?;

        match best_ebuild_path(&ebuilds, &dep) {
            Some(path) => println!("{path}"),
            None => crate::style::warn_line!("no ebuild found for '{raw}'"),
        }
    }
    Ok(())
}

/// The highest-version ebuild among `ebuilds` matching `dep`
fn best_ebuild_path<'a>(
    ebuilds: &'a [portage_repo::EbuildIn<'a>],
    dep: &Dep,
) -> Option<&'a camino::Utf8Path> {
    ebuilds
        .iter()
        .filter(|item| dep_matches_cpv(dep, item.ebuild.cpv()))
        .max_by(|a, b| a.ebuild.cpv().version.cmp(&b.ebuild.cpv().version))
        .map(|item| item.ebuild.path())
}

pub fn dep_matches_cpv(dep: &Dep, cpv: &Cpv) -> bool {
    if dep.cpn != cpv.cpn {
        return false;
    }
    match (&dep.version, &dep.op) {
        (None, _) => true,
        (Some(v), Some(Operator::Equal)) if dep.glob => cpv.version.glob_matches(v),
        (Some(v), Some(op)) => {
            let ord = cpv.version.cmp(v);
            match op {
                Operator::Less => ord == Ordering::Less,
                Operator::LessOrEqual => ord != Ordering::Greater,
                Operator::Equal => ord == Ordering::Equal,
                Operator::Approximate => {
                    let mut base = v.clone();
                    base.revision = Default::default();
                    let mut cv = cpv.version.clone();
                    cv.revision = Default::default();
                    cv == base
                }
                Operator::GreaterOrEqual => ord != Ordering::Less,
                Operator::Greater => ord == Ordering::Greater,
            }
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use std::sync::Arc;

    use portage_repo::{RepoSet, Repository};

    use super::*;

    fn make_repo(dir: &tempfile::TempDir, cat: &str, pkg: &str, ver: &str) -> Repository {
        std::fs::create_dir_all(dir.path().join("metadata")).unwrap();
        std::fs::write(dir.path().join("metadata").join("layout.conf"), "").unwrap();
        std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
        std::fs::write(dir.path().join("profiles").join("categories"), cat).unwrap();
        let pkg_dir = dir.path().join(cat).join(pkg);
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(
            pkg_dir.join(format!("{pkg}-{ver}.ebuild")),
            "EAPI=8\nSLOT=0\n",
        )
        .unwrap();
        Repository::builder()
            .in_memory_cache()
            .open(dir.path())
            .unwrap()
    }

    // Before `which::run` scanned `set.ebuilds()` (every repo, shadowed by
    // cpv) instead of just `set.main()`, an atom that resolved to an
    // overlay-only package still failed the file lookup — `resolve_atom`
    // found the name, but the ebuild scan never looked outside main.
    #[test]
    fn finds_an_overlay_only_ebuild() {
        let main_dir = tempfile::tempdir().unwrap();
        let overlay_dir = tempfile::tempdir().unwrap();
        let main = make_repo(&main_dir, "sys-apps", "unrelated", "1.0");
        let overlay = make_repo(&overlay_dir, "app-misc", "bar", "2.0");

        let set = RepoSet::from_ordered(vec![Arc::new(main), Arc::new(overlay)], 0, Vec::new());
        let ebuilds: Vec<_> = set.ebuilds().unwrap().collect();

        let dep = Dep::from_str("app-misc/bar").unwrap();
        let path = best_ebuild_path(&ebuilds, &dep).expect("overlay-only ebuild found");
        assert!(path.as_str().contains("bar-2.0.ebuild"));
    }

    // A cpv present in both main and an overlay is shadowed by
    // `RepoSet::ebuilds` down to the higher-priority repo's copy — `which`
    // must report that file, the one the merge would actually build.
    #[test]
    fn a_duplicate_cpv_reports_the_higher_priority_repos_file() {
        let main_dir = tempfile::tempdir().unwrap();
        let overlay_dir = tempfile::tempdir().unwrap();
        let main = make_repo(&main_dir, "sys-apps", "foo", "1.0");
        let overlay = make_repo(&overlay_dir, "sys-apps", "foo", "1.0");
        let overlay_path_prefix = overlay.path().to_owned();

        // Overlay ranked above main (index 0).
        let set = RepoSet::from_ordered(vec![Arc::new(overlay), Arc::new(main)], 1, Vec::new());
        let ebuilds: Vec<_> = set.ebuilds().unwrap().collect();

        let dep = Dep::from_str("sys-apps/foo").unwrap();
        let path = best_ebuild_path(&ebuilds, &dep).expect("ebuild found");
        assert!(path.starts_with(&overlay_path_prefix));
    }
}
