# em wall-clock: search + -p

- **date (UTC):** 2026-07-25T21:51:10+00:00
- **host:** thalia (aarch64, 128 cores)
- **git:** 56a16bc (working tree **dirty** — uncommitted metadata-cache / builder / active work)
- **em:** `target/release/em` 0.1.0
- **repo:** /var/db/repos/gentoo

## What was run

1. `cargo bench -p portage-bench --bench resolve` (earlier same session)
2. hyperfine `em search` / `em -s` (5 runs, warmup 1)
3. hyperfine `em -p` packages (5 runs, warmup 1; `--ignore-failure` for autounmask exit 1)
4. hyperfine emerge baselines (3 runs)
5. `benchmarks/scripts/compare-search.sh` (em vs emerge -s vs qsearch)

## Historical references (BENCHMARKS.md / thalia 2026-07-11)

| metric | historical |
|--------|------------|
| em -p firefox | **0.76–0.9 s** (after bdepend_trim fix) |
| em -p libreoffice | **~0.94 s** |
| em vs emerge -p firefox | **~4–5×** faster |
| em search / `-s` gcc | **~0.1 s** vs emerge ~5 s |

## Criterion resolve (this session)

| bench | this run | historical (BENCHMARKS.md) |
|-------|----------|----------------------------|
| load_repo | **1.25 s** | 1.225–1.274 s |
| build_provider | **~0.53 s** | ~0.58–0.63 s |
| solve firefox (pubgrub only) | **11.6 ms** | ~6.7 ms (different config/machine notes) |


## Results (mean wall time)

### em search / -s

| command | mean | min | max |
|---------|------|-----|-----|
| `em search gcc` | **0.084s** | 0.061s | 0.116s |
| `em search python` | **0.100s** | 0.090s | 0.108s |
| `em search firefox` | **0.091s** | 0.073s | 0.101s |
| `em search rust` | **0.095s** | 0.083s | 0.109s |
| `em -s gcc` | **0.158s** | 0.102s | 0.333s |

### em -p

| command | mean | min | max |
|---------|------|-----|-----|
| `em -p firefox` | **1.444s** | 1.316s | 1.648s |
| `em -p gcc` | **1.075s** | 0.962s | 1.312s |
| `em -p openssh` | **1.023s** | 0.957s | 1.103s |
| `em -p python` | **1.083s** | 0.964s | 1.206s |
| `em -p qtbase` | **1.327s** | 1.217s | 1.555s |
| `em -p libreoffice` | **1.686s** | 1.559s | 1.856s |

### emerge baselines

| command | mean | min | max |
|---------|------|-----|-----|
| `emerge -p firefox` | **4.094s** | 4.045s | 4.123s |
| `emerge -s gcc` | **5.812s** | 5.803s | 5.829s |

## vs historical (thalia)

| metric | historical | this run | delta |
|--------|------------|----------|-------|
| em -p firefox | 0.76–0.9s | **1.444s** | +74% **REGRESSION** |
| em -p libreoffice | ~0.94s | **1.686s** | +79% **REGRESSION** |
| em -p gcc | (not listed) | n/a | |
| em search gcc | ~0.1s | **0.084s** | -16% faster |
| emerge -p firefox | ~3.7s | **4.094s** | em **2.8×** vs emerge |
| emerge -s gcc | ~5.2s | **5.812s** | em **69×** vs emerge |

### Notes

- `em -p firefox` exits 1 when USE changes are required (autounmask); wall time still measured with hyperfine `--ignore-failure`.
- Working tree dirty: metadata-cache abstraction + `Repository` builder + `em active` / XDG.
- Search is healthy; **`-p` looks ~1.6–1.9× slower than the 2026-07-11 thalia baseline** and should be investigated before treating this as green.
