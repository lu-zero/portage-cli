//! Criterion microbenchmarks for install-image ELF scanning.
//!
//! ```sh
//! # default tree: $ELFSCAN_BENCH_DIR or /usr/lib64
//! cargo bench -p portage-cli --bench elfscan
//!
//! ELFSCAN_BENCH_DIR=/path/to/image cargo bench -p portage-cli --bench elfscan
//! ```
//!
//! Compares serial (`jobs=1`) vs parallel (`jobs=available`) on the same tree.
//! For wall-clock vs Portage's `scanelf`, see `benchmarks/bench-elfscan.sh`.

use std::path::Path;
use std::time::Duration;

use camino::Utf8Path;
use criterion::{Criterion, criterion_group, criterion_main};
use portage_cli::elfscan::{collect_regular_files, scan_image_with_jobs};

fn bench_dir() -> &'static Path {
    static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        std::env::var_os("ELFSCAN_BENCH_DIR")
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| {
                for cand in ["/usr/lib64", "/usr/lib", "/lib64", "/lib"] {
                    let p = Path::new(cand);
                    if p.is_dir() {
                        return p.to_path_buf();
                    }
                }
                Path::new("/usr").to_path_buf()
            })
    })
    .as_path()
}

fn elfscan_benches(c: &mut Criterion) {
    let dir = bench_dir();
    let utf = match Utf8Path::from_path(dir) {
        Some(p) => p,
        None => {
            eprintln!("skip: non-UTF-8 bench dir {dir:?}");
            return;
        }
    };
    let n_files = collect_regular_files(dir).len();
    eprintln!("elfscan bench tree: {} ({} regular files)", utf, n_files);
    if n_files == 0 {
        eprintln!("skip: empty tree");
        return;
    }

    let mut group = c.benchmark_group("elfscan_image");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(15));
    group.warm_up_time(Duration::from_secs(2));

    group.bench_function("serial_jobs1", |b| {
        b.iter(|| scan_image_with_jobs(utf, Some(1)))
    });

    let jobs = portage_cli::elfscan::default_jobs();
    group.bench_function(format!("parallel_jobs{jobs}"), |b| {
        b.iter(|| scan_image_with_jobs(utf, Some(jobs)))
    });

    group.finish();
}

criterion_group!(benches, elfscan_benches);
criterion_main!(benches);
