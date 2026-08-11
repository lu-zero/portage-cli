# Repo/overlay abstraction redesign: fresh anchor + quick-vs-release, 2026-08-11

- **current:** `2b79d47` (HEAD after the full RepoSet redesign — see
  [[repo-overlay-abstraction-redesign]] / `todo/repo-overlay-abstraction-redesign.md`)
- **why:** after landing the redesign and its post-landing perf follow-ups
  (`1a01a0f`, `684ad7a`, `7a354eb`), record a fresh criterion anchor point
  and confirm quick vs release show no meaningful difference for the
  actual CLI paths touched by the redesign.

## 1. Criterion `resolve` bench — fresh anchor, partial delta vs 2026-07-25

Single run, no interleaved baseline binary (criterion compares against its
own last locally-saved run under `target/criterion/`, last recorded
2026-07-25 — a ~2.5 week gap covering unrelated changes too, not a
controlled A/B for just this session). Full stdout tail:
`resolve-bench-tail-2b79d47.txt` (the earlier portion, covering
`load_repo`/`build_provider`/`firefox`/`gcc`, was lost to a `tail -80`
truncation mistake and criterion had already rotated its baseline files
by the time this was noticed, so no delta is available for those four —
only fresh absolute values).

**Caveat:** `resolve_load`'s `load_repo`/`build_provider` targets use
`Repository::builder().in_memory_cache().open()` plus raw
`category()`/`package()`/`cache_entry()` calls directly — this bench does
**not** exercise `RepoSet`, `repo_entries()`, or `EbuildsAcross` at all.
It's the project's established "resolve" anchor, not a direct test of
this session's changes. Section 2 below is the one that actually
exercises the redesigned code path.

| target | delta vs 2026-07-25 | note |
|---|---|---|
| `load_repo` | — (lost to truncation) | fresh anchor: 1251.65 ms |
| `build_provider` | — (lost to truncation) | fresh anchor: 540.0 ms |
| `firefox` | — (lost to truncation) | fresh anchor: 12.079 ms |
| `gcc` | — (lost to truncation) | fresh anchor: 4.485 ms |
| `rust` | +0.09% | criterion: "No change in performance detected" |
| `openssh` | -0.53% | criterion: "Change within noise threshold" |
| `python` | -0.15% | criterion: "Change within noise threshold" |

All `target/criterion/` data has been rotated forward — this run is now
the saved baseline for the next comparison.

## 2. CLI wall-clock, quick vs release — the path that actually changed

Same-run interleaved `hyperfine` (`--warmup 2 --runs 8 -i`), real host
repo (`gentoo` + `guru` + `crossdev` + `exp-llvm-libc` configured), both
binaries built from the identical `2b79d47` tree, no concurrent cargo
builds (confirmed via `ps aux` before each run — see
[[never-benchmark-during-a-background-build]]).

| target | quick | release | delta |
|---|---|---|---|
| `www-client/firefox` | 1.059 s ± 0.039 s | 1.042 s ± 0.037 s | release 1.02x faster |
| `sys-devel/gcc` | 879.8 ms ± 22.5 ms | 889.1 ms ± 19.9 ms | quick 1.01x faster |
| `net-misc/openssh` | 878.2 ms ± 35.8 ms | 898.0 ms ± 47.8 ms | quick 1.02x faster |

All three within run-to-run noise (±2-5%), no consistent direction —
matches the [[repo-overlay-abstraction-redesign]] finding for
`which`/`depends`/`-p @world` earlier the same day. `quick` (thin LTO)
remains safe to use for iteration and even ad-hoc benchmarking in this
area; `lto = true` vs `lto = "thin"` doesn't move the needle for this
codebase's I/O- and parse-dominated workload.

## Conclusion

No regression detected by either method. The criterion anchor is now
current (2026-08-11, `2b79d47`) for future comparisons. The redesign's
actual perf story is the one already established via targeted hyperfine
work earlier the same day: `which` unchanged, `depends` ~2x faster,
`-p @world` unchanged/slightly faster.
