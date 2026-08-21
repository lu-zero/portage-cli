# Lazy DEPEND-family parsing: new anchor, 2026-08-21

- **baseline:** `559b644` (previous anchor — VDB read path + set resolution, 2026-08-16)
- **current:** `c0d64e3` (HEAD after this session's work)
- **machine:** thalia (AmpereOne, 128 cores, 4 NUMA nodes, 256 GiB) — see
  [`machines/thalia.md`](../../machines/thalia.md).
- **why:** benchmarking the `ResolvedPolicy` refactor against this anchor
  surfaced a real, unrelated ~1.16-1.24x regression from `94ea5a1`'s
  `IUSE_EFFECTIVE` fix (bisected and fixed, `1e9a21b`), which recovered
  most but not all of the gap. A dhat allocation profile taken while
  chasing the residual gap found the actual dominant cost: `CacheEntry::
  parse` eagerly parsed every ebuild's DEPEND-family fields
  (DEPEND/RDEPEND/BDEPEND/PDEPEND/IDEPEND) for the whole tree — 47.7% of
  all bytes / 38% of all allocations for a single-package resolve — even
  though the solver only ever examines ~5% of the tree's CPNs as
  candidates. `LazyDepList` (`c0d64e3`) defers that parse to first access.

Methodology: worktree-built baseline (`~/Sources/portage-cli-anchor-
559b644`, sibling of `pkgcraft`/`brush`), every hyperfine comparison
interleaved-by-command within one invocation (`--warmup 2 -N -i`, 20
runs), `pgrep`/`ps` checked for a live build before every measurement,
load average 1.07 at the start of the run.

## Result: current master is now faster than the anchor, not just caught up

### 1. `em -p` — the four standard targets

| target | anchor `559b644` | current `c0d64e3` | ratio |
|---|---|---|---|
| `firefox` | 1.475 s ± 0.109 | 1.292 s ± 0.173 | **1.14 ± 0.17x faster** |
| `qtbase` | 1.435 s ± 0.094 | 1.166 s ± 0.126 | **1.23 ± 0.16x faster** |
| `texlive` | 1.410 s ± 0.106 | 1.200 s ± 0.145 | **1.18 ± 0.17x faster** |
| `@world` | 1.628 s ± 0.104 | 1.411 s ± 0.160 | **1.15 ± 0.15x faster** |

All four ratios are outside their error bars and all four move the same
direction by a similar amount — a real effect, not the uniform-drift noise
signature documented in the previous anchor. System time drops
substantially alongside wall clock (e.g. firefox: 28.2 s → 12.6 s across
20 runs) — consistent with the mechanism (less retained heap, less
page-fault/drop churn at exit), not just less CPU spent parsing.

Full raw output: [`hyperfine-interleaved.txt`](hyperfine-interleaved.txt).

### 2. `em regen` — unaffected, as expected

`regen` writes every ebuild's full metadata back to disk
(`CacheEntry::serialize()`), so it forces every DEPEND-family field
regardless of laziness — the optimization targets *resolves*, which only
touch a fraction of the tree, not `regen`, which touches all of it by
design.

5 runs each, real `/var/db/repos/gentoo` (32800 ebuilds), `-j20`:

| | anchor `559b644` | current `c0d64e3` | ratio |
|---|---|---|---|
| wall | 8.348 s ± 0.279 | 8.285 s ± 0.190 | 1.01 ± 0.04x |

Flat, inside noise, as expected. Output verified byte-identical between
the two binaries (`diff -rq`, full tree, exit 0) — the `LazyDepList`
plumbing changes nothing about what gets written.

Full raw output: [`hyperfine-regen.txt`](hyperfine-regen.txt).

## What moved and why

This anchor spans four commits since `559b644`:

- `5951b21` — `ResolvedPolicy` factoring (todo item 11). Confirmed flat
  on its own (interleaved A/B against its immediate parent, not repeated
  here) — pure construction-glue reorganization.
- `1e9a21b` — cache the profile-invariant half of `IUSE_EFFECTIVE`
  per solve instead of rebuilding it per candidate (fixes the `94ea5a1`
  regression this anchor run was chasing). Recovered most of that
  regression, not all — the residual is legitimate PMS-mandated
  force/mask matching that `94ea5a1` correctly added.
- `42522a4` — `--tree` display fix (unrelated root packages no longer
  nest under each other). Correctness only, no perf relevance.
- `c0d64e3` — **`LazyDepList`, the headline change here.** Defers
  DEPEND-family parsing to first access, memoized. dhat confirms the
  mechanism: dep-parsing allocation share dropped from 896MB/2.25M
  blocks (47.7%/38%) to 195MB/378K blocks (15.9%/9.3%) for
  `em -p sys-devel/gcc`.

Net effect: the four targets above are now **1.14-1.23x faster than the
`559b644` anchor**, not merely back to parity with it — the lazy-parsing
win outweighs the small residual cost of the `IUSE_EFFECTIVE` correctness
fix.

**Caught during implementation, not just measured after**: the first
`LazyDepList` pass wired the type everywhere but left the actual parse
eager at the construction site (wrapped an already-parsed result instead
of deferring it). Build/clippy/test/fmt/doc were all green and `em -p`
output was correct, but a wall-clock A/B showed no improvement — a dhat
re-run on the "finished" build showed identical allocation counts to
before the change, which is what caught it. Fixed before this anchor was
taken; see `git log -1 c0d64e3` for the corrected version's commit
message.

## Conclusion

New anchor for future comparisons: `c0d64e3`. Every `em -p` target
measured is faster than the previous anchor, `em regen` is unaffected
(confirmed both in timing and byte-identical output), and the residual
gap the previous anchor's chase originally set out to explain
(`94ea5a1`'s `IUSE_EFFECTIVE` regression) is now more than recovered.
