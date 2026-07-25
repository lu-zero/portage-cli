# Official scripts: dirty tree (56a16bc + uncommitted)

- **date (UTC):** 2026-07-25T22:07–22:14
- **em:** dirty working tree on top of `56a16bc`
- **scripts:** `compare-search.sh`, `bench-em-vs-emerge.sh` (RUNS=5)
- **search tree:** `benchmarks/gentoo` @ `0833c9f` (pin)
- **`-p` tree:** host repos (script default; no `GENTOO_REPO`)

## 1) compare-search (pin)

| tool | pattern | mean |
|------|---------|------|
| em | gcc | **0.088 s** |
| em | python | **0.096 s** |
| em | firefox | **0.100 s** |
| em | rust | **0.088 s** |
| emerge | gcc | 5.814 s |
| qsearch | gcc | 0.136 s |

Matches formal sweep search (~0.09–0.11 s). Healthy.

## 2) bench-em-vs-emerge

### Parity

Same pre-existing failures as baseline (texlive 89 diffs; firefox/thunderbird/libreoffice 7).

### Timing (table re-run means)

| Benchmark | em | emerge | speedup |
|-----------|-----|--------|---------|
| firefox -p | **1.290 s** | 4.061 s | 3.15× |
| libreoffice -p | **1.784 s** | 4.052 s | 2.27× |
| multi (5 pkgs) -p | **1.996 s** | 5.193 s | 2.60× |
| gcc -s | **0.163 s** | 5.809 s | 35.6× |

vs **2026-07-11 history** (firefox 0.76–0.9 s, ~4–5× emerge): **regressed**.

See sibling dir `20260725-222000-baseline-2afb668-confirm` — same slow `-p` on clean `2afb668`.
