# CLI wall-clock on a single host repo: `www-client/firefox`, 2026-09-26

- **historical:** 2026-07-25 sweep — `em` on `56a16bc` + dirty, same machine
  ([`20260725-220726-dirty-56a16bc-official/`](../../20260725-220726-dirty-56a16bc-official/README.md))
- **current:** `c23215b4`
- **machine:** thalia (AmpereOne, 128 cores, 4 NUMA nodes) —
  [`machines/thalia.md`](../../machines/thalia.md); all runs `numactl -N 0 -m 0`
- **repo:** `/var/db/repos/gentoo`, host repos, single repo — *not* the
  `benchmarks/gentoo` pin, so this is comparable to the 2026-07-25 `-p` figures
  which also used host repos.
- **why:** the criterion benches cannot answer "is the tool faster"; only CLI
  wall-clock on a real plan can.

## Workload is comparable

| | 2026-07-25 | now |
|---|---|---|
| `em -p` package count | 75 | 73 |
| `emerge -p` package count | 82 | 82 |

Two packages fewer on `em` (the tree moved since July); `emerge` unchanged. Close
enough to compare, but not identical, so treat single-digit-percent deltas as
noise on top of an already-approximate workload.

## Current figures

`benchmarks/bench-resolve-modes.sh` (RUNS=5, WARMUP=1), single host repo:

| mode | em | emerge | speedup |
|---|---|---|---|
| `-p` | 1.006 s ± 0.030 | 2.990 s ± 0.013 | 2.97× |
| `-up` | 1.016 s ± 0.062 | 2.972 s ± 0.028 | 2.93× |
| `-uNp` | 1.053 s ± 0.026 | 3.023 s ± 0.043 | 2.87× |
| `-uDp` | 1.126 s ± 0.038 | 5.474 s ± 0.140 | 4.86× |
| `-uNDp` | 1.165 s ± 0.030 | 5.506 s ± 0.193 | 4.73× |

Verbose modes, one interleaved `hyperfine` invocation, 10 runs (`-v` adds
download-size computation, USE reporting and per-package narration):

| command | mean | σ | min | user | sys |
|---|---|---|---|---|---|
| `em -vup` | 0.834 s | 0.022 | 0.808 | 1.00 | 0.31 |
| `em -vp` | 0.837 s | 0.016 | 0.815 | 1.01 | 0.30 |
| `em -p` | 0.842 s | 0.020 | 0.817 | 1.00 | 0.33 |
| `em -up` | 0.861 s | 0.035 | 0.820 | 1.02 | 0.36 |
| `em -vDp` | 0.884 s | 0.024 | 0.854 | 1.05 | 0.29 |
| `emerge -p` | 2.958 s | 0.037 | 2.909 | 2.84 | 0.11 |

Two things fall out. **Verbosity is nearly free** — `-vp` (0.837 s) against `-p`
(0.842 s) is inside noise, so the narration and download-size pass are not where
resolve time goes. And **all five resolve modes land within 5% of each other**
(0.834–0.884 s), so the cost is dominated by repo load and provider build, not by
which update set gets selected.

## Against the July history

| | 2026-07-25 | now | delta |
|---|---|---|---|
| `em -p firefox` | 1.290 s | 0.842–1.006 s | **-22% to -35%** |
| `emerge -p firefox` | 4.061 s | 2.958 s | -27% |
| speedup | 3.15× | 2.97–3.51× | not comparable |

**The ratio is not a valid comparison across two months.** `emerge` itself got
27% faster on this host over the same period (4.061 s → 2.958 s, Portage
3.0.82.2), so the denominator moved. The `em` absolute figure improved 22–35%;
whether the *relative* position improved is unanswerable from numbers taken two
months apart with a different Portage underneath.

## Noise on this host

`em -p` measured 1.006 s in the `bench-resolve-modes.sh` run and 0.842 s in the
bare `hyperfine` run twenty minutes later on the same binary — a 20% spread
between sessions, larger than any effect this session's changes are claimed to
have. The `results/` README convention already warns that the baseline drifts
±30 ms run-to-run; this is the coarser, between-invocation version of the same
problem. Single-session deltas below ~20% on this workload should not be
believed, and `--warmup 2` with 10 runs only controls within-invocation variance,
not this.

## Files

- `meta.env` — machine, load, Portage version, hyperfine version
- `hyperfine-verbose.json` — raw hyperfine export, all 6 commands × 10 runs
- `resolve-modes.txt` — full `bench-resolve-modes.sh` output incl. package parity
