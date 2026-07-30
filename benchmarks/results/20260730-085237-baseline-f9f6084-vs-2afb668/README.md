# em vs em: did it get slower? (Theo's report, 2026-07-30)

- **date (UTC):** 2026-07-30T08:52–09:05
- **current:** `f9f6084` (HEAD)
- **baseline:** `2afb668` (last formal `-p` baseline, see
  `../20260725-222000-baseline-2afb668-confirm/`) — 120 commits back
- **method:** git worktree for the baseline, both built `--release`, timed
  head-to-head with `hyperfine -N -i` against the live host repo (see
  [[benchmark-baseline-worktree]] memory for why the worktree must be a
  sibling of `pkgcraft`/`brush`, not `/tmp`)
- **not** an em-vs-emerge run — this answers "did em regress against
  itself", using `bench-em-vs-emerge.sh`'s same four workloads

## Result: no regression — current is faster

| Benchmark | baseline (2afb668) | current (f9f6084) | delta |
|-----------|---------------------|---------------------|-------|
| firefox -p | 1.271 s ± 0.019 s | 1.080 s ± 0.020 s | **1.18× faster** |
| libreoffice -p | 1.604 s ± 0.024 s | 1.244 s ± 0.028 s | **1.29× faster** |
| multi (5 pkgs) -p | 1.758 s ± 0.022 s | 1.356 s ± 0.020 s | **1.30× faster** |
| gcc -s | 102.8 ms ± 12.4 ms | 99.4 ms ± 16.8 ms | ~flat (noise) |

Plan-shape parity (package counts) is identical baseline vs current on every
target checked, so the timing delta is real perf, not a smaller/larger
resolve graph. Raw hyperfine output: `hyperfine.txt`.

## But: there IS a real historical regression, just not in this window

`../20260725-222000-baseline-2afb668-confirm/README.md` already flagged that
`2afb668`'s `-p` times (~1.3-1.8 s) were **already regressed** relative to
2026-07-11 history (~0.8-0.98 s), and that the formal sweep at the time never
re-checked `-p` after that. This run only covers `2afb668 → f9f6084` (i.e.
the last 5 days / 120 commits) and finds that segment got *faster*, not
slower — it does not explain the earlier 2026-07-11 → 2026-07-23ish jump.

**If Theo's "got slower" impression predates 2026-07-25**, it's most likely
this older, still-unexplained gap, not anything in the current commit range.
Bisecting the 2026-07-11 → 2afb668 window (a real "formal sweep" tag/commit
from around 2026-07-11 would need to be located first) is the next step if
that older regression needs root-causing.

## Live-system note (unrelated to em)

`-p www-client/firefox` now exits 1 on this host on **both** binaries: an
installed `llvm-core/lldb-22.1.6` is pinned against the installed
`llvm-22.1.6` slot, conflicting with a proposed `llvm-22.1.8` update. This is
real host package drift since 2026-07-25 (this always meant "USE changes
required", not a new failure mode), not an em bug — confirmed by running
both binaries directly (not just via hyperfine) and seeing identical
USE-change-required output on each.
