use std::collections::BTreeSet;

use anyhow::Result;
use futures_util::StreamExt;
use portage_atom::{Dep, DepEntry};
use portage_repo::RepoSet;
use portage_vdb::Vdb;

pub async fn run(
    set: &RepoSet,
    vdb: Option<&Vdb>,
    mode: super::ResolveMode,
    atoms: &[String],
) -> Result<()> {
    // Metadata-cache-backed (the same fast path `resolve`/`load_repos` get),
    // not a per-ebuild `Repository::cache_entry` lookup. Collected once
    // since every atom in `atoms` re-scans the same data.
    let entries: Vec<_> = set.entries().collect().await;

    for raw in atoms {
        let target = super::resolve_atom(set, vdb, mode, raw)?;
        let matches = reverse_deps(&entries, &target);

        if atoms.len() > 1 {
            println!("[{raw}]");
        }
        for cpn in &matches {
            println!("{cpn}");
        }
    }
    Ok(())
}

/// Every cpn across `entries` (every repo, not just main) whose DEPEND/
/// RDEPEND/BDEPEND/PDEPEND/IDEPEND names `target`.
fn reverse_deps(entries: &[portage_repo::EntryIn<'_>], target: &Dep) -> BTreeSet<String> {
    let mut matches: BTreeSet<String> = BTreeSet::new();
    for item in entries {
        let m = &item.entry.metadata;
        let dep_trees = [&m.depend, &m.rdepend, &m.bdepend, &m.pdepend, &m.idepend];

        if dep_trees.iter().any(|tree| tree_contains(target, tree)) {
            matches.insert(item.cpv.cpn.to_string());
        }
    }
    matches
}

fn tree_contains(target: &Dep, entries: &[DepEntry]) -> bool {
    entries.iter().any(|e| entry_matches(target, e))
}

fn entry_matches(target: &Dep, entry: &DepEntry) -> bool {
    match entry {
        DepEntry::Atom(dep) => dep.blocker.is_none() && dep.cpn == target.cpn,
        DepEntry::UseConditional { children, .. }
        | DepEntry::AllOf(children)
        | DepEntry::AnyOf(children)
        | DepEntry::ExactlyOneOf(children)
        | DepEntry::AtMostOneOf(children) => tree_contains(target, children),
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use std::sync::Arc;

    use portage_repo::{RepoSet, Repository};

    use super::*;

    /// A repo with one package, whose cache entry's RDEPEND names `rdepend_on`
    /// (e.g. `"sys-apps/foo"`), or no RDEPEND at all when `None`.
    fn make_repo(
        dir: &tempfile::TempDir,
        cat: &str,
        pkg: &str,
        ver: &str,
        rdepend_on: Option<&str>,
    ) -> Repository {
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

        let cache_dir = dir.path().join("metadata").join("md5-cache").join(cat);
        std::fs::create_dir_all(&cache_dir).unwrap();
        let rdepend = rdepend_on.unwrap_or("");
        std::fs::write(
            cache_dir.join(format!("{pkg}-{ver}")),
            format!("EAPI=8\nDESCRIPTION=t\nSLOT=0\nRDEPEND={rdepend}\n"),
        )
        .unwrap();

        Repository::builder()
            .in_memory_cache()
            .open(dir.path())
            .unwrap()
    }

    /// Before `depends::run` scanned `set.entries()` (every repo) instead of
    /// just `set.main()`, a reverse-dependency living only in an overlay
    /// (guru, crossdev, a local overlay) was invisible to `em query depends`.
    #[tokio::test]
    async fn finds_a_reverse_dependency_that_lives_only_in_an_overlay() {
        let main_dir = tempfile::tempdir().unwrap();
        let overlay_dir = tempfile::tempdir().unwrap();
        let main = make_repo(&main_dir, "sys-apps", "foo", "1.0", None);
        let overlay = make_repo(&overlay_dir, "app-misc", "bar", "2.0", Some("sys-apps/foo"));

        let set = RepoSet::from_ordered(vec![Arc::new(main), Arc::new(overlay)], 0, Vec::new());
        let entries: Vec<_> = set.entries().collect().await;

        let target = Dep::from_str("sys-apps/foo").unwrap();
        let matches = reverse_deps(&entries, &target);
        assert!(
            matches.contains("app-misc/bar"),
            "overlay-only reverse dependency must be found: {matches:?}"
        );
    }

    #[tokio::test]
    async fn a_package_with_no_matching_dependents_finds_nothing() {
        let main_dir = tempfile::tempdir().unwrap();
        let overlay_dir = tempfile::tempdir().unwrap();
        let main = make_repo(&main_dir, "sys-apps", "foo", "1.0", None);
        let overlay = make_repo(&overlay_dir, "app-misc", "bar", "2.0", None);

        let set = RepoSet::from_ordered(vec![Arc::new(main), Arc::new(overlay)], 0, Vec::new());
        let entries: Vec<_> = set.entries().collect().await;

        let target = Dep::from_str("sys-apps/foo").unwrap();
        let matches = reverse_deps(&entries, &target);
        assert!(matches.is_empty());
    }
}
