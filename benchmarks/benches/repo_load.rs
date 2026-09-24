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
    group.bench_function("repo_entries", |b| {
        b.iter(|| {
            let entries = runtime.block_on(repo_entries(&repo));
            assert_eq!(entries.len(), entry_count);
            std::hint::black_box(entries)
        })
    });
    group.finish();
}

criterion_group!(benches, bench_repo_load);
criterion_main!(benches);
