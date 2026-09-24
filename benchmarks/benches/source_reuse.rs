use std::fs;
use std::hint::black_box;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use camino::Utf8Path;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use portage_repo::{Ebuild, Repository, SourceContext, SourceOpts, source_parallel};

struct Fixture {
    root: PathBuf,
    repo: Repository,
    ebuilds: Vec<Ebuild>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn fixture(size: usize) -> Fixture {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "portage-source-reuse-{}-{suffix}",
        std::process::id()
    ));
    fs::create_dir_all(root.join("metadata")).unwrap();
    fs::write(root.join("metadata/layout.conf"), "masters =\n").unwrap();
    fs::create_dir_all(root.join("profiles")).unwrap();
    fs::write(root.join("profiles/repo_name"), "bench\n").unwrap();
    fs::write(root.join("profiles/categories"), "cat\n").unwrap();

    let mut ebuilds = Vec::with_capacity(size);
    for i in 0..size {
        let dir = root.join("cat").join(format!("pkg{i:05}"));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("pkg{i:05}-1.0.ebuild"));
        fs::write(&path, format!("EAPI=8\nDESCRIPTION=\"pkg{i}\"\nSLOT=0\n")).unwrap();
        ebuilds.push(Ebuild::from_path(Utf8Path::from_path(&path).unwrap()).unwrap());
    }
    let repo = Repository::builder().in_memory_cache().open(&root).unwrap();
    Fixture {
        root,
        repo,
        ebuilds,
    }
}

fn run(fixture: &Fixture, jobs: usize) -> usize {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let receiver = source_parallel(
            &fixture.repo,
            fixture.ebuilds.clone(),
            &SourceOpts {
                jobs: Some(jobs),
                dedup: false,
            },
            &SourceContext::new(),
        );
        let mut count = 0;
        while let Ok((_, result)) = receiver.recv_async().await {
            count += usize::from(result.is_ok());
        }
        count
    })
}

fn bench_source_reuse(c: &mut Criterion) {
    let mut group = c.benchmark_group("source_reuse");
    group.sample_size(10);
    for size in [32usize, 128, 512] {
        let fixture = fixture(size);
        for jobs in [1usize, 4] {
            group.bench_with_input(
                BenchmarkId::new(format!("{size}x{jobs}"), size),
                &size,
                |b, _| b.iter(|| black_box(run(&fixture, jobs))),
            );
        }
    }
    group.finish();
}

criterion_group!(benches, bench_source_reuse);
criterion_main!(benches);
