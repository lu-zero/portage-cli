# Session perf A/B: `d737929e` vs `8704e49e`, 2026-09-26

The open question from the 2026-09-25 review: **do the session's 19 `perf:`
commutes compose into anything measurable end to end?** Each had a paired
microbenchmark; none of that adds up to a wall-clock claim on its own.

- **baseline:** `d737929e` — the commit before the session
- **current:** `8704e49e`
- **scope:** 60 commits, 19 `perf:`, 9 `fix:`, the rest refactor/docs
- **machine:** thalia (AmpereOne, 128 cores, 4 NUMA) —
  [`machines/thalia.md`](../../machines/thalia.md), all runs `numactl -N 0 -m 0`
- **target:** `www-client/firefox -p`, single host repo
- **profile:** `--release` for both; binaries md5-distinct (see `meta.env`)

## Why firefox and not `@system`

`em -p --emptytree @system` **cannot** be used: the plan differs between the two
commits — same 351 ebuilds but a different set, with `b122fc18` ("retain selected
virtual branches") changing which packages the graph keeps. A timing delta there
would mix "faster" with "different work".

`www-client/firefox` is the opposite case, and that is what makes the measurement
attributable: both binaries emit a **byte-identical 112-line plan**
(`plan-baseline.txt` vs `plan-head.txt`, timing line masked). Same work, so any
difference is the code.

## Result: no measurable difference

| method | baseline | current | delta |
|---|---|---|---|
| interleaved, one invocation | 894.6 ms ± 52.4 | 918.9 ms ± 76.0 | +2.7% (head slower) |
| each alone, own invocation | 876.8 ms ± 67.3 | 888.0 ms ± 56.9 | +1.3% (head slower) |
| minimum of 10 runs | 818.7 ms | 813.7 ms | −0.6% (head faster) |

The two statistics **disagree in sign** and both sit far inside the standard
deviations (57–76 ms on an ~880 ms measurement). The honest reading is that the
session's aggregate effect on this workload is **below the measurement floor** —
not a regression, and not a win.

Three methods were used deliberately: interleaved (the convention in
`results/20260816-.../README.md`) and single-command-per-invocation (the rule
established in `20260926-cli-firefox-.../`, after a grouped ten-command run proved
unreproducible). They agree, which is the point — the earlier two mistakes in this
session both came from trusting a single method.

## Caveat on the absolute numbers

System time in this session was 1.1–1.7 s, against ~0.33 s in the head-only runs
earlier the same day. Both binaries are equally affected, so the *ratio* stands, but
the absolute figures here (~880 ms) are higher than the ~834 ms measured earlier
and should not be compared with them. Machine state drifted between sessions;
only same-session A/B ratios are meaningful.

## What this does and does not say

- It does **not** say the 19 perf commits were useless. Each was a targeted
  improvement to a specific path with its own measurement, and those stand on
  their own evidence.
- It does say that **their sum is not observable in `em -p firefox`**, which is
  dominated by repo load and provider build. A workload that stresses the
  optimised paths specifically would be needed to see them — and finding that
  workload is the open item, not re-running this one.
- It also confirms the review's caution was right: 19 individual microbenchmark
  wins, each real, add up to nothing measurable at the CLI. That is the most
  common outcome for this shape of work, and it is only knowable by measuring.

## Files

- `meta.env` — machine, load, shas, binary checksums, parity statement
- `plan-baseline.txt` / `plan-head.txt` — the identical plans
- `hyperfine-interleaved.json` — both binaries, one invocation
- `hyperfine-baseline-alone.json`, `hyperfine-head-alone.json` — one per invocation
