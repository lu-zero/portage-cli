use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use portage_atom::{Cpv, Dep};
use portage_resolve::Avail;

fn make_avail(size: usize) -> (Avail, Dep) {
    let entries = (0..size)
        .map(|i| (Cpv::parse(&format!("app-misc/pkg{i}-1.0")).unwrap(), None))
        .collect();
    let dep = Dep::parse(&format!("app-misc/pkg{}-1.0", size - 1)).unwrap();
    (Avail::from_cpvs(entries), dep)
}

fn bench_avail_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("resolver/avail");
    for size in [100usize, 1_000, 10_000] {
        let (avail, dep) = make_avail(size);
        group.bench_with_input(BenchmarkId::new("atom_satisfied", size), &size, |b, _| {
            b.iter(|| std::hint::black_box(avail.atom_satisfied(&dep)))
        });
    }
    group.finish();
}

criterion_group!(benches, bench_avail_lookup);
criterion_main!(benches);
