//! Comparison benchmarks against pkgcraft — split out from dep_parsing.rs so
//! the pkgcraft-free benchmarks there don't need the `pkgcraft-compare`
//! feature (and its sizeable gix dependency) just to build.

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

// Small: hand-crafted, exercises all prefix variants (+/-/none).
const IUSE_SMALL: &str = "ssl debug threads +gtk -wayland X doc test";

// Large: representative of a real-world package (ffmpeg ~80 flags).
const IUSE_LARGE: &str = "\
+X alsa amr amrenc bluray bs2b bzip2 cdio chromaprint codec2 dav1d drm \
fdk flite fontconfig frei0r fribidi gcrypt gme gmp gnutls gsm iec61883 \
ieee1394 jack jpeg2k kvazaar lame libaom libaribb24 libass libcaca libilbc \
librtmp libsoxr lv2 lzma modplug ocr openal opencl opengl openh264 openmpt \
+openssl opus pulseaudio rabbitmq rav1e rubberband samba sdl snappy sndio \
speex srt ssh svg svt-av1 theora truetype twolame v4l vaapi vdpau vidstab \
vorbis vpx vulkan webp x264 x265 xml xvid \
cpu_flags_x86_mmx cpu_flags_x86_mmxext cpu_flags_x86_sse cpu_flags_x86_sse2 \
cpu_flags_x86_avx cpu_flags_x86_avx2 \
-gpl -test doc";

fn required_use_string() -> &'static str {
    "ssl? ( threads ) || ( ssl tls ) ?? ( gtk qt5 ) ^^ ( X wayland )"
}

fn bench_iuse(c: &mut Criterion) {
    use pkgcraft::pkg::ebuild::iuse::Iuse;

    let mut group = c.benchmark_group("comparison/IUse::parse_line");

    for (name, input) in [("small", IUSE_SMALL), ("large", IUSE_LARGE)] {
        group.bench_with_input(
            BenchmarkId::new("portage-metadata", name),
            input,
            |b, input| {
                b.iter(|| black_box(portage_metadata::IUse::parse_line(black_box(input)).unwrap()))
            },
        );

        group.bench_with_input(BenchmarkId::new("pkgcraft", name), input, |b, input| {
            b.iter(|| {
                black_box(
                    input
                        .split_whitespace()
                        .map(|s| Iuse::try_new(black_box(s)))
                        .collect::<Result<Vec<_>, _>>()
                        .unwrap(),
                )
            })
        });
    }

    group.finish();
}

fn bench_pkgcraft(c: &mut Criterion) {
    use pkgcraft::dep::DependencySet;
    use pkgcraft::eapi::EAPI_LATEST_OFFICIAL;

    let eapi = *EAPI_LATEST_OFFICIAL;
    let mut group = c.benchmark_group("pkgcraft/DependencySet::package");

    for (name, input) in [
        ("simple", simple_dep()),
        ("medium", medium_dep()),
        ("complex", complex_dep()),
    ] {
        group.bench_with_input(BenchmarkId::new("dep", name), input, |b, input| {
            b.iter(|| black_box(DependencySet::package(black_box(input), eapi).unwrap()))
        });
    }

    group.bench_function("required_use", |b| {
        b.iter(|| black_box(DependencySet::required_use(black_box(required_use_string())).unwrap()))
    });

    group.finish();
}

fn bench_comparison(c: &mut Criterion) {
    use pkgcraft::dep::DependencySet;
    use pkgcraft::eapi::EAPI_LATEST_OFFICIAL;

    let eapi = *EAPI_LATEST_OFFICIAL;
    let mut group = c.benchmark_group("comparison/complex_dep");
    let input = complex_dep();

    group.bench_function("portage-atom", |b| {
        b.iter(|| black_box(portage_atom::DepEntry::parse(black_box(input)).unwrap()))
    });

    group.bench_function("pkgcraft", |b| {
        b.iter(|| black_box(DependencySet::package(black_box(input), eapi).unwrap()))
    });

    group.finish();
}

criterion_group!(benches, bench_pkgcraft, bench_comparison, bench_iuse);
criterion_main!(benches);
