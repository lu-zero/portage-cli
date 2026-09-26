//! The gap-index fast path, and what a repo without a sync marker costs.
//!
//! `repo_entries` finds "suspects" — packages the in-tree md5-cache does not
//! serve correctly — by walking every ebuild and comparing digests. That walk is
//! most of a resolve's repo-load time, so the answer is memoised in a sidecar
//! keyed on the repo's sync stamp. Two questions this bench answers:
//!
//! 1. **Marked tree, populated gap list.** Every gap line is looked up in the
//!    primary entry set. A linear scan makes that O(gap x primary); an index
//!    makes it O(n). Sized so the difference is far above this machine's noise.
//! 2. **Unmarked tree.** No `timestamp.chk`, so the stamp is untrustworthy, the
//!    memo is never used, and every resolve pays the full walk. This is the
//!    per-resolve price of adding an overlay that is not git/rsync-synced.
//!
//! Fixtures follow `entries.rs`'s own test setup: a valid primary cache entry
//! with `_md5_` matching the ebuild, ebuild mtime older than the cache file's.

use std::hint::black_box;
use std::time::{Duration, SystemTime};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use portage_repo::{Repository, repo_entries};

const GAP_INDEX: &str = "gap-index";

fn set_mtime(path: &std::path::Path, t: SystemTime) {
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_times(std::fs::FileTimes::new().set_modified(t))
        .unwrap();
}

/// A repo of `count` ebuilds, each with a matching primary cache entry.
///
/// `sync_marker` false omits `metadata/timestamp.chk`, which is what makes the
/// stamp — and therefore the memo — untrustworthy.
fn make_repo(
    dir: &std::path::Path,
    cache_root: &std::path::Path,
    count: usize,
    sync_marker: bool,
) -> Repository {
    std::fs::create_dir_all(dir.join("metadata")).unwrap();
    std::fs::write(dir.join("metadata/layout.conf"), "").unwrap();
    std::fs::create_dir_all(dir.join("profiles")).unwrap();
    std::fs::write(dir.join("profiles/categories"), "bench\n").unwrap();
    if sync_marker {
        std::fs::write(dir.join("metadata/timestamp.chk"), "1\n").unwrap();
    }
    let repo = Repository::builder()
        .user_cache_root(
            camino::Utf8PathBuf::try_from(cache_root.to_owned()).expect("utf-8 cache root"),
        )
        .open(dir)
        .unwrap();

    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    for i in 0..count {
        let pkg = format!("pkg{i:06}");
        let ebuild = format!("EAPI=8\nDESCRIPTION=\"d{i}\"\nSLOT=0\n");
        let pkg_dir = dir.join("bench").join(&pkg);
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let ebuild_path = pkg_dir.join(format!("{pkg}-1.0.ebuild"));
        std::fs::write(&ebuild_path, &ebuild).unwrap();

        let cache_file = dir.join(format!("metadata/md5-cache/bench/{pkg}-1.0"));
        std::fs::create_dir_all(cache_file.parent().unwrap()).unwrap();
        std::fs::write(
            &cache_file,
            format!(
                "EAPI=8\nDESCRIPTION=d{i}\nSLOT=0\n_md5_={:x}\n",
                md5::compute(ebuild.as_bytes())
            ),
        )
        .unwrap();

        set_mtime(&ebuild_path, base);
        set_mtime(&cache_file, base + Duration::from_secs(60));
    }
    repo
}

/// Write a gap index naming the first `gap` of `count` packages.
///
/// Those entries are in fact served correctly by the primary cache, so the fast
/// path accepts every line and returns early — which is exactly the case that
/// stresses the lookup.
fn write_gap_index(repo: &Repository, gap: usize) {
    let stamp = repo.sync_stamp().expect("marked tree has a stamp");
    let sidecar = repo.sidecar_path(GAP_INDEX).expect("durable secondary");
    std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
    let mut text = stamp;
    for i in 0..gap {
        text.push_str(&format!("\nbench/pkg{i:06}-1.0"));
    }
    std::fs::write(&sidecar, text).unwrap();
}

struct Fixture {
    _tree: tempfile::TempDir,
    _cache: tempfile::TempDir,
    repo: Repository,
    expected: usize,
}

fn fixture(count: usize, gap: usize) -> Fixture {
    let tree = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let repo = make_repo(tree.path(), cache.path(), count, true);
    if gap > 0 {
        write_gap_index(&repo, gap);
    }
    Fixture {
        _tree: tree,
        _cache: cache,
        repo,
        expected: count,
    }
}

fn bench_gap_list(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut group = c.benchmark_group("gap_index");
    for count in [2_000usize, 8_000] {
        for gap in [0usize, count / 10, count] {
            let f = fixture(count, gap);
            let name = format!("primary_{count}_gap_{gap}");
            group.bench_with_input(BenchmarkId::from_parameter(name), &gap, |b, _| {
                b.iter(|| {
                    let entries = rt.block_on(repo_entries(black_box(&f.repo)));
                    assert_eq!(entries.len(), f.expected);
                    black_box(entries)
                });
            });
        }
    }
    group.finish();
}

/// An overlay with no `timestamp.chk` cannot use the memo, so every resolve
/// walks and digests its whole tree. The marked fixture is the control.
fn bench_unmarked_tree(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut group = c.benchmark_group("gap_index_unmarked");
    for count in [500usize, 2_000] {
        for marked in [true, false] {
            let tree = tempfile::tempdir().unwrap();
            let cache = tempfile::tempdir().unwrap();
            let repo = make_repo(tree.path(), cache.path(), count, marked);
            if marked {
                write_gap_index(&repo, count);
            }
            group.bench_with_input(
                BenchmarkId::new(format!("ebuilds_{count}"), format!("marked_{marked}")),
                &marked,
                |b, _| {
                    b.iter(|| {
                        let entries = rt.block_on(repo_entries(black_box(&repo)));
                        assert_eq!(entries.len(), count);
                        black_box(entries)
                    });
                },
            );
        }
    }
    group.finish();
}

criterion_group!(benches, bench_gap_list, bench_unmarked_tree);
criterion_main!(benches);
