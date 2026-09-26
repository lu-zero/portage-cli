# Benchmarks Overview (for blogpost / reproducibility)

This document collects **all** benchmark code, data, and reproduction instructions from the portage-cli workspace.

**Current as of HEAD** (confirm with `git rev-parse HEAD` and `cargo check -p portage-bench --benches`).

All microbenchmarks use [criterion](https://crates.io/crates/criterion). Wall-clock use [hyperfine](https://github.com/sharkdp/hyperfine).

## Machines

Per-machine hardware characterization, NUMA details, freq behavior, available trees, and notes for reproducibility are kept in `benchmarks/machines/`.

See `machines/README.md` for overview.

- [thalia](machines/thalia.md) — Current AmpereOne server (128 cores, 4 NUMA nodes, 256 GiB). Use this for new runs. Includes recommendations for `numactl` binding.
- [mneme](machines/mneme.md) — Apple M2 Max laptop. Historical data from prior runs; **fresh full benchmark runs (micro + sweep + comparisons) to be executed later** on the actual M2 hardware (user: "we'll run the benchmarks on the m2 later"). The machine file is prepped with macOS-specific repro instructions, sysctl characterization, verification notes (md5), etc.

When adding a new machine (or fresh run data), create/update `machines/<name>.md` with characterization output + notes, then link here.

**Always** record and link the relevant machine .md when publishing new benchmark results. Old numbers from M2 Max are not directly comparable to server-class AmpereOne runs due to core count, memory system, NUMA, etc.

## Collected Runs and Data

All historical and partial runs have been consolidated into the per-machine files:
- See `machines/mneme.md` (the M2 Max / "mneme") for full collected historical tables (regen, resolve/solver, parsing, interner micro, search, etc. from MEMORY.md, PROFILES.md, results.md, gentoo-interner/README.md, and prior thalia partials). It now includes a "Collected Historical Runs on mneme" section and detailed prep/instructions for new comparative runs. It also lists **all benchmark bash scripts in the specific crates** (portage-repo/bench*.sh, root bench-regen.sh, benchmarks/ scripts, etc.) for running on mneme.
- See `machines/thalia.md` for current server data and partial fresh numbers.

When new runs are done on mneme, append results + characterization to `machines/mneme.md` and update consolidated views here or in MEMORY.md.

Note: many of the bash scripts for regen and comparisons live in the specific crates (e.g. portage-repo/ has dedicated bench-regen.sh, bench.sh, bench-pk.sh for lib-level and pk comparisons; root has bench-regen.sh for em). Use the per-crate ones when benchmarking components on mneme, in addition to the higher-level ones in benchmarks/. See machines/mneme.md for the full list and usage.

## Fresh Numbers from thalia (AmpereOne) - Partial Run

See `machines/thalia.md` for full specs.

While waiting for clean state (no other builds/numactl users), a reduced-sample run of dep_parsing bench was completed (using default features, all-NUMA bind).

**IUse::parse_line microbenchmarks (this machine):**

```
comparison/IUse::parse_line/pkgcraft/small
                        time:   [509.72 ns 509.79 ns 509.83 ns]

comparison/IUse::parse_line/portage-metadata/large
                        time:   [6.3247 µs 6.3266 µs 6.3285 µs]

comparison/IUse::parse_line/pkgcraft/large
                        time:   [8.4182 µs 8.4191 µs 8.4214 µs]
```

(Notes from run: "performance has improved" messages from criterion's change detection vs. previous baseline on this machine. Full interner/resolve/dedup runs were heavy to compile; re-run with `numactl -N0 -m0` once other agents finish.)

These give single-thread parse perf on the 128-core AmpereOne. Add more results to this doc (or a results/ subdir) as they complete, always referencing the machine file.

## Fresh Run on thalia (AmpereOne, 2026-06-14) - Cache Regen and Dep Resolution

See `machines/thalia.md` (the "Full Blown Scans Re-run" section) for full hardware, exact logs, per-crate script outputs (including RSS), verify file counts, and all repro commands. The data below is a summary of the post-fixes re-run using only full blown scans (fresh empty target dirs on the portage-repo/gentoo test tree for regen; default repo discovery for dep -p).

All comparisons executed via the bash scripts that live in the crates (`portage-repo/bench-*.sh`, top-level `bench-regen.sh`, `benchmarks/scripts/compare-regen.sh`, `benchmarks/bench-em-vs-emerge.sh`) after fixing CLI syntax, SCRIPT_DIR setup, and PK discovery paths.

### Cache Regen Comparative on thalia (32k ebuilds test tree, j=8, via compare-regen.sh — em / pk + egencache (opt-in))

| tool | j | run | real      | user      | sys      | Notes |
|------|---|-----|-----------|-----------|----------|-------|
| em   | 8 | 1   | 0m18.120s | 2m11.310s | 0m12.394s | Current em CLI (NUMA0); full cold exhaustive |
| pk   | 8 | 1   | 0m48.263s | 5m25.512s | 1m7.887s  | pkgcraft (NUMA0); full cold exhaustive |

(egencache opt-in with INCLUDE_EGENCACHE=1; uses stock plain sudo rm + sudo egencache -j N --repo gentoo --update on live (repo name defaults to gentoo). 4m37.251s j=20 is the reference. Output is markdown table.)

The compare script includes egencache only with INCLUDE_EGENCACHE=1 (default off) using the *exact plain stock* command:

    sudo rm -rf /var/db/repos/gentoo/metadata/md5-cache
    sudo egencache -j $jobs --repo gentoo --update

(hardcoded live; repo name defaults to gentoo). Output is valid markdown table. This collects the slow full-cold datapoints (4m37s reference). em/pk use your GENTOO_REPO (set to live for same tree). Set SKIP=egencache to disable. No portage source hacked. Earlier "fast" numbers were artifacts. See thalia.md.

Repro (egencache opt-in): `GENTOO_REPO=/var/db/repos/gentoo EM=target/release/em PK=../pkgcraft/target/release/pk INCLUDE_EGENCACHE=1 ./benchmarks/scripts/compare-regen.sh 8 16 20 24 32`

See `machines/thalia.md` for the complete j=8/j=20 tables, per-crate RSS/hyperfine data, dep numbers, and verification that the main test cache was never modified.

### Dependency Resolution Timing (em -p vs emerge -p, hyperfine 5 runs; from bench-em-vs-emerge.sh)

See `machines/thalia.md` ("Dep Resolution Full Scans" subsection) for the full parity table (now emitted as valid markdown by the script) + hyperfine summaries + "RESULT: parity FAILED" note.

The script now prints:
- A markdown table for parity (see above edit).
- Raw hyperfine summaries for timings.
- A consolidated markdown timing summary table (parsed via --export-json + jq).

**SKIP_TIMING=1** skips the entire timing block (only fast parity checks; useful for quick iteration).

Summary (representative from recent runs; re-run for exact — timings vary):
- firefox -p: em ~0.9 s vs emerge ~3.7 s (**~4×**)
- libreoffice -p: em ~1.0 s vs emerge ~4.0 s (**~4×**)
- multi (5 pkgs) -p: em ~1.0 s vs emerge ~4.7 s (**~4.5×**)
- gcc -s: em ~0.1 s vs emerge ~5.2 s (**~50×**)

Repro: `EM=target/release/em ./benchmarks/bench-em-vs-emerge.sh` (or `RUNS=5`, `SKIP_TIMING=1` for parity only). Output from runs is easy to copy-paste as md tables now.

Parity excellent on most; documented small diffs on texlive-core + multi (emerge backtracking vs em full graph). Full details in thalia.md.

This data + historical from mneme will be used for blogpost tables. Repro via the scripts in crates (as noted in thalia.md).

## Update 2026-07-11: performance regression found and fixed on thalia (commit `9cff6ff`)

A user-flagged regression ("the speed regression is severe") turned out to be real: `em -p www-client/firefox`
had drifted from the `~0.9s` / `~4×` baseline above to **~2.1s (~1.7×)**. Root-caused via automated `git bisect run`
across ~212 commits to `762e6456` (2026-07-05, "check USE-dep brackets against installed VDB packages"), which made
`bdepend_avail.rs` eagerly read USE/IUSE for every installed package (712 on the dev host) at `Avail` construction —
almost all of it never actually checked against a USE-dep atom. Fixed, then kept digging: a follow-up allocation/
interning audit and a `dhat`-heap profiling pass (new `dhat-heap` cargo feature, off by default) found several more
real costs, culminating in the actual headline fix — `bdepend_trim::avail_for_consumer` was rebuilding the entire
BROOT/prefix VDB scan from scratch for every `(candidate, consumer)` pair in the post-solve BDEPEND trim, an O(n²)
cost that dhat measured as **110MB across just 322 calls**, dwarfing every other allocation site in the profile.
Hoisting that scan to run once per trim pass (nothing mutates the VDB mid-trim, so it's safe to reuse) closed the
remaining gap.

Net result: not just recovered, but faster than the original baseline — multi-target plans roughly **halved**
versus this session's starting point, since `bdepend_trim`'s O(n²) cost scales with plan size.

| Target | Before this session | After (now) | em's own speedup | em vs. emerge (now) |
|---|---|---|---|---|
| firefox `-p` | 2.1s | **0.76s** | 2.76× | **4.85×** |
| libreoffice `-p` | — | **0.94s** | — | **4.21×** |
| multi (5 pkgs) `-p` | ~2.0s | **0.98s** | 2.04× | **4.75×** |
| gcc `-s` | — | **0.16s** | — | **14.7×** |

Repro: `cargo build --release -p portage-cli && ./benchmarks/bench-em-vs-emerge.sh`. For allocation profiling:
`cargo build --release -p portage-cli --features dhat-heap`, run against a target that resolves cleanly (exit 0 —
`process::exit` on the "config changes needed" path skips the profiler's `Drop`, so a target needing USE changes
never writes `dhat-heap.json`), then load the file at
[dh_view.html](https://nnethercote.github.io/dh_view/dh_view.html).

Fourteen commits total, each independently verified (full test suite via `cargo nextest run`, clippy, fmt, live
parity + timing) — see `git log` from `12ed0bf` through `9cff6ff` for the complete, individually-described chain
(crossdev preflight fix, `host_copies` interleave correctness fix, the two-part regression root cause, five
allocation/interning fixes, the `dhat-heap` tooling addition, and the `bdepend_trim` O(n²)→O(n) fix).

## Update 2026-07-12: `UseConfig`/`ForceMask` per-version cost cut (commit `d4091a3`)

`todo/useconfig-clone-elimination.md` had proposed making the solver's
per-version `VersionData.desired` a `Cow<UseConfig>` so a package with no
local IUSE/`package.use` match could borrow the global config instead of
cloning it. An independent review (Fable model) of that plan, done *before*
implementation, found the premise false: `desired_use` already forces
`.into_owned()` before `ForceMask::apply` runs, and `ForceMask::apply`
mutates `cfg` for essentially every real package (`ForceMask::is_empty()`
requires the *global* `use.mask` to be empty too, which no real profile
has) — so the proposed "free borrow" path would never fire in practice.

The review found the actual dominant per-version cost instead:
`ForceMask::effective` unconditionally scanned the *entire* global
`use.mask` (hundreds of entries on a real profile) and disabled every one
on `cfg`, per version, regardless of whether the package's own `IUSE` even
declares that flag. Fixed by restricting that scan to the package's own
`IUSE` set (safe: `UseConfig` already defaults an absent flag to
`Disabled`, same as an explicit `disable()` would). Also fixed
`apply_package_use`, which cloned whenever the `package.use` list was
non-empty rather than when an entry actually matched the package — true on
every call on a real profile, since *some* `package.use` entry always
exists somewhere in the system.

| Target | Before | After | Speedup |
|---|---|---|---|
| `dev-qt/qtwebengine` `-p` (82-pkg plan) | 755.6 ms | 701.7 ms | ~8% |
| `app-office/libreoffice` `-p` (134-pkg plan) | 874.9 ms | 810.9 ms | ~8% |
| `sys-devel/gcc` `-p` (16-pkg plan, light IUSE) | 520.0 ms | 523.1 ms | none (below noise floor, as expected) |

The win scales with plan size × IUSE richness (what the fixed scan is
proportional to), not a fixed per-invocation overhead — small plans see
nothing, which matches the earlier estimate in
`todo/useconfig-clone-elimination.md` that the *originally proposed* fix
would be below the noise floor end-to-end. This one wasn't, because it
targeted the actual dominant cost instead of the one the doc guessed at.

Parity unchanged (`benchmarks/bench-em-vs-emerge.sh SKIP_TIMING=1`): same
pre-existing diff counts on firefox/thunderbird/libreoffice, both before
and after this change. Bonus: the `ForceMask` fix incidentally corrected a
latent over-masking bug — the pre-fix binary carried two phantom packages
(`virtual/libintl`, `virtual/libiconv`) into the `sys-devel/gcc` plan that
real `ROOT=<dir> emerge -vp sys-devel/gcc` does not include; post-fix, the
two plans match exactly (16/16 packages, byte-identical USE flags).

Repro: `cargo build --release -p portage-cli`, then the two-binary
`hyperfine` recipe in [`docs/design/benchmarks.md`](../docs/design/benchmarks.md#before-after-comparisons-for-a-specific-change).

## Locations of Benchmarks

### Central harness & scripts
- `benchmarks/` (member `portage-bench`)
  - `benches/*.rs`: dep_parsing, realworld_dep_parsing, resolve, dedup, repo_load, avail_lookup, trim_scaling, graph_scaling, vdb_snapshot (criterion)
  - `src/main.rs`: custom solver comparison tool (used for profiling)
  - `scripts/`: bench-sweep.sh, bench-eval.sh, compare-*.sh, maint.sh
  - `bench-em-vs-emerge.sh`: parity + timing vs real emerge (plain `-p`/`-s`)
  - `bench-resolve-modes.sh`: `-p`/`-up`/`-uNp`/`-uDp`/`-uNDp` counts + hyperfine vs emerge
  - `bench-elfscan.sh`: wall-clock install-image ELF scan (`em` serial/parallel vs Portage `scanelf`)
  - Data: `MEMORY.md`, `PROFILES.md`, `results.md`, `README.md`

### Per-crate microbenchmarks
- `gentoo-interner/benches/interner.rs` + tables in `gentoo-interner/README.md`
- `portage-atom/benches/parsing.rs` (compares to pkgcraft baseline)
- `portage-atom-resolvo/benches/parsing.rs`
- `portage-vdb/benches/vdb.rs`, `portage-vdb/benches/ownership.rs`
- `portage-cli/benches/elfscan.rs` — criterion serial vs parallel `scan_image` (merge-time NEEDED.ELF.2 path)

**No benches** in: portage-repo, portage-metadata, gentoo-core, gentoo-stages, portage-distfiles (they are exercised via the central ones or examples).

See also:
- `docs/design/benchmarks.md` — quick-start map of what to run and where (this file is the historical record/data)
- `docs/design/architecture.md` (mentions portage-bench)

## How to Run / Reproduce (current workspace)

From the portage-cli root:

```sh
# Microbenchmarks only (fast)
cargo bench -p portage-bench                    # all central criterion benches
cargo bench -p portage-bench --bench resolve    # solver on real Gentoo data

# With different interner (see features in benchmarks/Cargo.toml)
cargo bench -p portage-bench --no-default-features --features lasso

# For gentoo-interner specifically
cargo bench -p gentoo-interner --bench interner
cargo bench -p gentoo-interner --bench interner --no-default-features --features symbol-table
```

### ELF image scan (merge-time NEEDED / vs Portage scanelf)

Portage builds `NEEDED.ELF.2` with pax-utils `scanelf` on the install image.
`em` uses `portage_cli::elfscan::scan_image` (object crate + flume workers).

```sh
# Criterion: serial (jobs=1) vs parallel (available_parallelism)
cargo bench -p portage-cli --bench elfscan
ELFSCAN_BENCH_DIR=/path/to/image cargo bench -p portage-cli --bench elfscan

# Wall-clock vs scanelf (needs hyperfine; pax-utils for Portage side)
./benchmarks/bench-elfscan.sh                 # default /usr/lib64
./benchmarks/bench-elfscan.sh /path/to/image
SKIP_SCANELF=1 RUNS=10 ./benchmarks/bench-elfscan.sh

# One-shot harness (used by the script)
cargo run -p portage-cli --release --example elfscan_bench -- --jobs 8 /usr/lib64
```

### Full wall-clock + comparison sweeps (needs Gentoo tree + hyperfine)

```sh
# One-time setup
git clone --depth 1 https://github.com/gentoo/gentoo.git gentoo   # ~200-300 MB
cargo install hyperfine

# Optional: pkgcraft for baselines (pinned rev in Cargo.toml; local override via .cargo/config.toml)
# git clone https://github.com/pkgcraft/pkgcraft ../pkgcraft

# Run sweep (builds per config, runs criterion + regen + search + baselines)
cd benchmarks
./scripts/bench-sweep.sh                    # all 6 configs (interner x alloc)
./scripts/bench-sweep.sh --configs papaya-mimalloc --no-criterion

# Evaluate results into tables
./scripts/bench-eval.sh                     # latest
./scripts/bench-eval.sh -o my-report.md     # to file

# Quick CLI comparisons (assumes release build of em)
../bench-em-vs-emerge.sh target/release/em   # or set EM=...
./scripts/compare-regen.sh
./scripts/compare-search.sh
```

**Hardware note for historical results**: Most numbers below are from Apple M2 Max (12 cores), rustc 1.95 / ~1.92 era. Re-run on your machine for blogpost.

**Reproducibility tips**:
- Use shallow clone for consistent ~31k-32k ebuilds.
- Pin jobs for regen: `--regen-jobs 6` or around core count / 2.
- Always verify cache correctness after regen (file count + aggregate md5 of contents).
- For solver benches, the resolve bench loads a real (or filtered) repo and solves real targets.
- Criterion HTML reports go to `target/criterion/`.
- To match old sweeps: use same interner/allocator features + mimalloc.

See `benchmarks/scripts/README.md` and `benchmarks/README.md` for full options.

## Consolidated Tables from Historical Runs

**Sources & dates** (see individual .md for full context/hardware):
- `benchmarks/MEMORY.md` (2025 data, M2 Max)
- `benchmarks/PROFILES.md` (2025-05-16 arm64 profile)
- `benchmarks/results.md` (regen scaling)
- `gentoo-interner/README.md` (micro + resolve)

### 1. Metadata Cache Regen (wall-clock, full shallow Gentoo ~31k ebuilds, 12 threads)

| Config              | Time     | vs pkgcraft |
|---------------------|----------|-------------|
| **papaya-mimalloc** | **10.10s** | 3.2× faster |
| lasso-mimalloc      | 10.17s   | 3.1× faster |
| symbol-table-mimalloc | 10.10s | 3.1× faster |
| symbol-table-default | 12.04s | 2.6× faster |
| lasso-default       | 12.07s   | 2.6× faster |
| papaya-default      | 13.78s   | 2.3× faster |
| **pkgcraft**        | **31.79s** | baseline  |

*Takeaway*: mimalloc ~20% win. Interner secondary. em ~3× faster than pkgcraft.

### 2. Solver Resolve - Load + Provider Build (criterion, full Gentoo)

| Config              | load_repo | build_provider |
|---------------------|-----------|----------------|
| lasso-mimalloc      | **1.531s** | **265.76ms** |
| papaya-mimalloc     | 1.556s   | 273.59ms     |
| symbol-table-mimalloc | 1.582s | 281.76ms   |
| ... (others slower) | ...      | ...          |

### 3. Solver Solve Targets (selected, ms, papaya-mimalloc often wins with mimalloc)

| Target   | papaya-mimalloc (best in many) | Notes |
|----------|--------------------------------|-------|
| firefox  | **6.711ms**                    |       |
| gcc      | **1.301ms**                    |       |
| rust     | **2.641ms**                    |       |
| openssh  | **0.976ms**                    |       |
| python   | **1.415ms**                    |       |

Full matrix in MEMORY.md.

### 4. Atom/Dep Parsing (criterion, vs pkgcraft)

**Simple/medium/complex synthetic**:
| Benchmark | portage-atom (papaya) | pkgcraft | portage-atom faster |
|-----------|-----------------------|----------|---------------------|
| simple    | 262 ns                | 282 ns   | 7%                  |
| medium    | 1.31 µs               | 1.47 µs  | 12%                 |
| complex   | 3.27 µs               | 3.46 µs  | 5%                  |

**Real-world** (large ebuild RDEPENDs):
| Input   | portage-atom (papaya) | pkgcraft | faster by |
|---------|-----------------------|----------|-----------|
| texlive | 38.96 µs              | 68.18 µs | 75%       |
| pandoc  | 17.53 µs              | 28.52 µs | 63%       |
| ffmpeg  | 31.89 µs              | 45.00 µs | 41%       |

### 5. Interner Backend Microbenchmarks (gentoo-interner, 32-core Linux)

| Workload          | Papaya     | Lasso      | symbol_table | Winner       |
|-------------------|------------|------------|--------------|--------------|
| intern_new/100    | 143 µs     | 120 µs     | **62 µs**    | symbol_table |
| intern_new/1000   | 1.18 ms    | 1.41 ms    | **587 µs**   | symbol_table |
| intern_new/10000  | 24.9 ms    | **9.05 ms**| 14.2 ms      | Lasso        |
| ... (see full in gentoo-interner/README.md) | ... | ... | ... | ... |

Real resolve load:
| Bench                | Papaya | Lasso | symbol_table | Winner |
|----------------------|--------|-------|--------------|--------|
| load_repo            | 1.274s | 1.270s| **1.225s**   | symbol_table |
| build_provider       | 632ms  | **583ms** | 622ms | Lasso |

### 6. Regen Parallelism Scaling (hyperfine, example on 12-core)

| -j | Time     | Notes             |
|----|----------|-------------------|
| 4  | ~17.5s   | Underutilized     |
| 6  | **~13.7s** | Optimal        |
| 8  | ~15.5s   | Contention        |
| 12 | ~15.8s   | Higher contention |

(From results.md; always verify correctness with file count + content hash.)

### 7. Solver Profile Comparison (arm64 profile, with USE + keywords)

| Solver  | Packages | Time   |
|---------|----------|--------|
| PubGrub | 316      | 3.0ms  |
| Resolvo | 88       | 1.1ms  |
| Portage | 246      | 2.9s   |

(See PROFILES.md for overlap analysis.)

### 8. Search (wall-clock, negligible interner/alloc effect)

~25-40ms for common queries (gcc, firefox, rust) — I/O bound.

## Re-running for Fresh Blogpost Data

1. Ensure current code: `cargo check -p portage-bench --benches` (and per-crate).
2. Setup shallow repo as above.
3. Run `benchmarks/scripts/bench-sweep.sh --configs papaya-mimalloc` (or full).
4. `./benchmarks/scripts/bench-eval.sh -o blog-tables.md`
5. Augment with `cargo bench` for micro numbers.
6. For parity: `./benchmarks/bench-em-vs-emerge.sh target/release/em`

**To match exact historical**:
- Use same rustc version if possible.
- Note machine (cores, CPU).
- Capture `meta.env` from sweeps.

## Notes for Blogpost

- papaya chosen as default interner for overall profile + zero extra deps.
- mimalloc gives consistent 15-25% wins on hot paths.
- em significantly faster than both pkgcraft and classic portage for these tasks.
- All numbers should be re-run or clearly dated/attributed for the post.

See individual source .md files for full context, caveats, and raw data.

### 9. Repository loading (thalia, 2026-09-24)

The new `portage-bench --bench repo_load` benchmark exercises `Repository::repo_entries`
against the real 33,121-entry Gentoo tree, including cache parsing and freshness checks.

| Benchmark | Median |
|-----------|--------|
| `repository/load/repo_entries` | 0.766 s |

Reproduction:

```sh
GENTOO_REPO=/var/db/repos/gentoo cargo bench -p portage-bench --bench repo_load -- --noplot
```

This is a baseline for subsequent repository-loading changes; it is not directly
comparable with the older in-memory `resolve/load_repo` numbers.

### 10. Availability lookup (thalia, 2026-09-24)

The indexed `Avail::atom_satisfied` path is effectively constant-time across
synthetic set sizes:

| Entries | Median |
|---------|--------|
| 100 | 16.82 ns |
| 1,000 | 16.83 ns |
| 10,000 | 16.85 ns |

Reproduction:

```sh
cargo bench -p portage-bench --bench avail_lookup -- --noplot
```

This benchmark was added with the index; the pre-index linear baseline is not
claimed as a measured number.

### 11. Trim scaling (thalia, 2026-09-24)

`trim_scaling` builds synthetic plans with one dependency edge per package and
matching synthetic VDBs. The 128-entry fixture performs 8,128 BDEPEND consumer
checks and 16,256 DEPEND consumer checks.

| Trim | Pre-change median | Prefix-view only | Cache + snapshot | Consumer index |
|------|------------------:|-----------------:|-----------------:|---------------:|
| BDEPEND, 128 packages / 128 VDB entries | 197.42 ms | 3.09 ms | 0.57 ms | 0.54 ms |
| DEPEND, 128 packages / 128 VDB entries | 3.76 ms | 3.64 ms | 0.39 ms | 0.18 ms |

The isolated BDEPEND baseline ranged from 197–222 ms across runs; the table uses
the allocator-matched 197.42 ms sample. The pre-change values came from an isolated `HEAD` worktree running the same
fixture; the middle column uses only the prefix-availability view; the next
column adds the evaluated-dependency snapshot; the final column adds the
consumer index. Criterion uses 10 samples, a one-second warm-up, and a
three-second measurement window for this benchmark.
The steady-state allocation counts for the same 128-package fixture fell from
about 1,082,465 to 2,225 for BDEPEND and from 16,390 to 270 for DEPEND.

Reproduction:

```sh
cargo bench -p portage-bench --bench trim_scaling -- --noplot
```

The benchmark reports plan-pair and VDB sizes. Allocation attribution for
larger real plans still belongs in the `dhat-heap` run rather than being
inferred from wall time.

### 12. Graph/order scaling (thalia, 2026-09-24)

`graph_scaling` builds a cyclic RDEPEND chain with one solver-decided virtual
choice and measures `install_order_with_stats`. The PubGrub solver adapter and
CLI now consume this one graph/order result, so the reported build count is one
per pass.

| Packages | Selected nodes | Edges | Virtual expansions | SCCs / largest | Repair probes / nodes | Median |
|---------:|---------------:|------:|-------------------:|----------------:|----------------------:|-------:|
| 32 | 33 | 33 | 1 | 2 / 32 | 1 / 31 | 156.68 µs |
| 64 | 65 | 65 | 1 | 2 / 64 | 1 / 63 | 475.86 µs |
| 128 | 129 | 129 | 1 | 2 / 128 | 1 / 127 | 1.56 ms |

Reproduction:

```sh
cargo bench -p portage-bench --bench graph_scaling -- --noplot
```

The benchmark is intentionally a soft-cycle scaling fixture; it is not a
replacement for a real-repository resolve timing.

### 13. VDB snapshot sharing (thalia, 2026-09-24)

`vdb_snapshot` builds a synthetic BROOT/prefix VDB and compares one captured
raw snapshot with the independent-adapter path. `load_once` enumerates the roots
once and derives both views; `reuse` holds the snapshot across iterations;
`scan_each_view` lets the availability and host-installed adapters enumerate
independently. The fixture has 1.5 raw rows per requested package because every
other package also has a prefix row.

| Host entries | Raw rows | `scan_each_view` | `load_once` | `reuse` | `load_once` reduction |
|--------------:|---------:|-----------------:|------------:|--------:|----------------------:|
| 128 | 192 | 1.294 ms | 0.873 ms | 0.674 ms | 32.5% |
| 512 | 768 | 5.572 ms | 4.179 ms | 2.748 ms | 25.0% |
| 2,048 | 3,072 | 24.985 ms | 24.111 ms | 13.348 ms | 3.5% |

Criterion used 10 samples, a one-second warm-up, and a three-second measurement
window. The 2,048-entry result is close to run-to-run variation, so the smaller
fixtures are the directional evidence. This is an isolated enumeration/adapter
measurement, not an end-to-end resolve claim; the field cache still applies to
lazy VDB field reads.

Reproduction:

```sh
cargo bench -p portage-bench --bench vdb_snapshot -- --noplot
```

The owner-boundary regressions remove both VDB roots after capture and verify
that the host adapter retains host-then-prefix order while availability retains
both union rows. The target/sysroot extension is measured in section 30.

### 14. Policy-scoped version facts (thalia, 2026-09-24)

`policy_facts` feeds the real `repo::Adapter` a synthetic repository with 2 versions per CPN, 16 IUSE flags, a USE-conditional license, and a non-empty force/mask policy. `cached_warm` reuses one policy generation; `cached_reuse_4` performs four effective-USE reads per version to model the repeated post-solve consumers. The cold rows include cache allocation and first-use bookkeeping.

| CPNs | Uncached | Cached cold | Cached warm | Uncached ×4 | Cached ×4 |
|------:|---------:|------------:|------------:|------------:|----------:|
| 128 | 847.23 µs | 862.66 µs | 289.28 µs | 2.066 ms | 810.23 µs |
| 512 | 3.422 ms | 3.706 ms | 1.266 ms | 8.378 ms | 3.337 ms |
| 2,048 | 14.312 ms | 15.466 ms | 5.462 ms | 33.473 ms | 13.925 ms |

The warm path is 61.9–65.9% faster than the uncached control in this isolated fixture; the four-read path is 58.4–60.8% faster. Cold-cache overhead is visible and expected. The `generations` rows deliberately advance through four policy generations and are not a warm-cache claim. This measures policy-fact reuse, not end-to-end resolve time.

Reproduction (30 samples, one-second warm-up, two-second measurement window):

```sh
cargo bench -p portage-bench --bench policy_facts -- --noplot --warm-up-time 1 --measurement-time 2 --sample-size 30
```

### 15. CPN-indexed policy lists (thalia, 2026-09-24)

`policy_lists` runs the full acceptance filter over a synthetic target with matching entries at the end of keyword, license, property, restrict, mask, and unmask lists. The current fixture uses the production CPN indexes; the baseline is the same fixture at `ac54eb1c`, before BF-207, with raw list scans.

| Noise entries per list | Baseline full filter | Indexed full filter | Reduction |
|----------------------:|---------------------:|----------------------:|----------:|
| 128 | 1.948 µs | 0.812 µs | 58.3% |
| 512 | 5.675 µs | 0.816 µs | 85.6% |
| 2,048 | 20.550 µs | 0.815 µs | 96.0% |

The fixture has one candidate and measures policy-list lookup/index construction trade-offs, not provider construction or end-to-end resolve time. Criterion used 30 samples, a one-second warm-up, and a two-second measurement window.

Reproduction on the current tree:

```sh
cargo bench -p portage-bench --bench policy_lists -- --noplot --warm-up-time 1 --measurement-time 2 --sample-size 30
```

The baseline was run from a detached worktree at `ac54eb1c` with the same fixture and raw-list adapter.

### 16. Exact CPV cache lookup (thalia, 2026-09-25)

`repo_cache_lookup` builds a synthetic repository with 128/512/2,048 CPNs and 8/32/128 versions per CPN, then warms three exact-version lookups per CPN. The baseline is the same fixture at `d737929e` (after BF-207, before BF-208), using the former linear vector scans; the indexed side builds its CPV index once per fixture.

| Fixture | Baseline lookup | Indexed lookup | Change |
|---------|----------------:|---------------:|-------:|
| 128×8 | 29.1 µs | 54.8 µs | +88.3% |
| 128×32 | 78.8 µs | 55.4 µs | −29.7% |
| 128×128 | 302.5 µs | 56.0 µs | −81.5% |
| 512×8 | 122.7 µs | 221.2 µs | +80.2% |
| 512×32 | 380.5 µs | 226.1 µs | −40.6% |
| 512×128 | 1.506 ms | 227.7 µs | −84.9% |
| 2,048×8 | 704.9 µs | 1.168 ms | +65.7% |
| 2,048×32 | 1.926 ms | 1.231 ms | −36.1% |
| 2,048×128 | 11.242 ms | 1.344 ms | −88.0% |

The eight-version rows show the expected hash-index setup/lookup overhead; the index is not a universal win for tiny per-CPN vectors. At 32 versions it reduces lookup time by 29.7–40.6%, and at 128 versions by 81.5–88.0%. Index construction adds roughly 2.5–15% to the isolated build fixture. This is an exact-lookup measurement, not an end-to-end resolve claim; raw duplicate-repository scans remain unchanged.

Criterion used 30 samples, a one-second warm-up, and a two-second measurement window. Reproduction on the current tree:

```sh
cargo bench -p portage-bench --bench repo_cache_lookup -- --noplot --warm-up-time 1 --measurement-time 2 --sample-size 30
```

The baseline was run from a detached worktree at `d737929e` with the same fixture and raw-list lookup implementation.

### 17. Typed graph node indices (thalia, 2026-09-25)

`graph_scaling` uses the existing soft-cycle/OR fixture. BF-210 keeps one cached lexical key per selected node, maps graph edges and repair positions through typed `(PortagePackage, Version)` indices, deduplicates ordering adjacency, and reuses generation-stamped reachability marks. The baseline is the same fixture at `b122fc18`.

| Selected nodes | Baseline order | Indexed order | Reduction |
|---------------:|---------------:|--------------:|----------:|
| 33 | 154.5 µs | 148.8 µs | 3.7% |
| 65 | 467.2 µs | 457.5 µs | 2.1% |
| 129 | 1.548 ms | 1.522 ms | 1.7% |

The fixture reports one repair reachability probe and the same 33/65/129-edge graph at each size. This is an isolated graph-order measurement; it does not claim an end-to-end resolve speedup. Criterion used 10 samples, a one-second warm-up, and a two-second measurement window.

Reproduction on the current tree:

```sh
cargo bench -p portage-bench --bench graph_scaling -- --noplot --warm-up-time 1 --measurement-time 2 --sample-size 10
```

The baseline was run from a detached worktree at `b122fc18` with the same fixture.

### 18. Provider post-processing fast path (thalia, 2026-09-25)

`provider_postprocess` constructs synthetic providers with 128/512/2,048 packages. The all-known fixture exercises the new no-drop path; the missing-branch fixture includes complete OR siblings plus one absent target. The baseline is the same fixture at `b122fc18`.

| Packages | All-known baseline | All-known fast path | Missing baseline | Missing fast path |
|---------:|-------------------:|--------------------:|----------------:|-------------------:|
| 128 | 222.6 µs | 218.6 µs | 1.313 ms | 1.104 ms |
| 512 | 1.246 ms | 1.080 ms | 8.498 ms | 6.299 ms |
| 2,048 | 8.648 ms | 6.868 ms | 46.181 ms | 32.906 ms |

The complete-repository path is 1.8–20.6% faster; the missing-branch path is 15.9–28.7% faster in this fixture because short-circuit detection avoids the old unconditional pairwise/partition work. This is isolated provider construction, not an end-to-end resolve claim. Criterion used 10 samples, a one-second warm-up, and a two-second measurement window.

Reproduction on the current tree:

```sh
cargo bench -p portage-bench --bench provider_postprocess -- --noplot --warm-up-time 1 --measurement-time 2 --sample-size 10
```

The baseline was run from a detached worktree at `b122fc18` with the same fixture.

### 19. Worker-local shell reuse (thalia, 2026-09-25)

`source_reuse` creates 32/128/512 trivial ebuilds and drains `source_parallel` with one or four workers. BF-301 creates one shell per worker and relies on the existing baseline reset before each ebuild; the baseline is the same fixture at `de02498f`, where every ebuild constructed a fresh shell.

| Ebuilds × workers | Baseline | Worker-local shell | Reduction |
|------------------:|---------:|-------------------:|----------:|
| 32 × 1 | 11.914 ms | 3.189 ms | 73.2% |
| 32 × 4 | 13.160 ms | 4.665 ms | 64.6% |
| 128 × 1 | 52.277 ms | 13.580 ms | 74.0% |
| 128 × 4 | 52.261 ms | 14.478 ms | 72.3% |
| 512 × 1 | 209.53 ms | 53.096 ms | 74.7% |
| 512 × 4 | 209.55 ms | 54.124 ms | 74.2% |

The source-worker regression covers two ebuilds through the one-worker path and confirms the reused shell remains hermetic. This is an isolated synthetic sourcing measurement, not an end-to-end regen claim. Criterion used 10 samples, a one-second warm-up, and a two-second measurement window.

Reproduction on the current tree:

```sh
cargo bench -p portage-bench --bench source_reuse -- --noplot --warm-up-time 1 --measurement-time 2 --sample-size 10
```

The baseline was run from a detached worktree at `de02498f` with the same fixture.

### 20. Per-shell distdir resolution (thalia, 2026-09-25)

The same `source_reuse` fixture isolates BF-302 from BF-301 by comparing the BF-301 parent (`cd0b37d2`) with the distdir cache. The shell now probes and creates the writable distdir once, then reuses the resolved pair for later ebuilds.

| Ebuilds × workers | BF-301 parent | Cached distdir | Reduction |
|------------------:|--------------:|---------------:|----------:|
| 32 × 1 | 3.175 ms | 2.967 ms | 6.6% |
| 32 × 4 | 4.645 ms | 4.394 ms | 5.4% |
| 128 × 1 | 13.425 ms | 12.616 ms | 6.0% |
| 128 × 4 | 14.477 ms | 13.531 ms | 6.5% |
| 512 × 1 | 52.623 ms | 49.241 ms | 6.4% |
| 512 × 4 | 53.536 ms | 50.706 ms | 5.3% |

This is an isolated synthetic sourcing measurement. Criterion used 10 samples, a one-second warm-up, and a two-second measurement window.

Reproduction on the current tree:

```sh
cargo bench -p portage-bench --bench source_reuse -- --noplot --warm-up-time 1 --measurement-time 2 --sample-size 10
```

The baseline was run from a detached worktree at `cd0b37d2` with the same fixture.

### 21. Eclass digest/path memo (thalia, 2026-09-25)

`eclass_memo` creates a fresh repository for every iteration and live-sources 32/128/512 ebuilds that inherit one shared eclass. The baseline is the BF-302 parent at `d10ec9a4`; the current tree memoizes eclass digests by resolved path and seeds the name memo used by cache validation. Criterion used 10 samples, a one-second warm-up, and a two-second measurement window.

| Ebuilds | BF-302 parent | Eclass memo | Difference |
|--------:|--------------:|------------:|-----------:|
| 32 | 5.198 ms | 5.330 ms | +2.5% |
| 128 | 17.879 ms | 17.449 ms | -2.4% |
| 512 | 67.848 ms | 67.958 ms | +0.2% |

The result is neutral/noisy for this end-to-end sourcing fixture; the shell work dominates the saved digest reads. No speedup is claimed.

Reproduction on the current tree:

```sh
cargo bench -p portage-bench --bench eclass_memo -- --noplot --warm-up-time 1 --measurement-time 2 --sample-size 10
```

The baseline was run from a detached worktree at `d10ec9a4` with the same fresh-repository fixture.

### 22. Ebuild content reuse during regen (thalia, 2026-09-25)

`regen_reads` creates 32/128/512 trivial ebuilds and runs one-worker `regen_cache` against a staging output. Each iteration reuses the source tree but re-reads each ebuild, so this measures the complete synthetic regen path rather than raw syscall counts. The baseline is the exact `f898dd99` worktree with the same lockfile and fixture; the current tree parses the one content read and carries its digest into cache construction.

| Ebuilds | `f898dd99` baseline | Current | Reduction |
|--------:|---------------------:|--------:|----------:|
| 32 | 5.3384 ms | 4.9981 ms | 6.4% |
| 128 | 20.311 ms | 19.341 ms | 4.8% |
| 512 | 80.968 ms | 78.031 ms | 3.6% |

The synthetic median improvement is modest and narrows as the fixture grows; it is not a real-tree or end-to-end Portage claim. Criterion used 10 samples, a one-second warm-up, and a two-second measurement window.

Reproduction on the current tree:

```sh
cargo bench --locked -p portage-bench --bench regen_reads -- --noplot --warm-up-time 1 --measurement-time 2 --sample-size 10
```

The baseline was run from a detached worktree at `f898dd99` with the same fresh fixture and lockfile.

### 23. Metadata cache serialization (thalia, 2026-09-25)

`metadata_serialize` parses one representative full `CacheEntry` and repeatedly serializes it. The baseline is the exact `774ebef5` worktree with the same fixture and lockfile; the current tree writes fields directly into the final buffer instead of constructing per-field vectors and strings.

| Measurement | `774ebef5` baseline | Current | Reduction |
|-------------|---------------------:|--------:|----------:|
| Median time | 4.3334 µs | 2.1491 µs | 50.4% |

The result is a synthetic microbenchmark for the serializer, not a real-repository or full-regeneration claim. The exact serialized bytes are also covered by the cache serialization regression. Criterion used 10 samples, a one-second warm-up, and a two-second measurement window.

Reproduction on the current tree:

```sh
cargo bench --locked -p portage-bench --bench metadata_serialize -- --noplot --warm-up-time 1 --measurement-time 2 --sample-size 10
```

The baseline was run from a detached worktree at `774ebef5` with the same fixture and lockfile.

### 24. Prepared staging writer (thalia, 2026-09-25)

`regen_reads` exercises the complete synthetic regeneration path and isolates the staging-writer change against the exact `b9eb8b11` worktree. The current tree constructs one shared writer after `stage_dir_target` has prepared the category directories, then uses its no-`create_dir_all` write path for each entry.

| Ebuilds | `b9eb8b11` baseline | Current | Change |
|--------:|--------------------:|--------:|-------:|
| 32 | 4.9769 ms | 5.1832 ms | +4.1% |
| 128 | 20.447 ms | 19.500 ms | -4.6% |
| 512 | 78.097 ms | 77.193 ms | -1.2% |

The result is mixed on this single-worker synthetic fixture, so no end-to-end speedup is claimed. Criterion used 10 samples, a one-second warm-up, and a two-second measurement window; the baseline used the same fixture and lockfile in a detached worktree.

Reproduction on the current tree:

```sh
cargo bench --locked -p portage-bench --bench regen_reads -- --noplot --warm-up-time 1 --measurement-time 2 --sample-size 10
```

### 25. Async cache discovery (thalia, 2026-09-25)

`repository/load/repo_entries` runs the cache-discovery and read path against the real `/var/db/repos/gentoo` tree (33,121 metadata entries). The baseline is the exact `6b76d606` worktree with the same lockfile and benchmark command; the current tree moves jwalk discovery to `spawn_blocking` and moves descriptor chunks into the read workers.

| Measurement | `6b76d606` baseline | Current | Reduction |
|-------------|--------------------:|--------:|----------:|
| Median time | 787.35 ms | 674.80 ms | 14.3% |

This is a real-tree repository-load measurement, not a full `em -p` claim. Criterion used 10 samples, a one-second warm-up, and a requested two-second measurement window; it automatically expanded the collection window for this benchmark.

Reproduction on the current tree:

```sh
GENTOO_REPO=/var/db/repos/gentoo cargo bench --locked -p portage-bench --bench repo_load -- --noplot --warm-up-time 1 --measurement-time 2 --sample-size 10
```

The baseline used the same command and lockfile in a detached worktree at `6b76d606`.

### 26. Cache worker budget (thalia, 2026-09-25)

`cache_workers` reads and parses the real `/var/db/repos/gentoo` metadata cache (33,121 entries) at explicit worker counts and at the default. The baseline is the exact `3c873f62` worktree; the current tree caps automatic cache/source workers at 16 and reuses that budget for the overlapping primary and secondary cache reads in `repo_entries`.

| Worker setting | `3c873f62` baseline | Current | Change |
|----------------|--------------------:|--------:|-------:|
| Default | 160.29 ms | 197.58 ms | +23.3% |
| 1 | 515.16 ms | 481.14 ms | -6.6% |
| 4 | 273.71 ms | 218.86 ms | -20.0% |
| 16 | 196.26 ms | 207.20 ms | +5.6% |
| 32 | 206.80 ms | 187.99 ms | -9.1% |
| 64 | 202.40 ms | 180.49 ms | -10.8% |
| 128 | 170.62 ms | 172.88 ms | +1.3% |

The default row is the policy comparison; the explicit rows are diagnostic scaling measurements and are noisy across separate runs. The cap intentionally trades isolated cache-read throughput for bounded concurrency. The matched real-tree `repository/load/repo_entries` benchmark is 663.10 ms at the baseline and 672.48 ms currently (+1.4%), so no speedup is claimed.

Reproduction on the current tree:

```sh
GENTOO_REPO=/var/db/repos/gentoo cargo bench --locked -p portage-bench --bench cache_workers -- --noplot --warm-up-time 1 --measurement-time 2 --sample-size 10
```

The baseline used the same fixture, benchmark, and lockfile in a detached worktree at `3c873f62`.

---

### 27. Merge-batch VDB ownership index (thalia, 2026-09-25)

`portage-vdb/benches/ownership.rs` creates synthetic VDBs with 32 `obj` entries per package and performs one collision query per package. The baseline is the exact `0feec1ea` worktree using the pre-index `Vdb::find_collisions` implementation once per query. The current benchmark builds one ownership index, checks its VDB metadata stamp, and reuses it for the whole query batch.

| Packages | `0feec1ea` repeated scans | Current checked index batch | Reduction |
|---------:|--------------------------:|--------------------------:|----------:|
| 128 | 99.457 ms | 11.781 ms | 88.2% |
| 256 | 418.47 ms | 21.215 ms | 94.9% |
| 512 | 1.7788 s | 47.716 ms | 97.3% |

This is a synthetic VDB benchmark, not a full merge or real-system claim. The index retains path/CPV maps and `InstalledPackage` handles to avoid repeatedly allocating and parsing every `CONTENTS` file; each query also performs a small VDB metadata-stamp check. The CLI enables reuse only for plans with at least 16 merge entries; smaller plans and privilege-worker children retain the one-shot scan path because a full index build costs more than several streaming scans. No separate allocation profiler was run for this change. Criterion used 10 samples with its default warm-up and measurement windows.

On the real 727-package, 1,388,688-entry `/var/db/pkg` tree, the same benchmark's one-shot build measured a 3.343 s median and a 64-query reuse batch measured 26.8 µs. This is a build/reuse measurement, not a full merge claim; it motivated the CLI's 16-entry activation threshold. The real-tree benchmark is:

```sh
VDB_ROOT=/var/db/pkg cargo bench -p portage-vdb --bench ownership -- --noplot vdb_ownership_real
```

The benchmark's repeated-build RSS is allocator-retention noise, so no peak-memory claim is made here.

Reproduction on the current tree:

```sh
cargo bench -p portage-vdb --bench ownership -- --noplot index_batch
```

The baseline used the same fixture and command in a detached worktree at `0feec1ea`, with a temporary benchmark registration that exercised the pre-index API.

---

### 28. Preserve-libs state reuse (thalia, 2026-09-25)

The ignored `portage-cli` test `preserve_libs::tests::benchmark_reused_preserve_state` models a batch of synthetic VDB replacements. The exact `dc57ead3` baseline was measured in a detached worktree with the same fixture: it reloads the registry, rebuilds the link graph, reclaims, and stores once per replacement. The current path retains the registry and graph, updates package records incrementally, and performs one final reclaim/store. Each row is the median of three release-test runs on this host:

| Packages | `dc57ead3` repeated state operations | Current reused state | Reduction |
|---------:|------------------------------------:|-------------------:|----------:|
| 32 | 6.687708 ms | 524.407 µs | 92.2% |
| 64 | 14.918696 ms | 948.012 µs | 93.6% |
| 128 | 47.864809 ms | 3.027420 ms | 93.7% |

This is a synthetic state-workload measurement, not a full merge or real-system claim. The current implementation still scans the VDB once when its run-scoped graph is first needed and updates only the changed package thereafter; no separate allocation profile was run. The current fixture also prints its own repeated-operation control, but the table uses the exact historical worktree for the comparison.

Reproduction on the current tree:

```sh
cargo test --release -p portage-cli preserve_libs::tests::benchmark_reused_preserve_state --lib -- --ignored --nocapture
```

The historical row was reproduced from a temporary detached worktree at `dc57ead3`; it was removed after the run.

---

### 29. Run-scoped parsed VDB fields (thalia, 2026-09-25)

The ignored `portage-vdb` test `field_cache::tests::benchmark_parsed_metadata_reuse` builds synthetic packages and repeatedly reads EAPI, SLOT, USE, IUSE, and RDEPEND through independent live VDB handles. The exact `9bc6c785` baseline used the process-wide raw-string cache; the current tree shares parsed values through a weak-root run cache. Each row is the median of three release-test runs after a warm-up scan on this host:

| Packages | `9bc6c785` repeated parsing | Current parsed cache | Reduction |
|---------:|----------------------------:|--------------------:|----------:|
| 64 | 1.532920 ms | 477.587 µs | 68.8% |
| 128 | 3.080220 ms | 849.951 µs | 72.4% |
| 256 | 4.152914 ms | 1.234116 ms | 70.3% |

This is a synthetic parsed-field workload, not a full `em -p` or merge claim. The current cache retains raw and typed values only while live VDB/package handles exist; `register`/`unregister` clear one package without a global `retain()`. External writes remain invisible to a live cache, matching the prior push-invalidation policy. No separate allocation profile was run.

Reproduction on the current tree:

```sh
cargo test --release -p portage-vdb field_cache::tests::benchmark_parsed_metadata_reuse --lib -- --ignored --nocapture
```

The historical row was reproduced from a temporary detached worktree at `9bc6c785`; it was removed after the run.

---

### 30. Target/sysroot VDB snapshot sharing (thalia, 2026-09-25)

The `vdb_snapshot` benchmark's `resolver/vdb_target_sysroot_snapshot` group models a board-root resolve with a toolchain sysroot and target VDB. The current `load_once` path captures each distinct VDB root once, then derives target-installed, sysroot-installed, `DEPEND`, and base-DB views. The exact `450e3bf9` baseline independently enumerates the roots for each view. Both fixtures are filesystem-warmed before measurement; Criterion used 10 samples with a one-second warm-up and three-second measurement window.

| Packages per root | `450e3bf9` independent views | Current shared snapshots | Reduction |
|------------------:|---------------------------:|-----------------------:|----------:|
| 128 | 9.0683 ms | 5.3268 ms | 41.3% |
| 512 | 39.666 ms | 22.517 ms | 43.2% |
| 2,048 | 189.47 ms | 110.11 ms | 41.9% |

This is a synthetic target/sysroot enumeration-and-adapter workload, not a full resolve or merge claim. The snapshot is invocation-scoped and root-selection rules remain separate for target, sysroot, and base views. No separate allocation profile was run.

Reproduction on the current tree:

```sh
cargo bench -p portage-bench --bench vdb_snapshot -- --noplot --sample-size 10 --warm-up-time 1 --measurement-time 3
```

The historical row was reproduced from a temporary detached worktree at `450e3bf9`; it was removed after the run.

### 31. Binpkg reuse prerequisites (thalia, 2026-09-25)

The ignored `portage-cli` test `merge::entry_roots_tests::benchmark_binpkg_key_and_reuse_lookup` measures the two per-entry prerequisites involved in binpkg action selection. Its release median over five samples was 6.459 ms for 128 package-environment keys, 72.0 µs for one reuse lookup per entry, and 138.8 µs for a modeled duplicate lookup. The duplicate is a control for the old two-lookup shape, not an end-to-end historical benchmark.

The current merge path skips key calculation for VDB-present entries and source-only runs, and passes one local/remote reuse decision to both activity classification and the action path. No separate allocation profile was run; the decision retains at most one local path or one remote URL per active entry.

Reproduction on the current tree:

```sh
cargo test --release -p portage-cli benchmark_binpkg_key_and_reuse_lookup -- --ignored --nocapture
```

### 32. GPKG streaming verification/extraction (thalia, 2026-09-25)

The ignored `portage-binpkg` test `gpkg::tests::benchmark_extract_image_streams_large_member` builds an unsigned 64 MiB image GPKG and times only `extract_image`. Three release runs of the identical test at exact `c735f182` and on the current tree measured medians of 89.793 ms and 80.357 ms respectively. The timing difference is modest and synthetic; the main result is memory: a matched standalone run reported 65,536 KiB peak RSS at `c735f182` versus 4,032 KiB after streaming, and `strace` observed 12 fewer process executions because the outer image/Manifest extraction is no longer repeated.

Manifest generation and verification now hash members in fixed-size chunks, decompression streams to a temporary tar, and all members needed by one verification are staged with a single `tar` invocation. Detached OpenPGP verification still buffers its member because the `pgp` API requires a contiguous slice. No full-merge speedup is claimed, and no separate allocation profiler was run.

Reproduction on the current tree:

```sh
cargo test --release -p portage-binpkg benchmark_extract_image_streams_large_member -- --ignored --nocapture
```

---

### 33. Post-review measurement pass (thalia, 2026-09-26)

Machine: **thalia** (Ampere-1a, 128c, 4 NUMA nodes), all runs under
`numactl -N 0 -m 0` per `machines/thalia.md`. Criterion for the microbenchmarks,
`hyperfine` for wall-clock.

**Targeted re-measurement of the paths changed by the 2026-09-26 review fixes.**

| bench | median | criterion verdict |
|---|---|---|
| `dep_parsing/DepEntry::parse/dep/simple` | 447.6 ns | -0.2%, within noise |
| `dep_parsing/DepEntry::parse/dep/medium` | 2.274 us | -1.5%, improved |
| `dep_parsing/DepEntry::parse/dep/complex` | 5.273 us | -0.7%, within noise |
| `dep_parsing/RequiredUseExpr::parse/required_use` | 1.186 us | +0.5%, within noise |
| `repo_load/repository/load/repo_entries` | 675.6 ms | no change (p = 0.64) |

The parse numbers matter because the `u64` overflow fix replaced `parse_cpv`'s
`verify_map` (which swallowed every error through `.ok()?`) with explicit
sequencing and `parse_whole`. That is a rewrite of the hot atom path, and it
costs nothing measurable.

`repo_entries` says nothing either way about the gap-index change. The gap index
does exist on this host -- `~/.cache/em/md5-cache/gentoo/gap-index` -- and is
opened on every resolve (`strace -e openat` shows one open per run), but it holds
only its sync-stamp line and **no CPVs**, so the per-CPV loop executes zero times.

A synthetic bench (`benches/gap_index.rs`) was then built to populate the gap
list and give the change a fair test. **It does not show a speedup.**

| case | indexed (HEAD) | linear (`17f99cd8^`) | linear vs indexed |
|---|---|---|---|
| `primary_2000_gap_0` | 11.617 ms | - | - |
| `primary_2000_gap_200` | 11.518 ms | - | - |
| `primary_2000_gap_2000` | 11.661 ms | - | - |
| `primary_8000_gap_8000` | 50.74 ms | 43.34 ms | **-14.6%** |

Two findings, both inconvenient for the change:

1. At 2,000 primary entries the total is **flat in gap size** (11.617 / 11.518 /
   11.661 ms for 0 / 200 / 2,000 gap lines). The whole gap loop is inside the
   noise there, so the O(gap x primary) term this change removes is not a cost
   worth removing at that size.
2. At 8,000 x 8,000 the *linear* build is reproducibly 14.6% **faster** across
   three interleaved rounds. A 32M-comparison scan cannot make a scan faster
   than an index, so this is not the lookup -- it is almost certainly code layout
   between two builds differing in one function. Reproducible, not signal.

**The index turned out to be a regression, and the `gap_0` control is what proved
it.** With an empty gap list the changed code never runs, yet the indexed build
was still 11.3% slower there -- so the cost is not the lookup but the
`HashMap<Cpv, usize>` built unconditionally over every primary entry, which is
dearer to construct than the `HashSet` it replaced. A `Cpv` comparison fails on an
interned pointer almost immediately, so 32M comparisons of the old scan cost less
than one 8,000-entry hash map.

The index was reverted; the accompanying ordering fix was kept. Re-measured
against the indexed build over 5 interleaved rounds, the revert is 14.6-18.7%
faster at every gap size, and `gap_0` is the *fastest* case precisely because
there is no lookup left to do. Full data, both binaries' checksums and the
methodology are in
[`results/20260926-gap-index-6154f5f2-vs-c3c29af3/`](results/20260926-gap-index-6154f5f2-vs-c3c29af3/README.md).

The lesson generalises past this change: **a data-structure swap inside a fast
path needs a case where the new structure's construction is free**, or the
measurement just measures the construction. The `gap_0` case is that control, and
it was the only reason this was caught rather than shipped as a win.

A harness note worth keeping: `cargo build -p portage-bench --benches` defaults to
the **dev** profile. Copying `target/release/deps/<bench>` after such a build
silently re-copies the previous `cargo bench` artifact, so an A/B compares one
binary against itself. `cargo bench` must be the producer, or the build must say
`--release` explicitly -- and the two artifacts should be md5-compared before
trusting any result.

### Overlay cost of a repo with no sync marker

The same bench measures what it costs to add an overlay *without* a
`timestamp.chk`, since such a tree can never use the memo and re-walks on every
resolve:

| ebuilds | marked (memo used) | unmarked (walks every resolve) | penalty |
|---|---|---|---|
| 500 | 3.046 ms | 7.395 ms | 2.4x (+4.3 ms) |
| 2,000 | 11.718 ms | 28.227 ms | 2.4x (+16.5 ms) |

That is ~8.6 us per ebuild per resolve, linear in tree size -- so a 500-ebuild
hand-made overlay costs ~4 ms per resolve, a 4,000-ebuild one ~34 ms, and a
30,000-ebuild one ~260 ms. `guru` (3,764 ebuilds) has a sync marker and pays
none of it. The marked/unmarked split is also the proof that the marked fixture
genuinely consults the memo, which the real-tree bench could not show.

**A session-level wall-clock A/B is not available, and the reason is structural.**
Timing `em -p --emptytree @system` and `-vp --emptytree dev-libs/openssl` at
`d737929e` (the commit before the session) against `b1bcf570` gives *different
plans*, not just different times: same ebuild count (351) but a different set --
`llvm-runtimes/clang-*-config-23` multi-slot entries in place of
`sys-devel/binutils-config-5.6` and others. The session contained nine `fix:`
commits, and `b122fc18` ("retain selected virtual branches") changes which
packages the graph keeps. Any timing delta between the endpoints therefore mixes
"faster" with "different work", and cannot be attributed.

**The resolve bench shows no signal above this machine's noise floor.** Repeated
runs of the identical binary disagree in sign: `targets/gcc` read -0.2% on one
run and +0.7% on the next, `targets/firefox` spanned +0.09% to +12.9% with
p = 0.23. Current medians for the record -- `load/load_repo` 1.002 s,
`load/build_provider` 529.9 ms, and per-target 4.22 ms (openssh), 4.51 ms (gcc),
5.79 ms (python), 9.32 ms (rust), 26.33 ms (firefox). Criterion's single-run
comparison against its own previous save is not a usable instrument for sub-1%
effects here; the same conclusion as the BF-512 `Adjacency` measurement, where 12
interleaved runs put the spread (88-137 ms) above the effect.

**The aggregate question stays open, and this is the experiment that would settle
it:** replay only the 19 `perf:` commits onto `d737929e`, excluding the nine
`fix:` commits, then A/B that against `d737929e`. Both ends would compute the same
plan, so the delta is attributable. It needs two `--release` builds of a 19-commit
series and is not cheap, which is why it is proposed rather than run.

Reproduction on the current tree:

```sh
numactl -N 0 -m 0 cargo bench -p portage-bench --bench dep_parsing
numactl -N 0 -m 0 cargo bench -p portage-bench --bench repo_load
numactl -N 0 -m 0 cargo bench -p portage-bench --bench resolve
```

---

*Generated from scattered sources in the repo. Run the scripts on current HEAD to refresh.*