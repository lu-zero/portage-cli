# Confirm last formal commit: 2afb668

- **date (UTC):** 2026-07-25T22:19–22:26
- **em:** clean worktree at `2afb668` (`/tmp/portage-cli-bench-2afb668`)
- **note:** this is the commit recorded in `bench-results/20260723-101215-2afb668/`
  (sweep measured search/regen/criterion only — **not** `-p`)
- **search tree:** `benchmarks/gentoo` @ `0833c9f`
- **`-p`:** host repos via `bench-em-vs-emerge.sh`

## 1) compare-search (pin) — confirms formal search numbers

| tool | pattern | mean | formal sweep (2026-07-23) |
|------|---------|------|---------------------------|
| em | gcc | **0.072 s** | 0.096 s |
| em | firefox | **0.088 s** | 0.092 s |
| em | rust | **0.086 s** | 0.108 s |

**Confirmed:** search still ~0.07–0.10 s on pin. No search regression.

## 2) bench-em-vs-emerge — the missing `-p` baseline

| Benchmark | em @ 2afb668 | emerge | vs 2026-07-11 history |
|-----------|--------------|--------|------------------------|
| firefox -p | **1.402 s** (first block 1.312 s) | ~4.09 s | history **0.76–0.9 s** — **already regressed at 2afb668** |
| libreoffice -p | **1.760 s** | ~4.09 s | history **~0.94 s** — **already regressed** |
| multi -p | **1.842 s** | ~5.26 s | history **~0.98 s** |
| gcc -s | **0.139 s** | ~5.77 s | OK (~0.1 s) |

Parity package diffs **identical** to dirty run (not introduced by uncommitted work).

## 3) pin A/B: 2afb668 vs dirty (`em --repo benchmarks/gentoo -p`)

| command | 2afb668 | dirty | |
|---------|---------|-------|---|
| -p firefox | 1.516 s | **1.347 s** | dirty ≈ same / slightly faster |
| -p libreoffice | 1.645 s | 1.709 s | noise |

**Conclusion:** uncommitted metadata-cache / builder / active work is **not** the `-p`
slowdown. The ~1.3–1.8 s `-p` times are already present at the last formal commit;
the 2026-07-11 ~0.8 s numbers have not held since, and the formal sweep never re-checked `-p`.
