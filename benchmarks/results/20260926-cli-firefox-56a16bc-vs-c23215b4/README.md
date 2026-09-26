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

## Cache sensitivity: measured, and it is not what it looks like

`em -p` measured 1.006 s in the `bench-resolve-modes.sh` run and 0.842 s twenty
minutes later on the same binary. The obvious explanation — the two tools
contending for the same 33,121-file metadata cache — is **wrong**, and testing it
directly says so.

| condition | `em -p` | `emerge -p` |
|---|---|---|
| alone, one command per invocation (3 separate runs) | 834.4 / 837.4 / 833.7 ms, min 802–808 | 2.943 s ± 0.028 |
| interleaved with `emerge -p`, one invocation | 848.0 ms ± 0.011 | 2.969 s ± 0.050 |
| same 10-command set as the script, one invocation | 843 ms ± 0.017 | 3.336 s ± 0.488 |

So mutual contention costs `em` about **1%** (834 → 848 ms). And the script's
1.006 s does not reproduce: the identical 10-command set measures `em -p` at
0.843 s. The real effect is in **`emerge`**, which is erratic *alone* in some
sessions (σ 0.49, range 2.96–3.97 s) and tight in others (σ 0.03), while its
`-uNp`/`-up`/`-uNDp` modes are consistently tight.

The giveaway that a multi-command invocation is the problem: in the ten-command
run, `em -up` (1.171 s) came out **slower** than `em -uDp` (0.978 s), and
`em -p`/`-uNp`/`-uDp`/`-uNDp` reordered between runs. Those share no state that
could invert their order, so a grouped hyperfine invocation is leaving each
command in a cache state that is not reproducible between invocations — and
`em`'s 33k-file metadata read is sensitive to it.

**Therefore: one command per invocation, and report the minimum.** Taken that
way `em -p www-client/firefox` is **834 / 837 / 834 ms across three independent
invocations, min 800–808 ms** — a spread of 0.4%, not 20%.

Practical rules this yields:

- Never compare a figure from a heterogeneous command set against one from a
  single-command set. The 2026-07-25 history came from
  `bench-em-vs-emerge.sh` (a multi-benchmark invocation); the 0.834–0.848 s
  figures came from single-command invocations. Those two are not directly
  comparable, and the honest comparison is `1.290 s` vs `~0.84 s` with a
  systematic bias favouring whichever was measured in a cleaner cache.
- `em -p` is ~0.84 s and `emerge -p` is ~2.94 s, so the honest single-repo
  speedup is **~3.5x**, not the 2.97x the grouped run reported.
- Absolute minima are stable across invocations; means are not. Quote the min.

## Files

- `meta.env` — machine, load, Portage version, hyperfine version
- `hyperfine-verbose.json` — raw hyperfine export, all 6 commands × 10 runs
- `resolve-modes.txt` — full `bench-resolve-modes.sh` output incl. package parity
