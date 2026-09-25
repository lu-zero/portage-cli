use std::fs;
use std::hint::black_box;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use portage_repo::{Ebuild, RegenOpts, RegenWriteTarget, Repository, SourceOpts, regen_cache};

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
        "portage-regen-reads-{}-{suffix}",
        std::process::id()
    ));
    fs::create_dir_all(root.join("metadata")).unwrap();
    fs::write(root.join("metadata/layout.conf"), "masters =\n").unwrap();
    fs::create_dir_all(root.join("profiles")).unwrap();
    fs::write(root.join("profiles/repo_name"), "bench\n").unwrap();
    fs::write(root.join("profiles/categories"), "cat\n").unwrap();

    for i in 0..size {
        let dir = root.join("cat").join(format!("pkg{i:05}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(format!("pkg{i:05}-1.0.ebuild")),
            "EAPI=8\nDESCRIPTION=\"base\"\nSLOT=0\n",
        )
        .unwrap();
    }

    let repo = Repository::builder().in_memory_cache().open(&root).unwrap();
    let ebuilds = repo.ebuilds().unwrap().into_iter().collect();
    Fixture {
        root,
        repo,
        ebuilds,
    }
}

fn run(fixture: &Fixture, runtime: &tokio::runtime::Runtime) -> usize {
    let (tx, rx) = flume::unbounded();
    drop(rx);
    let opts = RegenOpts {
        source: SourceOpts {
            jobs: Some(1),
            dedup: false,
        },
        write: RegenWriteTarget::Dir(fixture.root.join("out")),
    };
    let stats = runtime
        .block_on(regen_cache(
            &fixture.repo,
            fixture.ebuilds.clone(),
            &opts,
            tx,
        ))
        .unwrap();
    assert_eq!(stats.errors, 0);
    black_box(stats.total)
}

fn bench_regen_reads(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut group = c.benchmark_group("regen_reads");
    group.sample_size(10);
    for size in [32usize, 128, 512] {
        let fixture = fixture(size);
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| black_box(run(&fixture, &runtime)));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_regen_reads);
criterion_main!(benches);
