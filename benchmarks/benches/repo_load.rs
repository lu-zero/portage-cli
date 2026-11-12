use std::path::Path;

use criterion::{Criterion, criterion_group, criterion_main};
use portage_repo::{Repository, repo_entries};

const DEFAULT_REPO: &str = "/var/db/repos/gentoo";

fn bench_repo_load(c: &mut Criterion) {
    let repo_path = std::env::var("GENTOO_REPO").unwrap_or_else(|_| DEFAULT_REPO.to_string());
    if !Path::new(&repo_path).exists() {
        eprintln!("skipping repository-load benchmarks: {repo_path} not found");
        return;
    }

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .expect("failed to open repository");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let entry_count = runtime.block_on(repo_entries(&repo)).len();
    if entry_count == 0 {
        eprintln!("skipping repository-load benchmarks: {repo_path} has no metadata entries");
        return;
    }
    eprintln!("repository metadata entries: {entry_count}");

    let mut group = c.benchmark_group("repository/load");
    group.sample_size(10);
    // No sidecar store, so no gap index: every call walks and stats every ebuild.
    group.bench_function("repo_entries", |b| {
        b.iter_with_large_drop(|| {
            let entries = runtime.block_on(repo_entries(&repo));
            assert_eq!(entries.len(), entry_count);
            std::hint::black_box(entries)
        })
    });

    // What `em` does on an unchanged tree: a real cache root, so the gap index from the first call skips the ebuild walk.
    let cache_root = tempfile::tempdir().expect("cache root");
    let indexed = Repository::builder()
        .user_cache_root(
            camino::Utf8PathBuf::try_from(cache_root.path().to_owned()).expect("utf-8 cache root"),
        )
        .open(&repo_path)
        .expect("failed to open repository");
    assert_eq!(runtime.block_on(repo_entries(&indexed)).len(), entry_count);
    group.bench_function("repo_entries_indexed", |b| {
        b.iter_with_large_drop(|| {
            let entries = runtime.block_on(repo_entries(&indexed));
            assert_eq!(entries.len(), entry_count);
            std::hint::black_box(entries)
        })
    });

    // Many small reads through the tree: one per profiles/updates file.
    group.bench_function("profile_updates", |b| {
        b.iter(|| std::hint::black_box(repo.profile_updates().expect("updates")))
    });

    group.finish();
}

criterion_group!(benches, bench_repo_load);
criterion_main!(benches);
