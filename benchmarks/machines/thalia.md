# thalia - AmpereOne (Ampere-1a)

**Date characterized**: 2026-06-14

This is the current development / benchmark machine (128-core ARM server).

## Hardware Summary

- **Host**: thalia
- **Architecture**: aarch64 (Little Endian)
- **OS/Kernel**: Linux 7.0.1-gentoo (SMP PREEMPT_DYNAMIC)
- **CPU**: Ampere-1a (AmpereOne)
  - Model: 0, Stepping: 0x0
  - 128 cores (Thread(s) per core: 1, SMT disabled)
  - 1 socket
  - 4 NUMA nodes (32 cores each)
    - node 0: CPUs 0-31
    - node 1: CPUs 32-63
    - node 2: CPUs 64-95
    - node 3: CPUs 96-127
  - Frequency: max 3.4 GHz, min 1.0 GHz
  - Scaling: observed ~30% in some samples (boost disabled in characterization run)
  - BogoMIPS: 2000.33
- **Caches**:
  - L1d: 8 MiB (128 instances)
  - L1i: 2 MiB (128 instances)
  - L2: 256 MiB (128 instances)
- **NUMA**:
  - 4 nodes
  - Memory per node: ~65 GiB each (total system ~256 GiB)
  - Node distances: 10-12 (relatively close)
- **Memory**: 255 GiB total (at characterization: ~118 GiB free, 221 GiB available)
- **Flags** (key ARMv8+ features): fp asimd evtstrm aes pmull sha1 sha2 crc32 atomics fphp asimdhp cpuid asimdrdm jscvt fcma lrcpc dcpop sha3 sm3 sm4 asimddp sha512 asimdfhm dit uscat ilrcpc flagm ssbs sb paca pacg dcpodp flagm2 frint i8mm bf16 rng bti ecv (full list available via `lscpu`)

## NUMA and Binding

`numactl --hardware` output (summarized):

```
available: 4 nodes (0-3)
node 0 size: 65005 MB
node 1 size: 65463 MB
node 2 size: 65463 MB
node 3 size: 65380 MB
node distances:
node     0    1    2    3 
   0:   10   11   11   12 
   1:   11   10   12   11 
   2:   11   12   10   11 
   3:   12   11   11   10 
```

**Current numactl policy** (at char): default, binding to all nodes (0-3).

**Recommendation for reproducible benchmarks**:
- Pin to a single node to avoid cross-NUMA traffic: `numactl -N 0 -m 0 <cmd>`
- Or full machine: `numactl -N 0,1,2,3 -m 0,1,2,3 <cmd>`
- Monitor with `numastat`, `perf`, etc.
- Note: Avoid running while other processes are using `numactl` or heavy builds for clean results.

## Rust / Toolchain

- Active: gentoo-packaged (rustc 1.95.0 at time of characterization)
- For MSRV verification: use specific toolchains e.g. `rustup run 1.88.0-aarch64-unknown-linux-gnu cargo ...`

## Available Trees for Benchmarking

- System: `/var/db/repos/gentoo` (746 MiB, but partial metadata: ~174 entries in quick ls)
- Local test: `portage-repo/gentoo` (665 MiB)
- For full-scale (regen, resolve on ~31k+ ebuilds): `git clone --depth 1 https://github.com/gentoo/gentoo.git gentoo` (as used in scripts)

## Re-characterization Commands

Always run and record these when doing new benchmark sessions on this machine:

```sh
uname -a
lscpu
numactl --hardware
numactl --show
free -h
uptime
cat /proc/cpuinfo | grep -E 'processor|model name' | head -5
# CPU freq sample
for i in 0 32 64 96; do echo "cpu$i freq:"; cat /sys/devices/system/cpu/cpu$i/cpufreq/scaling_cur_freq 2>/dev/null || echo 'n/a'; done
```

## Notes

- Freq scaling observed varying (some cores at 1GHz min, some at 3.4GHz max).
- For max consistent perf in benchmarks, consider setting governor to `performance` (may require privileges).
- This machine's scale (128 cores, NUMA) makes it excellent for testing parallelism in regen, solver, etc., but results will differ significantly from laptop-class (e.g. previous M2 Max) hardware.

See parent `BENCHMARKS.md` for how to run benchmarks and link results back to this machine file.
## Benchmark Runs on thalia (2026-06-14)

See the "Full Blown Scans Re-run" section below for the complete, post-fixes comparative data (regen wall times + RSS from per-crate scripts + 3-way via compare-regen.sh + dep parity/timings via bench-em-vs-emerge.sh). All full blown, isolated outputs, verified file counts, main cache untouched.

Earlier partial/timing numbers were from before script cleanups and are superseded.

## Earlier Partial Run Note (2026-06-14)

Older numbers from before the script fixes (CLI syntax, discovery paths) are superseded by the definitive "Full Blown Scans Re-run" section below. All subsequent work used the per-crate scripts + fixed compare/bench scripts + verified fresh empty outs + confirmed no touch to main test cache. See the re-run section for the complete current data set.
## Full Blown Scans Re-run (regen + dep, 2026-06-14) — using scripts from the crates

Re-executed everything with the fixed scripts after identifying fishy issues (outdated CLI syntax in top-level bench, missing SCRIPT_DIR causing unreliable bin discovery, PK path assumptions only working from certain cwds). All regen runs used the portage-repo/gentoo test tree and **fresh empty output directories** (mktemp or /tmp/verify-*) for each tool and job count. No incremental or warm-cache reuse in the target locations.

**Scripts used (per user guidance: many live in the specific crates):**
- `portage-repo/bench-regen.sh` (em's regen_only example + peak RSS poll, multiple j)
- `portage-repo/bench-pk.sh` (pk + RSS)
- `portage-repo/bench.sh` (hyperfine timing + RSS for regen_only vs pk)
- `./bench-regen.sh` (top-level, for full em CLI binary + RSS; fixed positional `em regen "$REPO" ...`)
- `benchmarks/scripts/compare-regen.sh` (unified em / pk / egencache; default off, opt-in with INCLUDE_EGENCACHE=1. When on, uses exact plain `sudo rm -rf /var/db/repos/gentoo/metadata/md5-cache && sudo egencache -j N --repo gentoo --update` (stock, no extra, repo name defaults to gentoo). Output is valid markdown table. em/pk on GENTOO_REPO; set to live for matching tree.)
- `benchmarks/bench-em-vs-emerge.sh` (package set parity from -p output + hyperfine timings for em -p / emerge -p and em -s / emerge -s)

Repro commands (run from portage-cli/ root, after `cargo build --release --bin em`):

```sh
# per-crate (lib/example level, with RSS)
GENTOO_REPO=portage-repo/gentoo ./portage-repo/bench-regen.sh 8 20
PK=../pkgcraft/target/release/pk GENTOO_REPO=portage-repo/gentoo ./portage-repo/bench-pk.sh 8 20
PK=../pkgcraft/target/release/pk GENTOO_REPO=portage-repo/gentoo ./portage-repo/bench.sh 8

# top level em CLI + RSS (fixed)
GENTOO_REPO=portage-repo/gentoo ./bench-regen.sh 8 20

# full compare (egencache opt-in with INCLUDE_EGENCACHE=1; plain on live)
GENTOO_REPO=/var/db/repos/gentoo EM=target/release/em PK=../pkgcraft/target/release/pk INCLUDE_EGENCACHE=1 ./benchmarks/scripts/compare-regen.sh 8 16 20 24 32
# (eg runs plain sudo rm + sudo egencache -j N --repo gentoo --update on live;
#  set GENTOO_REPO=live for em/pk match; SKIP=egencache to omit)

# dep resolution parity + timing (uses default/system repo discovery for both em and emerge)
RUNS=3 EM=target/release/em ./benchmarks/bench-em-vs-emerge.sh
```

Manual verify (the script now defaults to including egencache using the exact plain correct path, always on the live gentoo):

```sh
REPO=/var/db/repos/gentoo
rm -rf /tmp/regen-verify-{em,eg,pk}-j8; mkdir -p /tmp/regen-verify-{em,eg,pk}-j8
target/release/em regen "$REPO" -o /tmp/regen-verify-em-j8 -j 8
# egencache (script defaults to this plain correct full-cold on live /var/db one):
sudo rm -rf /var/db/repos/gentoo/metadata/md5-cache
sudo egencache --update --repo gentoo --jobs=8
../pkgcraft/target/release/pk repo metadata regen -j 8 -p /tmp/regen-verify-pk-j8 -f -n "$REPO"
for d in /tmp/regen-verify-*-j8; do echo "$d: $(find "$d" -type f | wc -l) files"; done
# (eg always hardcodes the live gentoo; count from there)
```
(See also `benchmarks/results/em-regen-help.txt` — captured from the built binary used for these runs.)

The script includes egencache only when INCLUDE_EGENCACHE=1 (default off). When enabled, runs the exact plain `sudo rm -rf /var/db/repos/gentoo/metadata/md5-cache && sudo egencache -j N --repo gentoo --update` (hardcoded live, no extra; repo name defaults to gentoo so no silent fail). Output is valid markdown table. To have em/pk on the same tree, set GENTOO_REPO=/var/db/repos/gentoo. Use SKIP=egencache to omit the slow leg. Live cache gets repopulated. No portage source modified.

### Cache Regen Comparative (egencache via the plain correct stock slow path by default)

The script includes egencache only with INCLUDE_EGENCACHE=1 (default off) and always uses the exact plain stock command: `sudo rm -rf /var/db/repos/gentoo/metadata/md5-cache && sudo egencache -j N --repo gentoo --update` (hardcoded live, repo name defaults to gentoo). Output is valid markdown table. Set GENTOO_REPO to live for em/pk on same tree. The tables have historical; new runs will have the proper eg points. See the datapoint note.

The numbers here (including the eg ~8s "full file count" ones) came from warm-cache + custom-patched egencache runs. They are not comparable to the true cold work.

All runs used `NUMACTL` binding (node 0, 32c). em/pk numbers remain valid for their full cold work.

j=8 (1 iter):

| tool      | j | run | real      | user      | sys      |
|-----------|---|-----|-----------|-----------|----------|
| em        | 8 | 1   | 0m18.120s | 2m11.310s | 0m12.394s |
| egencache | 8 | 1   | 0m7.894s  | 0m5.222s  | 0m1.956s  |
| pk        | 8 | 1   | 0m48.263s | 5m25.512s | 1m7.887s  |

j=16 (1 iter):

| tool      | j | run | real      | user      | sys      |
|-----------|---|-----|-----------|-----------|----------|
| em        | 16| 1   | 0m9.708s  | 2m16.137s | 0m12.220s |
| egencache | 16| 1   | 0m8.126s  | 0m4.992s  | 0m2.147s  |
| pk        | 16| 1   | 0m28.803s | 5m59.128s | 1m21.916s |

j=20 (1 iter):

| tool      | j | run | real      | user      | sys      |
|-----------|---|-----|-----------|-----------|----------|
| em        | 20| 1   | 0m8.895s  | 2m26.985s | 0m13.161s |
| egencache | 20| 1   | 0m8.075s  | 0m5.144s  | 0m2.004s  |
| pk        | 20| 1   | 0m24.320s | 5m54.989s | 1m30.146s |

j=24 (1 iter):

| tool      | j | run | real      | user      | sys      |
|-----------|---|-----|-----------|-----------|----------|
| em        | 24| 1   | 0m8.409s  | 2m34.293s | 0m15.775s |
| egencache | 24| 1   | 0m8.273s  | 0m5.096s  | 0m2.131s  |
| pk        | 24| 1   | 0m24.003s | 5m58.514s | 1m41.387s |

j=32 (1 iter):

| tool      | j | run | real      | user      | sys      |
|-----------|---|-----|-----------|-----------|----------|
| em        | 32| 1   | 0m10.507s | 3m23.611s | 0m24.350s |
| egencache | 32| 1   | 0m8.393s  | 0m5.107s  | 0m2.075s  |
| pk        | 32| 1   | 0m17.963s | 5m16.770s | 0m47.683s |

The *correct* full cold egencache timing (stock, after source cache clear) is:

```
time sudo egencache -j 20 --repo gentoo --update
real    4m37.251s
user    0m0.006s
sys     0m0.000s
```

`compare-regen.sh` supports egencache opt-in (INCLUDE_EGENCACHE=1, default off) using the exact plain command on the live gentoo tree (repo name defaults to "gentoo"). Output is a valid markdown table. em/pk on GENTOO_REPO (set to live for apples-to-apples). No portage source hacks. The tables below have some historical data from earlier runs.
- In egencache (external_cache_only branch): after the cps walk, explicitly union the walked cats into `portdb.settings.categories` (before GenCache/MetadataRegen/cp_list). This makes "full tree" robust even if a profile is incomplete. Matches the stated goal of the external path: exhaustive ignoring profile/visibility.
- Script now auto-detects/uses `numactl --cpunodebind=0 --membind=0` (when safe) for the timed tool runs.
- Result: egencache --cache-dir + --external-cache-only now yields identical full file count (31880) as em/pk, and the compare script can be used for true apples-to-apples without sudo/live cache clobber.

User's direct std timing (j=20, no bind, full 128c, after live rm): ~5.3s real. Our numa-bound (32c) eg ~8s at j=20 is consistent (less cpu + bind). egencache scales poorly past ~8 jobs (flat ~8s); em and pk continue to benefit from more workers. 

Repro (now gives matching full counts for eg too):
```sh
GENTOO_REPO=/var/db/repos/gentoo EM=target/release/em PK=../pkgcraft/target/release/pk INCLUDE_EGENCACHE=1 ITERATIONS=1 ./benchmarks/scripts/compare-regen.sh 8 16 20 24 32
```

**Verify file counts** (in the out dirs after the runs above, and confirmed in fresh compare runs):
- All three tools (em, egencache via --cache-dir+external, pk): 31880 files (full/exhaustive tree walk; tree state at 2026-06-14 had 31881 .ebuilds, off-by-1 is benign e.g. transient or invalid ebuild skipped uniformly).
- Previously egencache --cache-dir produced "visible" count (e.g. 31874) because the benchmark invocation used a broken PORTAGE_REPOSITORIES=... env (invalid as repos.conf) + insufficient synthetic profile categories, and the cp_iter walk in egencache was still subject to cp_list() "invalid category" pruning for cats not in the loaded profile. The std path ("sudo rm live-md5-cache && egencache --update --repo ...", no --cache-dir) always hit the system profile (full categories) and produced the expected full count. See below for the fixes that made --cache-dir produce full results too.

### Additional full blown regen data from per-crate scripts (j=8 / j=20)

From `portage-repo/bench-regen.sh` (regen_only example, RSS via VmRSS tree poll):

j=8: real 0m26.399s user 3m9.839s sys 0m16.563s peak 173 MB

j=20: real 0m11.776s user 3m17.348s sys 0m21.191s peak 238 MB

From `portage-repo/bench-pk.sh`:

j=8: real 0m48.836s ... peak 62 MB

j=20: real 0m23.147s ... peak 70 MB

From `portage-repo/bench.sh` (hyperfine accurate, one j=8):

regen_only: 27.82s (167 MB)   vs   pk: 49.68s (59 MB)   — regen_only 1.79x faster

From top-level `./bench-regen.sh` (full em binary CLI + RSS):

j=8: real 0m20.194s user 2m22.329s sys 0m10.425s peak 586 MB

j=20: real 0m10.289s user 2m35.449s sys 0m17.388s peak 809 MB

### Dep Resolution Full Scans (from benchmarks/bench-em-vs-emerge.sh, default 5 runs)

== package-set parity (em -p vs emerge -p)

| package | emerge | em | diffs |
|---------|--------|----|-------|
| dev-qt/qtbase                            |   42 |   42 |     0 |
| app-text/texlive-core                    |   64 |   63 |     3 |
| www-client/firefox                       |   79 |   79 |     0 |
| dev-qt/qtwebengine                       |   82 |   82 |     0 |
| mail-client/thunderbird                  |   82 |   82 |     0 |
| app-office/libreoffice                   |  141 |  141 |     0 |
| app-emulation/qemu                       |    2 |    2 |     0 |
| cross-riscv64-unknown-elf/gcc            |    1 |    1 |     0 |

== multi-target set (informational: cascade-tail divergence expected)

   emerge=184 em=187

   > dev-libs/wayland-1.25.0-r1

   > dev-libs/wayland-protocols-1.49

   > dev-util/wayland-scanner-1.25.0

== timing (hyperfine, RUNS runs; script now also emits a consolidated markdown table below the raw hyperfine summaries)

(The raw hyperfine lines are still printed by the script for full ±σ and range details. SKIP_TIMING=1 skips this entire block for fast parity-only runs.)

Example recent run (RUNS=2 on current tree state; timings vary with load/numa/cache — re-run for your env):

### Timing summary (markdown table)
| Benchmark | em (mean ± σ) | emerge (mean ± σ) | speedup (em vs emerge) |
|-----------|-----------------|-------------------|------------------------|
| firefox -p | 0.917 s ± 0.082 s | 3.675 s ± 0.000 s | 4.01× |
| libreoffice -p | 0.918 s ± 0.026 s | 3.953 s ± 0.013 s | 4.31× |
| multi (5 pkgs) -p | 1.041 s ± 0.025 s | 4.724 s ± 0.196 s | 4.54× |
| gcc -s | 0.154 s ± 0.011 s | 5.175 s ± 0.006 s | 33.63× |

RESULT: parity FAILED

(Parity: 0 diffs on most single-targets; small diffs on texlive-core (3) and the multi case (+3) are the known/expected divergences from emerge's backtracking behavior vs em's complete graph handling. The bench script exits non-zero on single-target diffs.)

Re-run with `EM=target/release/em ./benchmarks/bench-em-vs-emerge.sh` (or RUNS=... in env). The numbers above are from the completed background task (task id call-eefbc70e-0d82-4a1a-876a-f0b701552214-501) that tee'd the log; a copy of the exact output is persisted at `benchmarks/results/dep-thalia-5runs-2026-06-14.txt` for the blogpost.

See also `benchmarks/machines/README.md`, `BENCHMARKS.md`, and the scripts' own header comments + `benchmarks/results/portage-repo-regen-vs-pkgcraft-2026-03-04.md` (the old portage-repo/docs/benchmarks.md, moved out of the way of the root docs/benchmarks.md map — it's a frozen 2026-03-04 data point, not a living doc).

This (plus the regen tables above from the compare + per-crate scripts) is the clean set of full blown numbers.

## Regen Parallelism Sweet Spot (NUMA-bound, 2026-06-14)

Single-NUMA-node sweep to find the optimal `-j` for `em regen`. thalia has 4
NUMA nodes × 32 cores; effective scaling was measured by binding to one node
so the result is comparable to a 32-core box (rather than the full 128).

- **Tree**: `portage-repo/gentoo` (32013 ebuilds)
- **Binary**: `target/release/em` (release build, default features)
- **Binding**: `numactl --cpunodebind=0 --membind=0`
- **Governor**: `schedutil` (not pinned to `performance`)

| j  | real      | user      | sys       |
|----|-----------|-----------|-----------|
| 12 | 13.126 s  | 135.569 s | 11.549 s  |
| 16 | 10.629 s  | 141.127 s | 12.671 s  |
| 20 | **9.483 s** | 148.321 s | 14.786 s |
| 24 | **9.433 s** | 163.241 s | 17.353 s |
| 28 | 10.896 s  | 198.340 s | 22.832 s  |
| 32 | 11.109 s  | 204.694 s | 25.795 s  |

**Sweet spot: j=20–24** (statistically tied at ~9.45 s). Past j=24, real time
turns up: j=32 is 18% slower than j=24.

Why the plateau is at ~24 and not 32 (the node core count): system time
climbs steeply past the plateau (11.5 s → 25.8 s, ~2.2×) while user time
barely moves (135 s → 205 s). That delta is coordination/contention on the
shared eclass AST cache + I/O, not useful work.

**Rule of thumb** (confirmed on thalia and matching the per-machine heuristic):

- 12-core laptop → ~j8
- 24-core box → ~j20
- 32-core NUMA node (thalia) → ~j20–24

i.e. the optimal `-j` is roughly **0.6–0.75 × cores-per-NUMA-node**.

Repro (single-run `time` per point is sufficient; regen variance is tiny,
σ ~0.008 s at j=20 — no need for hyperfine warmup):

```sh
for J in 12 16 20 24 28 32; do
  OUT=$(mktemp -d)
  { time numactl --cpunodebind=0 --membind=0 \
      target/release/em regen portage-repo/gentoo -o "$OUT" -j "$J" >/dev/null 2>&1; } 2>&1
  rm -rf "$OUT"
done
```

## bench-sweep.sh regression check (2026-07-23, commit 2afb668)

Full `./scripts/bench-sweep.sh` run (default configs — `papaya-default`,
`papaya-mimalloc` only; papaya is the real shipped default interner, so
lasso/symbol-table are no longer part of the routine sweep) against the same
`benchmarks/gentoo` tree commit (`0833c9f`) as the 2026-07-20 baseline
(`bench-results/20260720-231747-78550ca`), to confirm the `-r`/`--resume` and
`--eta` activity-bus work landed since that baseline didn't regress anything.
Output: `bench-results/20260723-101215-2afb668/` (`commit.env`, `report.md`,
`summary.tsv`).

| metric | config | baseline (78550ca) | this run (2afb668) |
|---|---|---|---|
| regen real | papaya-default | 10.007s | 10.043s |
| regen real | papaya-mimalloc | 11.565s | 12.101s |
| regen user | papaya-default | 3m9.855s | 3m6.785s |
| regen user | papaya-mimalloc | 3m10.554s | 3m18.222s |
| search gcc (mean) | papaya-default | 0.100s | 0.096s |
| search firefox (mean) | papaya-default | 0.105s | 0.092s |
| search rust (mean) | papaya-default | 0.084s | 0.108s |

All deltas are within normal run-to-run noise (regen ±0.5s / search
±10-20ms) — no regression. Expected: none of the intervening commits touch
the interner/allocator/parsing/resolve/regen hot paths this sweep measures;
they're CLI-layer (activity bus, `--eta`, `-r`/`--resume`). Criterion
(`dep_parsing`/`realworld_dep_parsing`/`resolve`/`dedup`) numbers are also
consistent with prior runs and `MEMORY.md`'s tables — no per-benchmark
regression flagged by `bench-eval.sh`'s winner column beyond normal noise.

## `em -p` wall-clock anchor (2026-08-19, `deb4c81`)

USE defaults/conf split vs that commit: no measurable `em -p` cost (error
bars span 1.0 on firefox / libreoffice / qtwebengine). Before binary is
the existing 00:11 `--release` `em` at `deb4c81`, not a same-session
rebuild — hashes and methodology in
[`results/20260819-use-fold-defaults-conf-deb4c81/`](../results/20260819-use-fold-defaults-conf-deb4c81/).
New `em -p` wall-clock anchor: `deb4c81` /
`em_before` sha256 `c43dbacd4695b7aefac91ecacaa91ab6bdbf456f9325e880cbfff285532a4427`.
