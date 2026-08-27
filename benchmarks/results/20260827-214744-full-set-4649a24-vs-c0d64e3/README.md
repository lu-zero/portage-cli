# Full unpushed set: confirm no regression + new anchor, 2026-08-27

- **baseline (anchor worktree):** `e5344f5` — the standing anchor since
  `20260826-widen-live-set-0141c809-vs-c0d64e3` (itself code-identical to
  `aed8be5`, which `20260823-164122` confirmed flat against `c0d64e3`).
- **current:** `4649a24` — HEAD, everything since `e5344f5` (67 commits,
  several days of work): the rest of the widened-autounmask arc's own
  follow-through, the crossdev libc-headers/board-root fixes
  (`b8c7ea9`, `2f795ad`, `cac0549`, `f8a072c`), and this session's own
  arc — `MergeRoot::Base`/`base_copies` (the toolchain-sysroot DEPEND
  double-plan fix, PMS table 8.2), the activity-log display fix, and the
  `Arc<str>`/typed-`Cpv`/`Cpn`/`RunPhase` follow-up
  (`236e036`..`4649a24`). None of this touches the `-p` resolve hot
  path directly — `base_copies`/`MergeRoot::Base` is a post-solve walk,
  and the typed-schema work is confined to the activity log and
  `ebuild.rs`'s phase dispatch — but it's been several days since the
  last confirm, so re-checking against the fixed anchor before pushing.
- **machine:** thalia (AmpereOne, 128 cores, 4 NUMA nodes, 256 GiB) — see
  [`machines/thalia.md`](../../machines/thalia.md).

Methodology: same worktree anchor as prior runs (`~/Sources/portage-cli-anchor`,
sibling of `pkgcraft`/`brush`, pinned at `e5344f5`), both sides built
`--release` fresh today. Every `-p` hyperfine comparison
interleaved-by-command within one invocation (`--warmup 3 --shell=none
--ignore-failure --min-runs 20`). NUMA-pinned to an idle node
(`numactl --cpunodebind=1 --membind=1`; node 1 had the most free memory
and no other `em`/`cargo`/`rustc` process at benchmark time, confirmed
via `pgrep`).

## Result: flat, no regression

### 1. `em -p` — the four standard targets

| target | anchor `e5344f5` | head `4649a24` | ratio |
|---|---|---|---|
| `firefox` | 1.108 s ± 0.104 | 1.056 s ± 0.068 | 1.05x (head faster) |
| `qtbase` | 995.1 ms ± 68.9 | 1.011 s ± 0.076 | 1.02x (anchor faster) |
| `texlive` | 1.062 s ± 0.080 | 1.060 s ± 0.070 | flat |
| `@world` | 1.229 s ± 0.082 | 1.229 s ± 0.067 | flat |

No consistent direction — unlike the uniform 1.10x pattern
`20260823`/`20260826` found for those (perf-relevant) arcs, here the two
sides interleave within each other's error bars. Expected: nothing in
this gap changes the `-p` solve/display path.

Raw output: [`hyperfine-interleaved.txt`](hyperfine-interleaved.txt),
[`hyperfine.json`](hyperfine.json).

### 2. Criterion `resolve` — flat, confirmed against same-commit drift

Two anchor rounds (order-swapped around the head round) to establish
this host's own noise floor for today, then one head round:

| bench | anchor round 1 | anchor round 2 (same commit) | head `4649a24` | head vs. anchor r1 |
|---|---|---|---|---|
| load_repo | 988.99 ms | 989.70 ms | 999.58 ms | +1.1% |
| build_provider | 531.19 ms | 530.15 ms | 521.83 ms | −1.8% |
| targets/firefox | 11.637 ms | 12.158 ms (+4.5%) | 12.118 ms | +4.1% |
| targets/gcc | 4.2002 ms | 4.1379 ms (−1.5%) | 4.3240 ms | +2.9% |
| targets/rust | 7.5318 ms | 7.9284 ms (+5.3%) | 7.7244 ms | +2.6% |
| targets/openssh | 3.8850 ms | 3.8819 ms | 4.0072 ms | +3.1% |
| targets/python | 5.4433 ms | 5.3878 ms | 5.5518 ms | +2.0% |

The two same-commit anchor rounds alone drift up to **5.3%**
(`targets/rust`) and **4.5%** (`targets/firefox`) — matching the
already-documented ~5-6% noise floor for this bench on this host. Every
head-vs-anchor delta above sits inside that same range, so none of it
is distinguishable from noise. Combined with the flat `-p` wall-clock
numbers, this is a clean confirm, not a regression.

Raw outputs: [`crit-anchor.txt`](crit-anchor.txt),
[`crit-anchor2.txt`](crit-anchor2.txt), [`crit-head.txt`](crit-head.txt).

## New anchor

Advanced the standing anchor worktree (`~/Sources/portage-cli-anchor`)
from `e5344f5` to `4649a24` (this HEAD) — confirmed flat above, and the
prior anchor was itself two arcs and several days behind. Use `4649a24`
as the anchor for future comparisons until the next confirm.
