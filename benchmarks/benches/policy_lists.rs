use std::collections::HashMap;
use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use portage_atom::interner::Interned;
use portage_atom::{Cpn, Cpv, Dep};
use portage_atom_pubgrub::UseLayer;
use portage_metadata::CacheEntry;
use portage_repo::AcceptSet;
use portage_resolve::force_mask::ForceMask;
use portage_resolve::repo::{
    AcceptKeywords, AcceptLicenses, AcceptProperties, AcceptRestrict, AcceptToken, PolicyMaskList,
    RepoData, ResolvePolicy, filter_reasons_for,
};

struct Fixture {
    data: RepoData,
    cpn: Cpn,
    accept_keywords: AcceptKeywords,
    accept_licenses: AcceptLicenses,
    accept_properties: AcceptProperties,
    accept_restrict: AcceptRestrict,
    package_mask: PolicyMaskList,
    package_unmask: PolicyMaskList,
    defaults: UseLayer,
    conf: UseLayer,
    env_use: UseLayer,
    force_mask: ForceMask,
}

impl Fixture {
    fn new(size: usize) -> Self {
        let arch = gentoo_core::Arch::intern("amd64");
        let cpn = Cpn::parse("bench/pkg").unwrap();
        let cpv = Cpv::parse("bench/pkg-1.0").unwrap();
        let cache = CacheEntry::parse(
            "EAPI=8\nSLOT=0\nKEYWORDS=~amd64\nIUSE=flag0\nLICENSE=GPL-2\nPROPERTIES=interactive\nRESTRICT=bindist\nDESCRIPTION=bench\n",
        )
        .unwrap();
        let mut versions = HashMap::new();
        versions.insert(cpn, vec![(cpv, cache)]);

        let mut keyword_overrides = Vec::with_capacity(size + 1);
        let mut license_overrides = Vec::with_capacity(size + 1);
        let mut property_overrides = Vec::with_capacity(size + 1);
        let mut restrict_overrides = Vec::with_capacity(size + 1);
        let mut mask_entries = Vec::with_capacity(size + 1);
        let mut unmask_entries = Vec::with_capacity(size + 1);
        for i in 0..size {
            let noise = Dep::parse(&format!("noise/pkg{i}")).unwrap();
            keyword_overrides.push((noise.clone(), vec![AcceptToken::parse("~amd64").unwrap()]));
            license_overrides.push((
                noise.clone(),
                AcceptSet::from_tokens_plain(&["GPL-2".into()]),
            ));
            property_overrides.push((
                noise.clone(),
                AcceptSet::from_tokens_plain(&["interactive".into()]),
            ));
            restrict_overrides.push((
                noise.clone(),
                AcceptSet::from_tokens_plain(&["bindist".into()]),
            ));
            mask_entries.push(noise.clone());
        }
        let target = Dep::parse("bench/pkg").unwrap();
        keyword_overrides.push((target.clone(), vec![AcceptToken::parse("~amd64").unwrap()]));
        license_overrides.push((
            target.clone(),
            AcceptSet::from_tokens_plain(&["GPL-2".into()]),
        ));
        property_overrides.push((
            target.clone(),
            AcceptSet::from_tokens_plain(&["interactive".into()]),
        ));
        restrict_overrides.push((
            target.clone(),
            AcceptSet::from_tokens_plain(&["bindist".into()]),
        ));
        mask_entries.push(target.clone());
        unmask_entries.push(target);

        Self {
            data: RepoData::from_parts(
                vec![cpn],
                versions,
                Interned::intern("bench"),
                HashMap::new(),
                HashMap::new(),
            ),
            cpn,
            accept_keywords: AcceptKeywords::new(
                &arch,
                &[AcceptToken::parse("amd64").unwrap()],
                keyword_overrides,
            ),
            accept_licenses: AcceptLicenses::new(
                AcceptSet::from_tokens_plain(&["MIT".into()]),
                license_overrides,
            ),
            accept_properties: AcceptProperties::new(
                AcceptSet::from_tokens_plain(&[]),
                property_overrides,
            ),
            accept_restrict: AcceptRestrict::new(
                AcceptSet::from_tokens_plain(&[]),
                restrict_overrides,
            ),
            package_mask: PolicyMaskList::new(mask_entries),
            package_unmask: PolicyMaskList::new(unmask_entries),
            defaults: UseLayer::empty(),
            conf: UseLayer::empty(),
            env_use: UseLayer::empty(),
            force_mask: ForceMask::default(),
        }
    }

    fn run(&self) -> usize {
        let policy = ResolvePolicy {
            accept_keywords: &self.accept_keywords,
            package_mask: &self.package_mask,
            package_unmask: &self.package_unmask,
            accept_licenses: &self.accept_licenses,
            accept_properties: &self.accept_properties,
            accept_restrict: &self.accept_restrict,
            defaults: &self.defaults,
            conf: &self.conf,
            env_use: &self.env_use,
            package_use: &[],
            profile_package_use: &[],
            force_mask: &self.force_mask,
            facts: None,
        };
        filter_reasons_for(
            &self.data,
            &self.cpn,
            &portage_atom_pubgrub::PortageVersionSet::any(),
            &policy,
        )
        .len()
    }
}

fn bench_policy_lists(c: &mut Criterion) {
    let mut group = c.benchmark_group("policy_lists");
    for size in [128, 512, 2048] {
        let fixture = Fixture::new(size);
        group.bench_with_input(BenchmarkId::new("full_filter", size), &size, |b, _| {
            b.iter(|| black_box(fixture.run()));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_policy_lists);
criterion_main!(benches);
