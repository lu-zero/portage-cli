use std::collections::HashSet;
use std::fs;
use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use portage_vdb::{Collision, ContentsEntry, ContentsKind, ContentsRef, OwnershipIndex, Vdb};

struct Fixture {
    _temp: tempfile::TempDir,
    vdb: Vdb,
    planned: Vec<ContentsEntry>,
}

impl Fixture {
    fn new(package_count: usize, files_per_package: usize) -> Self {
        let temp = tempfile::tempdir().expect("temporary VDB");
        let category = temp.path().join("bench");
        fs::create_dir_all(&category).expect("category directory");
        let mut planned = Vec::with_capacity(package_count);

        for package in 0..package_count {
            let package_dir = category.join(format!("pkg{package:05}-1.0"));
            fs::create_dir_all(&package_dir).expect("package directory");
            fs::write(package_dir.join("EAPI"), "8\n").expect("EAPI");
            let mut contents = String::new();
            for file in 0..files_per_package {
                let path = format!("/usr/lib/bench/pkg{package:05}/file{file:03}");
                contents.push_str(&format!("obj {path} deadbeef 0\n"));
                if file == 0 {
                    planned.push(obj_entry(&path));
                }
            }
            fs::write(package_dir.join("CONTENTS"), contents).expect("CONTENTS");
        }

        let vdb = Vdb::open(temp.path().to_str().expect("UTF-8 VDB path")).expect("VDB");
        Self {
            _temp: temp,
            vdb,
            planned,
        }
    }
}

fn obj_entry(path: &str) -> ContentsEntry {
    ContentsEntry {
        kind: ContentsKind::Obj,
        path: path.into(),
        md5: Some("deadbeef".into()),
        mtime: Some(0),
        target: None,
    }
}

fn legacy_find_collisions(vdb: &Vdb, planned: &[ContentsEntry]) -> usize {
    let targets: HashSet<&str> = planned
        .iter()
        .filter(|entry| matches!(entry.kind, ContentsKind::Obj | ContentsKind::Sym))
        .map(|entry| entry.path.as_str())
        .collect();
    let mut collisions = Vec::new();
    for pkg in vdb.packages() {
        let Ok(Some(raw)) = pkg.contents_raw() else {
            continue;
        };
        for entry in ContentsRef::parse(&raw) {
            if matches!(entry.kind, ContentsKind::Obj | ContentsKind::Sym)
                && targets.contains(entry.path.as_str())
            {
                collisions.push(Collision {
                    path: entry.path.to_path_buf(),
                    owner: pkg.clone(),
                });
            }
        }
    }
    collisions.len()
}

fn indexed_find_collisions(index: &OwnershipIndex, planned: &[ContentsEntry]) -> usize {
    index.find_collisions(planned, None).len()
}

fn bench_ownership(c: &mut Criterion) {
    let mut group = c.benchmark_group("vdb_ownership");
    group.sample_size(10);

    for package_count in [128usize, 256, 512] {
        let fixture = Fixture::new(package_count, 32);
        let query = |index: &OwnershipIndex| {
            fixture
                .planned
                .iter()
                .map(|entry| indexed_find_collisions(index, std::slice::from_ref(entry)))
                .sum::<usize>()
        };
        let query_checked = |index: &OwnershipIndex| {
            fixture
                .planned
                .iter()
                .map(|entry| {
                    assert!(index.is_current(&fixture.vdb));
                    indexed_find_collisions(index, std::slice::from_ref(entry))
                })
                .sum::<usize>()
        };

        group.bench_with_input(
            BenchmarkId::new("legacy_scan_each", package_count),
            &package_count,
            |b, _| {
                b.iter(|| {
                    black_box(
                        fixture
                            .planned
                            .iter()
                            .map(|entry| {
                                legacy_find_collisions(&fixture.vdb, std::slice::from_ref(entry))
                            })
                            .sum::<usize>(),
                    )
                })
            },
        );

        group.bench_with_input(
            BenchmarkId::new("index_batch", package_count),
            &package_count,
            |b, _| {
                b.iter(|| {
                    let index = OwnershipIndex::build(&fixture.vdb).expect("ownership index");
                    let count = query_checked(&index);
                    black_box((index, count))
                })
            },
        );

        let index = OwnershipIndex::build(&fixture.vdb).expect("ownership index");
        group.bench_with_input(
            BenchmarkId::new("index_reuse", package_count),
            &package_count,
            |b, _| b.iter(|| black_box(query(&index))),
        );

        group.bench_with_input(
            BenchmarkId::new("index_reuse_checked", package_count),
            &package_count,
            |b, _| b.iter(|| black_box(query_checked(&index))),
        );
    }

    group.finish();
}

fn bench_real_vdb(c: &mut Criterion) {
    let root = std::env::var("VDB_ROOT").unwrap_or_else(|_| "/var/db/pkg".into());
    let Ok(vdb) = Vdb::open(root.as_str()) else {
        eprintln!("skipping real-VDB ownership benchmark: {root} not found");
        return;
    };
    let planned: Vec<ContentsEntry> = vdb
        .packages()
        .into_iter()
        .filter_map(|package| {
            let contents = package.contents().ok()?;
            contents
                .into_iter()
                .find(|entry| matches!(entry.kind, ContentsKind::Obj | ContentsKind::Sym))
        })
        .take(64)
        .collect();
    if planned.is_empty() {
        eprintln!("skipping real-VDB ownership benchmark: no file entries");
        return;
    }

    let mut group = c.benchmark_group("vdb_ownership_real");
    group.sample_size(10);
    group.bench_function("build", |b| {
        b.iter(|| black_box(OwnershipIndex::build(&vdb).expect("ownership index")))
    });
    let index = OwnershipIndex::build(&vdb).expect("ownership index");
    group.bench_function("query_64", |b| {
        b.iter(|| {
            black_box(
                planned
                    .iter()
                    .map(|entry| {
                        index
                            .find_collisions(std::slice::from_ref(entry), None)
                            .len()
                    })
                    .sum::<usize>(),
            )
        })
    });
    group.finish();
}

criterion_group!(benches, bench_ownership, bench_real_vdb);
criterion_main!(benches);
