# Widened-autounmask arc: confirm no regression + new anchor, 2026-08-26

- **baseline (anchor worktree):** `e5344f5` — code-identical to `aed8be5`,
  which `20260823-164122` confirmed flat against the standing `c0d64e3`
  anchor (the worktree has since advanced only by that bench-results
  commit).
- **current:** `0141c809` — HEAD, everything since `bb9280e`: the brush
  fork bump (`5f04f59`), the crossdev libc-headers reorder fix
  (`b8c7ea9`, crossdev-only paths), and the widened-autounmask arc
  (candidate-supply widening with acceptance tiering, live-ebuild policy
  layer, dropped-dep escalation, invocation-gated persistence — commits
  `07a7b90..0141c809`, 12 code commits).
- **machine:** thalia (AmpereOne, 128 cores) — NUMA-pinned to node 1,
  both sides built `--release` fresh today, hyperfine interleaved
  (`--warmup 3 -N -i -m 20`), criterion run separately per tree with
  deltas from absolute point estimates.
- **why:** the arc touches resolver-core hot paths even in default mode:
  `choose_version` gained a tier filter on every call, provider
  ingestion grew tagging/liveness facts, and a post-ingestion
  propagation pass was added. Re-confirming the default path is flat
  before pushing, per house rule.

## Result

### 1. `em -p` hyperfine (interleaved) — head faster across all four targets

| target | anchor `c0d64e3` | head `0141c809` | ratio |
|---|---|---|---|
| firefox | 986.8 ms ± 23.8 | 894.3 ms ± 15.4 | 1.10x |
| qtbase | 929.2 ms ± 16.2 | 841.3 ms ± 18.1 | 1.10x |
| texlive | 965.5 ms ± 18.1 | 861.7 ms ± 19.0 | 1.12x |
| @world | 1.151 s ± 0.019 | 1.048 s ± 0.017 | 1.10x |

Uniform 1.10x-ish across every target with tight bars — the
same-direction-everywhere pattern `20260823-164122` called plausibly
real for its own set. Not attributable today: the unpushed set includes
the brush fork bump and twelve unrelated commits, and the identical-
binary drift documented for this host (±6% over days) overlaps part of
the range. Recorded as an observation; no regression either way.

Raw output: [`hyperfine-interleaved.txt`](hyperfine-interleaved.txt).

### 2. Criterion `resolve` — flat after one real regression was caught and fixed

Three rounds, order-swapped (anchor→head, then head→anchor), plus a
third head round after the fix below.

**The anchor caught a real regression**: `build_provider` landed +16%
(round 1) / +20% (round 2) above baseline, consistent across orders —
traced to `propagate_needs_unmask` walking every Choice/SlotChoice node
(and cloning branch constraints) on *every* provider build, though
tagging only exists under widened candidate supply. Fixed by gating the
walk on an ingestion-time scan for any tagged/live version
(`0141c80`). Post-fix: 528.6 ms vs anchor 526.7/536.1 ms — flat.

The same pre-fix runs also showed firefox resolve −13–16% "faster" —
which vanished once the gate landed, revealing its cause: the ungated
pass mutated Choice branches' tags even in strict mode, accidentally
pruning the search space. An unintended behavior change, not a win;
strict mode now matches baseline semantics exactly.

Post-fix full table (head vs anchor, best-of-two-rounds each side):

| bench | anchor | head | delta |
|---|---|---|---|
| load_repo | 993.72 ms | 992.09 ms | −0.2% |
| build_provider | 526.72 ms | 528.56 ms | +0.4% |
| targets/firefox | 12.238 ms | 12.284 ms | +0.4% |
| targets/gcc | 4.2769 ms | 4.4105 ms | +3.1% |
| targets/rust | 7.8908 ms | 7.9347 ms | +0.6% |
| targets/openssh | 4.0048 ms | 4.0838 ms | +2.0% |
| targets/python | 5.6478 ms | 5.6541 ms | +0.1% |

All within the documented ~5% noise floor for this bench on this host.

Raw outputs: [`crit-anchor.txt`](crit-anchor.txt),
[`crit-anchor2.txt`](crit-anchor2.txt),
[`crit-head.txt`](crit-head.txt) (pre-fix),
[`crit-head2.txt`](crit-head2.txt) (pre-fix),
[`crit-head3.txt`](crit-head3.txt) (post-fix).
