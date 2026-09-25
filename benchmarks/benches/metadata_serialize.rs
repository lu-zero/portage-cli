use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use portage_metadata::CacheEntry;

const CACHE: &str = "\
DEFINED_PHASES=install setup
DEPEND=dev-libs/libfoo dev-libs/libbar sys-devel/gcc
DESCRIPTION=Synthetic metadata serialization fixture
EAPI=8
HOMEPAGE=https://example.com/ https://example.org/
IUSE=foo bar baz
KEYWORDS=amd64 x86
LICENSE=MIT
PDEPEND=dev-libs/libbaz
RDEPEND=dev-libs/libfoo dev-libs/libbar
REQUIRED_USE=|| ( foo bar )
RESTRICT=!test? ( test )
SLOT=0
SRC_URI=https://example.com/foo.tar.gz
BDEPEND=dev-build/libfoo
IDEPEND=dev-libs/libbar
PROPERTIES=live !test? ( mirror )
INHERIT=foo
_eclasses_=foo\t00000000000000000000000000000000\tbar\t11111111111111111111111111111111
_md5_=22222222222222222222222222222222
";

fn bench_metadata_serialize(c: &mut Criterion) {
    let entry = CacheEntry::parse(CACHE).unwrap();
    let mut group = c.benchmark_group("metadata_serialize");
    group.sample_size(10);
    group.bench_function("full_entry", |b| {
        b.iter(|| black_box(entry.serialize()));
    });
    group.finish();
}

criterion_group!(benches, bench_metadata_serialize);
criterion_main!(benches);
