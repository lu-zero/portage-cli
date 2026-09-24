use std::collections::{HashMap, HashSet};
use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use portage_atom::interner::Interned;
use portage_atom::{Cpn, Cpv, Dep};
use portage_atom_pubgrub::{PackageRepository, UseOverride};
use portage_metadata::CacheEntry;
use portage_repo::AcceptSet;
use portage_resolve::force_mask::{ForceMask, ForceMaskLayer, fold_signed};
use portage_resolve::repo::{
    AcceptKeywords, AcceptLicenses, AcceptProperties, AcceptRestrict, Adapter, PolicyFactsCache,
    PolicyFactsRef, RepoData,
};

struct BenchRepo {
    data: RepoData,
    accept_keywords: AcceptKeywords,
    accept_licenses: AcceptLicenses,
    accept_properties: AcceptProperties,
    accept_restrict: AcceptRestrict,
    defaults: portage_atom_pubgrub::UseLayer,
    conf: portage_atom_pubgrub::UseLayer,
    env_use: portage_atom_pubgrub::UseLayer,
    force_mask: ForceMask,
    installed: HashSet<Cpv>,
    rebuilding: HashSet<Cpv>,
}

impl BenchRepo {
    fn new(size: usize) -> Self {
        let iuse = (0..16)
            .map(|i| format!("flag{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let mut versions = HashMap::new();
        let mut cpns = Vec::with_capacity(size);
        for i in 0..size {
            let cpn = Cpn::parse(&format!("bench/pkg{i}")).unwrap();
            let mut entries = Vec::with_capacity(2);
            for version in ["1.0", "2.0"] {
                let cpv = Cpv::parse(&format!("bench/pkg{i}-{version}")).unwrap();
                let cache = CacheEntry::parse(&format!(
                    "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nIUSE={iuse}\nLICENSE=flag0? ( GPL-2 )\nDESCRIPTION=bench\n"
                ))
                .unwrap();
                entries.push((cpv, cache));
            }
            cpns.push(cpn);
            versions.insert(cpn, entries);
        }
        let arch = gentoo_core::Arch::intern("amd64");
        Self {
            data: RepoData {
                cpns,
                versions,
                repo_name: Interned::intern("bench"),
                repo_of: HashMap::new(),
                real_cpn_of: HashMap::new(),
            },
            accept_keywords: AcceptKeywords::from_global(&arch, &["amd64"]),
            accept_licenses: AcceptLicenses::new(
                AcceptSet::from_tokens_plain(&["MIT".into()]),
                Vec::new(),
            ),
            accept_properties: AcceptProperties::new(
                AcceptSet::from_tokens_plain(&["*".into()]),
                Vec::new(),
            ),
            accept_restrict: AcceptRestrict::new(
                AcceptSet::from_tokens_plain(&["*".into()]),
                Vec::new(),
            ),
            defaults: portage_atom_pubgrub::UseLayer::empty(),
            conf: portage_atom_pubgrub::UseLayer::empty(),
            env_use: portage_atom_pubgrub::UseLayer::empty(),
            force_mask: ForceMask::one(ForceMaskLayer {
                use_force: fold_signed((1..16).map(|i| format!("flag{i}")).collect()),
                ..Default::default()
            }),
            installed: HashSet::new(),
            rebuilding: HashSet::new(),
        }
    }

    fn consume(
        &self,
        package_use: &[(Dep, Vec<UseOverride>)],
        facts: Option<PolicyFactsRef<'_>>,
    ) -> usize {
        self.consume_with_passes(package_use, facts, 1)
    }

    fn consume_with_passes(
        &self,
        package_use: &[(Dep, Vec<UseOverride>)],
        facts: Option<PolicyFactsRef<'_>>,
        passes: usize,
    ) -> usize {
        let adapter = Adapter {
            data: &self.data,
            accept_keywords: &self.accept_keywords,
            package_mask: &[],
            package_unmask: &[],
            accept_licenses: &self.accept_licenses,
            accept_properties: &self.accept_properties,
            accept_restrict: &self.accept_restrict,
            defaults: &self.defaults,
            conf: &self.conf,
            env_use: &self.env_use,
            package_use,
            profile_package_use: &[],
            force_mask: &self.force_mask,
            facts,
            installed_cpvs: &self.installed,
            rebuilding_cpvs: &self.rebuilding,
            autosolve_use: false,
            autounmask_widen: false,
        };
        let mut checksum = 0usize;
        for cpn in &self.data.cpns {
            let candidates = adapter.versions_for(cpn);
            checksum += candidates.len();
            for (cpv, _) in candidates {
                for _ in 0..passes {
                    let use_config = adapter.desired_use(&cpv);
                    checksum += usize::from(matches!(
                        use_config.get(Interned::intern("flag0")),
                        portage_atom_pubgrub::UseFlagState::Enabled
                    ));
                }
            }
        }
        black_box(checksum)
    }
}

fn package_use(flag: &str) -> Vec<(Dep, Vec<UseOverride>)> {
    vec![(
        Dep::parse("bench/pkg0").unwrap(),
        vec![UseOverride {
            flag: Interned::intern(flag),
            enable: true,
        }],
    )]
}

fn bench_policy_facts(c: &mut Criterion) {
    let mut group = c.benchmark_group("policy_facts");
    for size in [128, 512, 2048] {
        let repo = BenchRepo::new(size);
        let off = Vec::new();
        let on = package_use("flag0");
        group.bench_with_input(BenchmarkId::new("uncached", size), &size, |b, _| {
            b.iter(|| black_box(repo.consume(&off, None)));
        });

        group.bench_with_input(BenchmarkId::new("cached_cold", size), &size, |b, _| {
            b.iter(|| {
                let cache = PolicyFactsCache::new();
                let facts = cache.generation_for(&off);
                black_box(repo.consume(&off, Some(facts)));
            });
        });

        let cache = PolicyFactsCache::new();
        let facts = cache.generation_for(&off);
        group.bench_with_input(BenchmarkId::new("cached_warm", size), &size, |b, _| {
            b.iter(|| black_box(repo.consume(&off, Some(facts))));
        });
        group.bench_with_input(BenchmarkId::new("uncached_reuse_4", size), &size, |b, _| {
            b.iter(|| black_box(repo.consume_with_passes(&off, None, 4)));
        });
        group.bench_with_input(BenchmarkId::new("cached_reuse_4", size), &size, |b, _| {
            b.iter(|| black_box(repo.consume_with_passes(&off, Some(facts), 4)));
        });

        let generations = vec![off, on, Vec::new(), package_use("flag1")];
        group.bench_with_input(
            BenchmarkId::new("generations_uncached", size),
            &size,
            |b, _| {
                b.iter(|| {
                    let mut checksum = 0;
                    for package_use in &generations {
                        checksum += repo.consume(package_use, None);
                    }
                    black_box(checksum);
                });
            },
        );
        group.bench_with_input(BenchmarkId::new("generations", size), &size, |b, _| {
            b.iter(|| {
                let cache = PolicyFactsCache::new();
                let mut checksum = 0;
                for package_use in &generations {
                    let facts = cache.generation_for(package_use);
                    checksum += repo.consume(package_use, Some(facts));
                }
                black_box(checksum);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_policy_facts);
criterion_main!(benches);
