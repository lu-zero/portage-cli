//! Source every ebuild in a repository and compare the extracted metadata
//! against the existing `metadata/md5-cache/` entries.
//!
//! Progress is written to stderr; the final stats table goes to stdout.
//! Exit code is 1 if there are any sourcing errors or metadata mismatches.

use std::collections::BTreeMap;
use std::fs;
use std::process;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use clap::Parser;
use portage_metadata::CacheEntry;
use portage_repo::{Ebuild, Repository, SourceContext, SourceOpts, source_parallel};

/// Fields to compare between sourced metadata and the md5-cache.
///
/// Note: `INHERITED` (transitive eclass list) is intentionally excluded — it
/// is stored in the md5-cache as `_eclasses_=` with checksums and is not a
/// directly comparable text field.  `INHERIT` (direct eclass list) is included
/// because both sides now produce it correctly.
const COMPARE_KEYS: &[&str] = &[
    "EAPI",
    "DESCRIPTION",
    "SLOT",
    "HOMEPAGE",
    "SRC_URI",
    "LICENSE",
    "KEYWORDS",
    "IUSE",
    "REQUIRED_USE",
    "RESTRICT",
    "PROPERTIES",
    "DEPEND",
    "RDEPEND",
    "BDEPEND",
    "PDEPEND",
    "IDEPEND",
    "DEFINED_PHASES",
    "INHERIT",
];

/// Fields where token order does not affect semantic equivalence.
const UNORDERED_KEYS: &[&str] = &[
    "SRC_URI",
    "LICENSE",
    "IUSE",
    "KEYWORDS",
    "REQUIRED_USE",
    "RESTRICT",
    "PROPERTIES",
    "DEPEND",
    "RDEPEND",
    "BDEPEND",
    "PDEPEND",
    "IDEPEND",
];

const STRUCTURAL_TOKENS: &[&str] = &["(", ")", "||", "&&"];

fn token_multiset(s: &str) -> BTreeMap<&str, usize> {
    let mut map = BTreeMap::new();
    for tok in s.split_whitespace() {
        *map.entry(tok).or_insert(0) += 1;
    }
    map
}

fn find_extra_duplicates<'a>(ref_val: &'a str, src: &'a str) -> Vec<String> {
    let mut ref_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for tok in ref_val.split_whitespace() {
        if !STRUCTURAL_TOKENS.contains(&tok) {
            *ref_counts.entry(tok).or_insert(0) += 1;
        }
    }
    let mut src_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for tok in src.split_whitespace() {
        if !STRUCTURAL_TOKENS.contains(&tok) {
            *src_counts.entry(tok).or_insert(0) += 1;
        }
    }
    src_counts
        .into_iter()
        .filter(|(tok, src_n)| *src_n > *ref_counts.get(tok).unwrap_or(&0))
        .map(|(tok, _)| tok.to_string())
        .collect()
}

fn parse_cache_map(serialized: &str) -> BTreeMap<&str, &str> {
    let mut map = BTreeMap::new();
    for line in serialized.lines() {
        if let Some((key, value)) = line.split_once('=') {
            map.insert(key, value);
        }
    }
    map
}

fn matches_filter(cpv: &str, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    if let Some(prefix) = filter.strip_suffix('*') {
        cpv.starts_with(prefix)
    } else {
        cpv == filter
    }
}

struct FieldDiff {
    cpv: String,
    key: String,
    expected: String,
    got: String,
}

#[derive(Parser)]
#[command(about = "Source all ebuilds and compare against the md5-cache")]
struct Args {
    /// Path to the repository
    repo: String,
    /// Optional category/package glob filter (e.g. 'dev-lang/*')
    filter: Option<String>,
    /// Directory containing master repositories
    #[arg(long, value_name = "DIR")]
    repos_dir: Option<String>,
    /// Number of parallel workers (default: available CPUs)
    #[arg(short = 'j', long)]
    jobs: Option<usize>,
    /// Suppress per-ebuild progress output
    #[arg(short, long)]
    quiet: bool,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let jobs = args.jobs.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    });

    let repo = if let Some(ref dir) = args.repos_dir {
        match Repository::builder()
            .in_memory_cache()
            .open_with_masters(&args.repo, dir, None)
        {
            Ok(r) => {
                if !r.masters().is_empty() {
                    let names: Vec<&str> = r.masters().iter().map(|m| m.name()).collect();
                    eprintln!("Resolved masters: {}", names.join(", "));
                }
                r
            }
            Err(e) => {
                eprintln!("Error opening repository with masters: {e}");
                process::exit(1);
            }
        }
    } else {
        match Repository::builder().in_memory_cache().open(&args.repo) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Error opening repository: {e}");
                process::exit(1);
            }
        }
    };

    eprintln!("Collecting ebuilds...");
    let ebuilds = match repo.ebuilds() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Error collecting ebuilds: {e}");
            process::exit(1);
        }
    };

    let ebuilds: Vec<Ebuild> = if let Some(ref f) = args.filter {
        let f = f.clone();
        ebuilds
            .into_iter()
            .filter(move |eb| matches_filter(&eb.cpv().to_string(), &f))
            .collect()
    } else {
        ebuilds.into_iter().collect()
    };

    let total = ebuilds.len();
    eprintln!("Found {total} ebuilds to process with {jobs} workers.");

    let repo = Arc::new(repo);
    let progress = Arc::new(AtomicUsize::new(0));
    let errors = Arc::new(AtomicUsize::new(0));
    let mismatches = Arc::new(AtomicUsize::new(0));
    let missing_cache = Arc::new(AtomicUsize::new(0));
    let success = Arc::new(AtomicUsize::new(0));
    let diffs: Arc<Mutex<Vec<FieldDiff>>> = Arc::new(Mutex::new(Vec::new()));

    let ctx = SourceContext::new();
    let opts = SourceOpts {
        jobs: Some(jobs),
        dedup: false,
    };

    let source_rx = source_parallel(&repo, ebuilds, &opts, &ctx);
    // Drain on several tasks so comparison work stays parallel with sourcing.
    let mut handles = Vec::new();
    for _ in 0..jobs {
        let source_rx = source_rx.clone();
        let repo = Arc::clone(&repo);
        let progress = Arc::clone(&progress);
        let errors = Arc::clone(&errors);
        let mismatches = Arc::clone(&mismatches);
        let missing_cache = Arc::clone(&missing_cache);
        let success = Arc::clone(&success);
        let diffs = Arc::clone(&diffs);
        let quiet = args.quiet;

        handles.push(tokio::spawn(async move {
            while let Ok((ebuild, result)) = source_rx.recv_async().await {
                let i = progress.fetch_add(1, Ordering::Relaxed) + 1;
                let cpv = ebuild.cpv();
                let cpv_str = cpv.to_string();
                if !quiet {
                    eprint!("\r[{i}/{total}] {cpv_str:<60}");
                }

                let metadata = match result {
                    Err(e) => {
                        eprintln!("\nERROR sourcing {cpv_str}: {e}");
                        errors.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    Ok(s) => s,
                };

                let reference = match repo.cache_entry(cpv) {
                    Ok(Some(c)) => c,
                    Ok(None) => {
                        eprintln!("\nMISSING cache for {cpv_str}");
                        missing_cache.fetch_add(1, Ordering::Relaxed);
                        success.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    Err(e) => {
                        eprintln!("\nERROR reading cache for {cpv_str}: {e}");
                        errors.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                };

                let ebuild_md5 = fs::read(ebuild.path())
                    .map(|b| format!("{:x}", md5::compute(&b)))
                    .ok();

                let portage_repo::source::SourcedEbuild {
                    metadata,
                    eclasses: eclass_paths,
                } = metadata;
                let eclasses: Vec<(String, String)> = eclass_paths
                    .into_iter()
                    .filter_map(|(name, path)| {
                        fs::read(path.as_std_path())
                            .ok()
                            .map(|data| (name, format!("{:x}", md5::compute(&data))))
                    })
                    .collect();

                let sourced_entry = CacheEntry {
                    metadata,
                    md5: ebuild_md5,
                    eclasses,
                };

                let ref_serialized = reference.serialize();
                let src_serialized = sourced_entry.serialize();

                let ref_map = parse_cache_map(&ref_serialized);
                let src_map = parse_cache_map(&src_serialized);

                let mut has_diff = false;
                let mut new_diffs = Vec::new();
                for &key in COMPARE_KEYS {
                    let ref_val = ref_map.get(key).copied().unwrap_or("");
                    let src_val = src_map.get(key).copied().unwrap_or("");

                    if UNORDERED_KEYS.contains(&key) && !src_val.is_empty() {
                        let dups = find_extra_duplicates(ref_val, src_val);
                        if !dups.is_empty() {
                            eprintln!(
                                "\nWARN {cpv_str} {key}: extra duplicate tokens: {}",
                                dups.join(", ")
                            );
                        }
                    }

                    let differs = if UNORDERED_KEYS.contains(&key) {
                        token_multiset(ref_val) != token_multiset(src_val)
                    } else {
                        ref_val != src_val
                    };

                    if differs {
                        has_diff = true;
                        new_diffs.push(FieldDiff {
                            cpv: cpv_str.clone(),
                            key: key.to_string(),
                            expected: ref_val.to_string(),
                            got: src_val.to_string(),
                        });
                    }
                }

                if has_diff {
                    mismatches.fetch_add(1, Ordering::Relaxed);
                    diffs.lock().unwrap().extend(new_diffs);
                }
                success.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }
    for h in handles {
        if let Err(e) = h.await {
            eprintln!("\nFatal error: {e}");
            process::exit(1);
        }
    }

    if !args.quiet {
        eprintln!();
    }

    let mut all_diffs = diffs.lock().unwrap();
    all_diffs.sort_by(|a, b| a.cpv.cmp(&b.cpv).then(a.key.cmp(&b.key)));

    if !all_diffs.is_empty() {
        eprintln!("=== Field diffs ===");
        for d in all_diffs.iter() {
            eprintln!("DIFF {} {}:", d.cpv, d.key);
            eprintln!("  cache: {}", d.expected);
            eprintln!("  got:   {}", d.got);
        }
        eprintln!();
    }

    let err_count = errors.load(Ordering::Relaxed);
    let mismatch_count = mismatches.load(Ordering::Relaxed);

    println!("=== Results ===");
    println!("Total:         {total}");
    println!("Sourced OK:    {}", success.load(Ordering::Relaxed));
    println!("Errors:        {err_count}");
    println!("Mismatches:    {mismatch_count}");
    println!("Missing cache: {}", missing_cache.load(Ordering::Relaxed));

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

    if err_count > 0 || mismatch_count > 0 {
        process::exit(1);
    }
}
