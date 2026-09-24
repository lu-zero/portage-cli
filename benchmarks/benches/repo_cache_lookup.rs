use std::collections::HashMap;
use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use portage_atom::interner::Interned;
use portage_atom::{Cpn, Cpv, Version};
use portage_atom_pubgrub::PortagePackage;
use portage_metadata::CacheEntry;
use portage_resolve::repo::{RepoData, find_cache};

struct Fixture {
    data: RepoData,
    queries: Vec<(PortagePackage, Version)>,
}

fn make_data(cpn_count: usize, version_count: usize) -> (RepoData, Vec<(PortagePackage, Version)>) {
    let mut cpns = Vec::with_capacity(cpn_count);
    let mut versions = HashMap::with_capacity(cpn_count);
    let mut queries = Vec::with_capacity(cpn_count * 3);
    for cpn_index in 0..cpn_count {
        let cpn = Cpn::parse(&format!("cat/pkg{cpn_index}")).unwrap();
        let mut entries = Vec::with_capacity(version_count);
        for version_index in 0..version_count {
            let version = Version::parse(&(version_index + 1).to_string()).unwrap();
            let cpv = Cpv::new(cpn, version.clone());
            let cache =
                CacheEntry::parse("EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=cpv\n").unwrap();
            entries.push((cpv, cache));
        }
        for version_index in [0, version_count / 2, version_count - 1] {
            let version = Version::parse(&(version_index + 1).to_string()).unwrap();
            queries.push((PortagePackage::unslotted(cpn), version));
        }
        cpns.push(cpn);
        versions.insert(cpn, entries);
    }
    let data = RepoData::from_parts(
        cpns,
        versions,
        Interned::intern("bench"),
        HashMap::new(),
        HashMap::new(),
    );
    (data, queries)
}

impl Fixture {
    fn new(cpn_count: usize, version_count: usize) -> Self {
        let (data, queries) = make_data(cpn_count, version_count);
        Self { data, queries }
    }

    fn lookup(&self) -> usize {
        self.queries
            .iter()
            .filter(|(package, version)| {
                black_box(find_cache(&self.data, package, version).is_some())
            })
            .count()
    }
}

fn bench_repo_cache_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("repo_cache_lookup");
    for cpn_count in [128, 512, 2048] {
        for version_count in [8, 32, 128] {
            let fixture = Fixture::new(cpn_count, version_count);
            let id = format!("{cpn_count}x{version_count}");
            group.bench_with_input(BenchmarkId::new("build", &id), &id, |b, _| {
                b.iter(|| black_box(make_data(cpn_count, version_count).0));
            });
            group.bench_with_input(BenchmarkId::new("lookup", &id), &id, |b, _| {
                b.iter(|| black_box(fixture.lookup()));
            });
        }
    }
    group.finish();
}

criterion_group!(benches, bench_repo_cache_lookup);
criterion_main!(benches);
