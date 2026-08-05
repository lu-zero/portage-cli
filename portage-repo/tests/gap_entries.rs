//! `gap_entries` covers the ebuilds an in-tree cache misses.
//!
//! A version with no cache entry is invisible rather than merely uncached:
//! `RepoData` is built from cache entries alone, so the version can never be
//! selected and the atom reports as "no ebuilds".

use std::collections::HashSet;

use portage_atom::Cpv;
use portage_repo::Repository;
use tempfile::TempDir;

/// A repo holding one ebuild, with no metadata cache.
fn repo_with_one_ebuild() -> (TempDir, Repository) {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("metadata")).expect("metadata dir");
    std::fs::write(root.join("metadata/layout.conf"), "masters =\n").expect("layout.conf");
    std::fs::create_dir_all(root.join("profiles")).expect("profiles dir");
    std::fs::write(root.join("profiles/repo_name"), "test-repo\n").expect("repo_name");
    std::fs::write(root.join("profiles/categories"), "sys-libs\n").expect("categories");
    std::fs::create_dir_all(root.join("sys-libs/zlib")).expect("pkg dir");
    std::fs::write(
        root.join("sys-libs/zlib/zlib-1.0.ebuild"),
        "EAPI=8\nDESCRIPTION=\"t\"\nSLOT=\"0\"\n",
    )
    .expect("ebuild");
    let repo = Repository::builder()
        .in_memory_cache()
        .open(root)
        .expect("open repo");
    (tmp, repo)
}

fn ebuilds(repo: &Repository) -> Vec<portage_repo::Ebuild> {
    repo.ebuilds_with_masters(&[])
        .expect("walk ebuilds")
        .into_iter()
        .collect()
}

/// The hot path: every ebuild already has an entry, so nothing is sourced.
/// This must stay allocation-cheap and must never start an ebuild shell —
/// it runs on every resolve.
#[tokio::test]
async fn a_fully_covered_tree_yields_nothing() {
    let (_tmp, repo) = repo_with_one_ebuild();
    let all = ebuilds(&repo);
    assert_eq!(all.len(), 1, "fixture should hold exactly one ebuild");
    let covered: HashSet<Cpv> = all.iter().map(|e| e.cpv().clone()).collect();

    let got = portage_repo::gap_entries(&repo, &[], all, &covered).await;

    assert!(got.is_empty(), "nothing is missing, so nothing to source");
}

/// An empty ebuild list is the "tree could not be walked" case: report no gap
/// rather than treating every cached CPV as missing.
#[tokio::test]
async fn no_ebuilds_yields_nothing() {
    let (_tmp, repo) = repo_with_one_ebuild();
    let got = portage_repo::gap_entries(&repo, &[], Vec::new(), &HashSet::new()).await;
    assert!(got.is_empty());
}

/// The uncovered ebuild is sourced and returned, so the version becomes
/// selectable instead of vanishing.
#[tokio::test]
async fn an_uncovered_ebuild_is_sourced() {
    let (_tmp, repo) = repo_with_one_ebuild();
    let all = ebuilds(&repo);

    let got = portage_repo::gap_entries(&repo, &[], all, &HashSet::new()).await;

    assert_eq!(got.len(), 1, "the uncached ebuild should be recovered");
    assert_eq!(got[0].0.to_string(), "sys-libs/zlib-1.0");
    assert_eq!(got[0].1.metadata.slot.slot.as_str(), "0");
}

/// The sidecar is keyed on a sync stamp so an unchanged tree can skip the walk.
/// An in-memory secondary has nowhere to keep it, which must not panic —
/// tests and ephemeral repos simply recompute.
#[test]
fn an_in_memory_secondary_has_no_sidecar() {
    let (_tmp, repo) = repo_with_one_ebuild();
    assert!(repo.sidecar_path("gap-index").is_none());
}

/// The stamp must move when the tree is synced, or a stale index would keep
/// hiding packages. Touching the repo directory stands in for a sync.
#[test]
fn the_sync_stamp_follows_the_tree() {
    let (tmp, repo) = repo_with_one_ebuild();
    let before = repo.sync_stamp().expect("a readable tree has a stamp");

    // Second-resolution mtime: force a distinct value rather than sleeping.
    let marker = tmp.path().join("metadata/timestamp.chk");
    std::fs::write(&marker, "1\n").expect("write marker");
    let after = repo.sync_stamp().expect("stamp after sync");

    assert_ne!(before, after, "a synced tree must invalidate the index");
}
