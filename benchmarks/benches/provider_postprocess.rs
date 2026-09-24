use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use portage_atom::{Cpv, DepEntry};
use portage_atom_pubgrub::{InMemoryRepository, PackageDeps, PortageDependencyProvider};

fn deps(depend: &str) -> PackageDeps {
    PackageDeps {
        depend: DepEntry::parse(depend).unwrap().into(),
        rdepend: (vec![]).into(),
        bdepend: (vec![]).into(),
        pdepend: (vec![]).into(),
        idepend: (vec![]).into(),
    }
}

fn fixture(size: usize, missing: bool) -> InMemoryRepository {
    let mut repo = InMemoryRepository::new();
    for i in 0..size {
        let cpv = Cpv::parse(&format!("bench/pkg{i}-1.0")).unwrap();
        let next = (i + 1) % size;
        let dependency = if missing && i == 0 {
            "missing/package".to_string()
        } else {
            format!("bench/pkg{next}")
        };
        repo.add_version(cpv, None, None, deps(&dependency));
    }
    if missing {
        for i in 0..size {
            let cpv = Cpv::parse(&format!("bench/choice{i}-1.0")).unwrap();
            let next = (i + 1) % size;
            repo.add_version(
                cpv,
                None,
                None,
                deps(&format!("|| ( bench/pkg{next} bench/alt{i} )")),
            );
            repo.add_version(
                Cpv::parse(&format!("bench/alt{i}-1.0")).unwrap(),
                None,
                None,
                deps(""),
            );
        }
    }
    repo
}

fn bench_provider_postprocess(c: &mut Criterion) {
    let mut group = c.benchmark_group("provider_postprocess");
    group.sample_size(10);
    for size in [128usize, 512, 2048] {
        let known = fixture(size, false);
        group.bench_with_input(BenchmarkId::new("all_known", size), &size, |b, _| {
            b.iter_with_setup(
                || known.clone(),
                |repo| black_box(PortageDependencyProvider::new(repo)),
            )
        });
        let missing = fixture(size, true);
        group.bench_with_input(BenchmarkId::new("missing_branch", size), &size, |b, _| {
            b.iter_with_setup(
                || missing.clone(),
                |repo| black_box(PortageDependencyProvider::new(repo)),
            )
        });
    }
    group.finish();
}

criterion_group!(benches, bench_provider_postprocess);
criterion_main!(benches);
