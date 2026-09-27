//! `alias_entries` sources alias cpvs under their own category and keeps
//! the result in the alias's store, revalidated against the real ebuild.

use camino::Utf8PathBuf;
use portage_atom::Cpv;
use portage_repo::{Ebuild, MemoryMetadataCache, MetadataCache, Repository};
use tempfile::TempDir;

const EBUILD: &str = "EAPI=8\nDESCRIPTION=\"${CATEGORY}\"\nSLOT=\"0\"\n";

fn repo_with_musl() -> (TempDir, Repository, Utf8PathBuf) {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("metadata")).expect("metadata dir");
    std::fs::write(root.join("metadata/layout.conf"), "masters =\n").expect("layout.conf");
    std::fs::create_dir_all(root.join("profiles")).expect("profiles dir");
    std::fs::write(root.join("profiles/repo_name"), "test-repo\n").expect("repo_name");
    std::fs::write(root.join("profiles/categories"), "sys-libs\n").expect("categories");
    std::fs::create_dir_all(root.join("sys-libs/musl")).expect("pkg dir");
    let path =
        Utf8PathBuf::from_path_buf(root.join("sys-libs/musl/musl-1.0.ebuild")).expect("utf8");
    std::fs::write(&path, EBUILD).expect("ebuild");
    let repo = Repository::builder()
        .in_memory_cache()
        .open(root)
        .expect("open repo");
    (tmp, repo, path)
}

async fn description(repo: &Repository, store: &dyn MetadataCache, path: &Utf8PathBuf) -> String {
    let cpv = Cpv::parse("cross-aarch64-unknown-linux-musl/musl-1.0").expect("cpv");
    let got = portage_repo::alias_entries(repo, store, vec![Ebuild::with_cpv(cpv, path)]).await;
    assert_eq!(got.len(), 1);
    got[0].1.metadata.description.clone()
}

#[tokio::test]
async fn alias_store_is_reused_until_the_real_ebuild_changes() {
    let (_tmp, repo, path) = repo_with_musl();
    let store = MemoryMetadataCache::new();
    let cpv = Cpv::parse("cross-aarch64-unknown-linux-musl/musl-1.0").expect("cpv");

    assert_eq!(
        description(&repo, &store, &path).await,
        "cross-aarch64-unknown-linux-musl"
    );
    assert!(repo.cache_entry(&cpv).expect("lookup").is_none());

    // A marked store entry that is still fresh is served as-is, not re-sourced.
    let mut entry = store.get(&cpv).expect("get").expect("stored");
    entry.metadata.description = "from-store".into();
    store.put(&cpv, &entry).expect("put");
    assert_eq!(description(&repo, &store, &path).await, "from-store");

    std::fs::write(&path, format!("{EBUILD}# changed\n")).expect("edit ebuild");
    assert_eq!(
        description(&repo, &store, &path).await,
        "cross-aarch64-unknown-linux-musl"
    );
}
