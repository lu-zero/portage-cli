use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use portage_atom::{Cpn, Cpv, DepEntry};
use portage_atom_pubgrub::{
    InMemoryRepository, PackageDeps, PortageDependencyProvider, PortagePackage, PortageVersionSet,
};

fn deps(depend: &str, rdepend: &str) -> PackageDeps {
    PackageDeps {
        depend: DepEntry::parse(depend).unwrap().into(),
        rdepend: DepEntry::parse(rdepend).unwrap().into(),
        bdepend: (vec![]).into(),
        pdepend: (vec![]).into(),
        idepend: (vec![]).into(),
    }
}

fn fixture(size: usize) -> (PortageDependencyProvider, Vec<(Cpn, Cpv)>) {
    let mut repo = InMemoryRepository::new();
    let mut identities = Vec::with_capacity(size);
    for i in 0..size {
        let cpv = Cpv::parse(&format!("app-misc/pkg{i:05}-1.0")).unwrap();
        let cpn = cpv.cpn;
        let next = (i + 1) % size;
        let rdepend = if i == 0 {
            format!("app-misc/pkg{next:05} || ( app-misc/alt0 app-misc/alt1 )")
        } else {
            format!("app-misc/pkg{next:05}")
        };
        repo.add_version(cpv.clone(), None, None, deps("", &rdepend));
        identities.push((cpn, cpv));
    }

    let alt0 = Cpv::parse("app-misc/alt0-1.0").unwrap();
    let alt1 = Cpv::parse("app-misc/alt1-1.0").unwrap();
    repo.add_version(alt0.clone(), None, None, deps("", ""));
    repo.add_version(alt1.clone(), None, None, deps("", ""));
    let root = Cpv::parse("app-misc/pkg00000-1.0").unwrap();
    let mut provider = PortageDependencyProvider::new(repo);
    let target = PortagePackage::unslotted(root.cpn);
    let solution = provider
        .resolve_targets(vec![(target, PortageVersionSet::any())])
        .unwrap();
    let result = provider.install_order_with_stats(&solution);
    eprintln!(
        "graph fixture {size}: {} selected nodes, {} edges, {} virtual expansions, {} SCCs (largest {}), {} repair probes/{} nodes",
        result.stats.graph_nodes,
        result.stats.graph_edges,
        result.stats.virtual_expansions,
        result.stats.scc_count,
        result.stats.largest_scc,
        result.stats.repair_reachability_calls,
        result.stats.repair_reachability_nodes,
    );
    black_box(result);
    (provider, identities)
}

fn bench_graph(c: &mut Criterion) {
    let mut group = c.benchmark_group("resolver/graph");
    group.sample_size(10);
    for size in [32usize, 64, 128] {
        let (mut provider, identities) = fixture(size);
        let root = PortagePackage::unslotted(identities[0].0);
        let target = (root, PortageVersionSet::any());
        let solution = provider.resolve_targets(vec![target]).unwrap();
        group.bench_with_input(BenchmarkId::new("order", size), &size, |b, _| {
            b.iter(|| black_box(provider.install_order_with_stats(&solution)))
        });
    }
    group.finish();
}

criterion_group!(benches, bench_graph);
criterion_main!(benches);
