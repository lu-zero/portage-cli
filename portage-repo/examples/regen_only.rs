//! Minimal metadata regeneration benchmark — no comparison, optional write.
//!
//! Sources every ebuild in the repo in parallel and optionally writes the
//! resulting md5-cache files to a directory.  Intended as a like-for-like
//! comparison with:
//!
//!   pk repo metadata regen -p <cache-dir> -n -f -j <N> <repo>

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

#[cfg(feature = "mimalloc")]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::path::PathBuf;
use std::process;

use clap::Parser;
use portage_repo::{Ebuild, RegenOpts, Repository, SourceOpts, regen_cache};

#[derive(Parser)]
#[command(about = "Source all ebuilds and optionally write an md5-cache")]
struct Args {
    /// Path to the repository
    repo: String,
    /// Optional category/package glob filter (e.g. 'dev-util/*')
    filter: Option<String>,
    /// Write cache files to this directory
    #[arg(short = 'o', long, value_name = "DIR")]
    output: Option<PathBuf>,
    /// Number of parallel workers (default: available CPUs)
    #[arg(short = 'j', long)]
    jobs: Option<usize>,
    /// Deduplicate top-level dep entries before writing (matches pkgcraft output)
    #[arg(long)]
    dedup: bool,
}

fn matches_filter(ebuild: &Ebuild, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let cat_end = filter.find('/').unwrap_or(filter.len());
    let filter_cat = &filter[..cat_end];
    if ebuild.category() != filter_cat {
        return false;
    }
    let rest = &filter[cat_end..];
    if rest == "/" || (rest.ends_with("/*") && rest.len() == 2) {
        return true;
    }
    let cpv = ebuild.cpv().to_string();
    if let Some(prefix) = filter.strip_suffix('*') {
        cpv.starts_with(prefix)
    } else {
        cpv == filter
    }
}

#[tokio::main]
async fn main() {
    #[cfg(feature = "dhat-heap")]
    let _dhat = dhat::Profiler::new_heap();

    let args = Args::parse();

    let repo = match Repository::builder().in_memory_cache().open(&args.repo) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error opening repo: {e}");
            process::exit(1);
        }
    };

    let ebuilds = match repo.ebuilds() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Error listing ebuilds: {e}");
            process::exit(1);
        }
    };

    let ebuilds: Vec<_> = if let Some(ref f) = args.filter {
        let f = f.clone();
        ebuilds
            .into_iter()
            .filter(move |eb| matches_filter(eb, &f))
            .collect()
    } else {
        ebuilds.into_iter().collect()
    };

    let jobs = args.jobs.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    });
    let total = ebuilds.len();
    let filter_desc = args
        .filter
        .as_ref()
        .map(|f| format!(" (filter: {f})"))
        .unwrap_or_default();
    eprintln!(
        "Sourcing {total} ebuilds with {jobs} workers{}{}...",
        filter_desc,
        args.output
            .as_ref()
            .map(|p| format!(", writing to {}", p.display()))
            .unwrap_or_default()
    );

    let opts = RegenOpts {
        source: SourceOpts {
            jobs: Some(jobs),
            dedup: args.dedup,
        },
        write: args
            .output
            .map(portage_repo::RegenWriteTarget::Dir)
            .unwrap_or(portage_repo::RegenWriteTarget::None),
    };

    let stats = match regen_cache(
        &repo,
        &[],
        ebuilds,
        &opts,
        |done, total| {
            eprint!("\r[{done}/{total}]");
        },
        |_ebuild, e| {
            if let Some(diag) = e.parse_diagnostic() {
                eprintln!("{}", diag.render());
            }
        },
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("\nFatal error: {e}");
            process::exit(1);
        }
    };

    eprintln!();
    println!("Total: {}  Errors: {}", stats.total, stats.errors);

    let (hits, misses) = portage_repo::inherit::cache_stats();
    let total_lookups = hits + misses;
    if total_lookups > 0 {
        println!(
            "Eclass cache:  {} hits / {} misses ({:.1}% hit rate)",
            hits,
            misses,
            hits as f64 / total_lookups as f64 * 100.0
        );
    }

    if stats.errors > 0 {
        process::exit(1);
    }
}
