use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

fn simple_dep() -> &'static str {
    "dev-libs/openssl"
}

fn medium_dep() -> &'static str {
    "dev-libs/openssl sys-libs/zlib ssl? ( dev-libs/nss ) threads? ( sys-libs/libcompat )"
}

fn complex_dep() -> &'static str {
    "dev-libs/openssl:0=
    sys-libs/zlib
    ssl? (
        dev-libs/nss
        || ( dev-libs/libressl dev-libs/openssl:0= )
    )
    threads? ( sys-libs/libcompat )
    || ( sys-libs/glibc sys-libs/musl )
    !dev-libs/openssl-compat:0"
}

fn required_use_string() -> &'static str {
    "ssl? ( threads ) || ( ssl tls ) ?? ( gtk qt5 ) ^^ ( X wayland )"
}

fn bench_portage_atom(c: &mut Criterion) {
    let mut group = c.benchmark_group("portage-atom/DepEntry::parse");

    for (name, input) in [
        ("simple", simple_dep()),
        ("medium", medium_dep()),
        ("complex", complex_dep()),
    ] {
        group.bench_with_input(BenchmarkId::new("dep", name), input, |b, input| {
            b.iter(|| black_box(portage_atom::DepEntry::parse(black_box(input)).unwrap()))
        });
    }

    group.finish();
}

fn bench_portage_metadata(c: &mut Criterion) {
    let mut group = c.benchmark_group("portage-metadata/RequiredUseExpr::parse");

    group.bench_function("required_use", |b| {
        b.iter(|| {
            black_box(
                portage_metadata::RequiredUseExpr::parse(black_box(required_use_string())).unwrap(),
            )
        })
    });

    group.finish();
}

criterion_group!(benches, bench_portage_atom, bench_portage_metadata);
criterion_main!(benches);
