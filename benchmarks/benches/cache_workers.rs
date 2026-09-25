use std::hint::black_box;
use std::path::Path;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use portage_metadata::CacheEntry;
use portage_repo::{CacheReadOpts, Repository, cache_entries_parallel};

const DEFAULT_REPO: &str = "/var/db/repos/gentoo";

fn bench_cache_workers(c: &mut Criterion) {
    let repo_path = std::env::var("GENTOO_REPO").unwrap_or_else(|_| DEFAULT_REPO.to_string());
    if !Path::new(&repo_path).exists() {
        eprintln!("skipping cache-worker benchmarks: {repo_path} not found");
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
    let entry_count = runtime
        .block_on(cache_entries_parallel(
            std::slice::from_ref(&repo),
            &CacheReadOpts::default(),
            |text| CacheEntry::parse(text).map_err(portage_repo::Error::from),
        ))
        .expect("cache discovery")
        .len();
    if entry_count == 0 {
        eprintln!("skipping cache-worker benchmarks: {repo_path} has no metadata entries");
        return;
    }
    eprintln!("cache metadata entries: {entry_count}");

    let mut group = c.benchmark_group("cache_workers");
    group.sample_size(10);
    for jobs in [
        None,
        Some(1),
        Some(4),
        Some(16),
        Some(32),
        Some(64),
        Some(128),
    ] {
        let label = jobs.map_or_else(|| "default".to_owned(), |n| n.to_string());
        let opts = CacheReadOpts {
            jobs,
            ..CacheReadOpts::default()
        };
        group.bench_with_input(BenchmarkId::new("read", &label), &label, |b, _| {
            b.iter(|| {
                let entries = runtime
                    .block_on(cache_entries_parallel(
                        std::slice::from_ref(&repo),
                        &opts,
                        |text| CacheEntry::parse(text).map_err(portage_repo::Error::from),
                    ))
                    .expect("cache discovery");
                assert_eq!(entries.len(), entry_count);
                black_box(entries)
            })
        });
    }
    group.finish();
}

criterion_group!(benches, bench_cache_workers);
criterion_main!(benches);
