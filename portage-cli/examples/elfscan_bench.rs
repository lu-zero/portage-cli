//! Wall-clock ELF image scan harness (for hyperfine / Portage comparison).
//!
//! ```sh
//! cargo run -p portage-cli --release --example elfscan_bench -- /usr/lib64
//! cargo run -p portage-cli --release --example elfscan_bench -- --jobs 1 /usr/lib64
//! ```
//!
//! Prints one line: `elfs=<n> needed_lines=<n> elapsed_ms=<n> jobs=<n>`.

use std::env;
use std::path::Path;
use std::time::Instant;

use camino::Utf8Path;
use portage_cli::elfscan::scan_image_with_jobs;

fn main() {
    let mut jobs: Option<usize> = None;
    let mut dir: Option<String> = None;
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--jobs" | "-j" => {
                jobs = args.next().and_then(|s| s.parse().ok()).or(Some(1));
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: elfscan_bench [--jobs N] <image-dir>\n  \
                     N=1 serial; omit for available_parallelism()"
                );
                std::process::exit(0);
            }
            other if !other.starts_with('-') => dir = Some(other.to_string()),
            other => {
                eprintln!("unknown arg: {other}");
                std::process::exit(2);
            }
        }
    }
    let dir = dir.unwrap_or_else(|| {
        for c in ["/usr/lib64", "/usr/lib"] {
            if Path::new(c).is_dir() {
                return c.to_string();
            }
        }
        "/usr".to_string()
    });
    let utf = Utf8Path::new(&dir);
    if !utf.is_dir() {
        eprintln!("not a directory: {dir}");
        std::process::exit(1);
    }

    let t0 = Instant::now();
    let scan = scan_image_with_jobs(utf, jobs);
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    let j = jobs.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    });
    println!(
        "elfs={} needed_lines={} elapsed_ms={:.2} jobs={}",
        scan.needed_elf2.len(),
        scan.needed.len(),
        ms,
        j
    );
}
