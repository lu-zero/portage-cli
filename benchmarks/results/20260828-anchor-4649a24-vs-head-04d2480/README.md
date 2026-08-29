# Anchor 4649a24 vs head 04d2480: apparent 11-12% regression, confirmed LTO artifact

- **baseline (anchor worktree):** `4649a24` — standing anchor since
  [[benchmark-anchor-4649a24]].
- **current:** `04d2480` — everything since the anchor: crossdev
  config-site merge, PMS phase builtins, the `mirror://gentoo/`
  filename-hash fix, the `--jobs N` host_copies/base_copies blocker-edge
  fix, `todo/` doc-comment cleanup, and the `root_closure` consolidation
  refactor.
- **machine:** thalia (AmpereOne, 128 cores, 4 NUMA nodes) — see
  [`machines/thalia.md`](../../machines/thalia.md). Other unbound sessions
  (`opencode`, `maki`, `rust-analyzer`) were active throughout, per this
  host's usual multi-session load.

Methodology: same anchor worktree as prior runs
(`~/Sources/portage-cli-anchor`), interleaved `hyperfine` (`--warmup 3
--shell=none --ignore-failure --min-runs 20`), `numactl --cpunodebind=0
--membind=0` (node 0: most free memory, confirmed idle via `pgrep`/`ps`
at benchmark time).

## Result: real wall-clock delta, but it's an LTO codegen artifact, not a logic regression

### 1. Initial four-target run — consistent ~11-12% slower

| target | anchor `4649a24` | head `04d2480` | ratio |
|---|---|---|---|
| firefox | 1.027 s ± 0.111 | 1.145 s ± 0.084 | 1.11x slower |
| qtbase | 987.2 ms ± 119.4 | 1.094 s ± 0.092 | 1.11x slower |
| texlive | 980.6 ms ± 80.2 | 1.099 s ± 0.079 | 1.12x slower |
| @world | 1.140 s ± 0.069 | 1.272 s ± 0.096 | 1.12x slower |

Unlike the prior anchor confirm ([[benchmark-anchor-4649a24]], "no
consistent direction"), this is uniform across all four targets — worth
chasing rather than dismissing as noise. A same-commit anchor-vs-anchor
round (`@world`, 20 runs each) measured only ~1% drift, ruling out
today's ambient noise floor as the explanation.

Raw: [`hyperfine-interleaved.txt`](hyperfine-interleaved.txt),
[`anchor-vs-anchor.txt`](anchor-vs-anchor.txt).

### 2. Bisection: the delta is already present at `8ac0fa6`

Three-way interleaved (`@world`, 20 runs each), a fresh-today rebuild of
the anchor (ruling out stale-binary/day drift), and a worktree built at
`8ac0fa6` (the `--jobs N` blocker-edge fix, the first commit after the
anchor to touch `query/depgraph/mod.rs`):

| binary | time | vs anchor |
|---|---|---|
| anchor (fresh rebuild) | 1.173 s ± 0.083 | — |
| `8ac0fa6` | 1.299 s ± 0.065 | 1.11x slower |
| `04d2480` (head) | 1.315 s ± 0.091 | 1.12x slower |

`04d2480` (the later `root_closure` refactor) is flat relative to
`8ac0fa6` — the delta is entirely attributable to `8ac0fa6`.

Raw: [`fresh-anchor-recheck.txt`](fresh-anchor-recheck.txt),
[`bisect-world.txt`](bisect-world.txt).

### 3. `perf stat`: genuinely more work, not measurement noise

Single-run hardware counters, `@world`:

| | anchor | head | delta |
|---|---|---|---|
| instructions | 6,098,741,366 | 6,466,906,337 | +6.0% |
| cycles | 3,828,058,221 | 4,242,569,892 | +10.8% |
| page-faults | 2,881 | 3,387 | +17.6% |

### 4. `perf diff`: the extra work isn't in the new code at all

Symbolized `perf diff` between the two profiles shows no
`root_closure`/`base_copies`/`host_copies` symbol anywhere — the actual
changed code never appears as hot, consistent with its early-return gate
(`cross.active`/`base_merge_root()`) correctly short-circuiting for this
bare, non-offset invocation. Verified against `root_aware::detect`
directly, and against a clean environment with no leftover
`ROOT`/`SYSROOT`/`TARGET` env vars.

The dominant delta is in unrelated hot functions instead:

```
     0.72%     +7.88%  em  [.] <RandomState as BuildHasher>::hash_one::<&Interned<GlobalInterner>>
               +5.65%  em  [.] portage_resolve::force_mask::apply_signed
     4.83%     -4.66%  em  [.] <ForceMask>::effective
```

`force_mask.rs` wasn't touched by `8ac0fa6` at all. This is the
signature of whole-program LTO reacting to a source change anywhere in
a hot compilation unit (`query/depgraph/mod.rs`) by shifting inlining,
register-allocation, and code-layout decisions in unrelated functions —
not a real algorithmic cost from the new logic executing.

Raw: `perf-anchor.data`/`perf-head.data` (scratch, not committed —
regenerate via `perf record -F 999` on each binary then `perf diff`),
[`diff-output.txt`](diff-output.txt).

### 5. Confirmation: disable LTO, delta vanishes

Both sides rebuilt with `CARGO_PROFILE_RELEASE_LTO=false` (same source,
same commits, only the LTO flag changed):

| binary | time |
|---|---|
| anchor, no LTO | 1.310 s ± 0.082 |
| head, no LTO | 1.304 s ± 0.085 |

Flat — head is marginally faster if anything, well inside the error
bars. This confirms §4: the regression is entirely an LTO
whole-program-codegen artifact of this specific source diff's shape,
not a real cost of the new logic.

Raw: [`nolto-comparison.txt`](nolto-comparison.txt).

## Conclusion

No action needed on `8ac0fa6`/`04d2480`'s actual logic — both are
functionally correct (full test suite, clippy, fmt, rustdoc all clean;
live-verified via two real crossdev-stages stage3 builds, i586 and
riscv64, the latter specifically exercising the `--jobs N` fix this
regression hunt started from). The LTO sensitivity is real but not
actionable per-commit — an inherent property of full-LTO release builds
where any change can perturb unrelated hot-path codegen either way.

## Anchor

Not advancing the anchor from this investigation — the LTO-sensitive
wall-clock numbers here aren't a fair basis for a new baseline. Anchor
stays `4649a24` until the next confirm run establishes a clean number
with today's code.

## Follow-up: `force_mask` hot-spot investigation and fix (`44a208d`)

Per §4's profile, `portage_resolve::force_mask::apply_signed` was the
biggest *named* hot function (5.65% of samples). `ForceMask::effective()`
re-scanned each profile layer's raw `use.force`/`use.mask`/`use.stable.*`
token list (hundreds of entries) per package, doing an `IUSE` membership
check per token — the doc comment already claimed
O(package IUSE ∩ global set), but the code was still O(global set) per
call.

**First attempt (reverted in spirit, corrected in `44a208d`):** folded
each layer's token list to a `HashMap<Flag, bool>` once at profile-load
time, then always iterated the package's `IUSE_EFFECTIVE` set instead.
This made things measurably *worse* — confirmed via `perf stat`
instruction counts (6.74B vs anchor's 6.47B, +4%) — because
`IUSE_EFFECTIVE` includes every profile-injected `USE_EXPAND` flag (PMS
11.1.1: `LINGUAS`, `VIDEO_CARDS`, `PYTHON_TARGETS`, …), so it is not
reliably the smaller side the way the doc comment assumed.

**Fix:** `apply_folded` now compares `iuse.len()` vs the folded set's
length and walks whichever is actually smaller, per call. Verified via
`perf stat` (exact per-process instruction counts, not statistical wall
clock — this host had an unrelated heavy C++ build running throughout
this part of the investigation, making hyperfine unreliable):

| | instructions (`@world`) |
|---|---|
| anchor `4649a24` | 6,134,290,026 / 6,138,637,332 (two rounds) |
| head `44a208d` | 6,049,542,168 / 6,088,559,714 (two rounds) |

Head now executes *fewer* instructions than anchor — a real
improvement, on top of (not instead of) the LTO codegen noise §5
already characterized. Wall-clock under the ongoing background-build
contention showed anchor only ~6% faster (down from the ~11-21% seen
mid-investigation with the wrong iteration side), consistent with the
LTO-noise floor from §5 rather than a remaining logic cost.

Full test suite (522 + 157 across `portage-resolve`/`portage-cli`)
passes unchanged, including
`later_use_mask_unmask_cancels_earlier_package_stable_mask` — the
per-layer, interleaved-with-package-rules application order this
change is not allowed to disturb.

## Clean wall-clock rerun, 2026-08-29 (`44a208d` vs `4649a24`)

vllm.cpp was gone (load 1.48 at start, node 0 still the freest at ~26 GiB).
Both binaries rebuilt `--release` today, NUMA-pinned
`numactl --cpunodebind=0 --membind=0`, same interleaved `hyperfine`
(`--warmup 3 --shell=none --ignore-failure --min-runs 20`).

| target | anchor `4649a24` | head `44a208d` | ratio |
|---|---|---|---|
| firefox | 1.017 s ± 0.081 | 1.017 s ± 0.077 | flat |
| qtbase | 968.8 ms ± 84.1 | 1.003 s ± 0.105 | 1.03x (inside error) |
| texlive | 984.4 ms ± 77.4 | 1.045 s ± 0.078 | 1.06x (overlaps) |
| @world | 1.155 s ± 0.075 | 1.183 s ± 0.074 | 1.02x (overlaps) |

User time is essentially identical (firefox 1.252 vs 1.287 s; qtbase /
texlive / @world all within 7 ms). The leftover wall-clock jitter is
system time — the same LTO/page-fault noise §5 already characterized,
not a remaining logic cost. The 11–12% regression vs `04d2480` is gone.

`perf stat` `@world`, two rounds, same pin:

| | instructions |
|---|---|
| anchor `4649a24` | 6,099,887,003 / 6,092,591,934 |
| head `44a208d` | 6,043,969,783 / 6,047,536,896 |

Head still executes ~0.9% fewer instructions than the anchor, matching
the noisy-host counts from the follow-up above.

Raw: [`hyperfine-clean.txt`](hyperfine-clean.txt),
[`hyperfine-clean.json`](hyperfine-clean.json),
[`perf-stat-clean.txt`](perf-stat-clean.txt).

Standing anchor stays `4649a24`. This run is a clean number, so
`44a208d` is a fair candidate to advance to; not moved here because
the LTO-shaped `8ac0fa6` delta is still in the gap and the original
conclusion was not to bake that into a baseline.
