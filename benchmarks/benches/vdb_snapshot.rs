use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use portage_resolve::Avail;
use portage_resolve::Roots;
use portage_resolve::installed::{
    BrootSnapshot, load_host_installed, load_host_installed_from_snapshot,
};

struct Fixture {
    root: PathBuf,
    roots: Roots,
    snapshot: BrootSnapshot,
}

impl Fixture {
    fn new(size: usize) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is before the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "portage-bench-vdb-snapshot-{}-{stamp}",
            std::process::id()
        ));
        let host = root.join("host");
        let prefix = root.join("prefix");

        for index in 0..size {
            write_vdb_entry(&host, &format!("pkg{index:05}"), "1.0");
            if index % 2 == 0 {
                write_vdb_entry(&prefix, &format!("pkg{index:05}"), "2.0");
            }
        }

        let roots = Roots::for_test_overlay(host.to_str().unwrap(), prefix.to_str().unwrap());
        let snapshot = BrootSnapshot::load(&roots);
        Self {
            root,
            roots,
            snapshot,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn write_vdb_entry(root: &Path, package: &str, version: &str) {
    let dir = root
        .join("var/db/pkg/bench")
        .join(format!("{package}-{version}"));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("EAPI"), "8\n").unwrap();
    fs::write(dir.join("SLOT"), "0\n").unwrap();
    fs::write(dir.join("USE"), "benchmark\n").unwrap();
    fs::write(dir.join("IUSE"), "+benchmark\n").unwrap();
    fs::write(dir.join("CONTENTS"), "").unwrap();
}

fn bench_vdb_snapshot(c: &mut Criterion) {
    let mut group = c.benchmark_group("resolver/vdb_snapshot");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));

    for size in [128usize, 512, 2048] {
        let fixture = Fixture::new(size);
        eprintln!(
            "vdb snapshot fixture {size}: {} raw rows",
            fixture.snapshot.len()
        );

        group.bench_with_input(BenchmarkId::new("load_once", size), &size, |b, _| {
            b.iter(|| {
                let snapshot = BrootSnapshot::load(&fixture.roots);
                let avail = Avail::initial_bdepend_from_snapshot(&snapshot);
                let host = load_host_installed_from_snapshot(&snapshot);
                std::hint::black_box((avail, host));
            })
        });

        group.bench_with_input(BenchmarkId::new("reuse", size), &size, |b, _| {
            b.iter(|| {
                let avail = Avail::initial_bdepend_from_snapshot(&fixture.snapshot);
                let host = load_host_installed_from_snapshot(&fixture.snapshot);
                std::hint::black_box((avail, host));
            })
        });

        group.bench_with_input(BenchmarkId::new("scan_each_view", size), &size, |b, _| {
            b.iter(|| {
                let avail = Avail::initial_bdepend(&fixture.roots);
                let host = load_host_installed(&fixture.roots);
                std::hint::black_box((avail, host));
            })
        });
    }

    group.finish();
}

criterion_group!(benches, bench_vdb_snapshot);
criterion_main!(benches);
