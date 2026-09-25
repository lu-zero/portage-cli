use std::fs;
use std::hint::black_box;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use portage_repo::{Repository, repo_entries};

struct Fixture {
    root: PathBuf,
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
        "portage-eclass-memo-{}-{suffix}",
        std::process::id()
    ));
    fs::create_dir_all(root.join("metadata")).unwrap();
    fs::write(root.join("metadata/layout.conf"), "masters =\n").unwrap();
    fs::create_dir_all(root.join("profiles")).unwrap();
    fs::write(root.join("profiles/repo_name"), "bench\n").unwrap();
    fs::write(root.join("profiles/categories"), "cat\n").unwrap();
    fs::create_dir_all(root.join("eclass")).unwrap();
    fs::write(root.join("eclass/shared.eclass"), "IUSE=\"shared\"\n").unwrap();

    for i in 0..size {
        let dir = root.join("cat").join(format!("pkg{i:05}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(format!("pkg{i:05}-1.0.ebuild")),
            "EAPI=8\ninherit shared\nDESCRIPTION=\"base\"\nSLOT=0\n",
        )
        .unwrap();
    }
    Fixture { root }
}

fn run(fixture: &Fixture) -> usize {
    let repo = Repository::builder()
        .in_memory_cache()
        .open(&fixture.root)
        .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async { black_box(repo_entries(&repo).await.len()) })
}

fn bench_eclass_memo(c: &mut Criterion) {
    let mut group = c.benchmark_group("eclass_memo");
    group.sample_size(10);
    for size in [32usize, 128, 512] {
        let fixture = fixture(size);
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| black_box(run(&fixture)));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_eclass_memo);
criterion_main!(benches);
