# Full unpushed patch set (Grok + Opus review passes): confirm no regression, 2026-08-22

- **baseline (anchor):** `c0d64e3` (lazy DEPEND-family parsing, 2026-08-21 — unchanged worktree anchor)
- **current:** `99f54e2` (HEAD — the full unpushed set: `fb7121c` lazy SRC_URI + eclass
  interning, `2f64914` `Lazy<T>` extraction, `290ce4c` `is_empty_raw`→`is_empty` serialize
  fix, `db3ed9b` malformed-vs-empty parse-failure tracking, `855b418` closing the
  resolve-time re-source path's own gap, `99f54e2` failing parse on a truncated
  `_eclasses_` checksum)
- **machine:** thalia (AmpereOne, 128 cores, 4 NUMA nodes, 256 GiB) — see
  [`machines/thalia.md`](../../machines/thalia.md).
- **why:** `855b418` was already confirmed flat against this same anchor
  (see `20260821-225508-parse-failure-tracking-855b418-vs-c0d64e3/`). One more commit
  (`99f54e2`) landed on top since — a correctness fix (a complete `_eclasses_` pair with
  a bad checksum now fails `CacheEntry::parse` instead of being silently trusted) written
  with Grok, on top of a set that had two rounds of Opus design review. Re-running to
  confirm the full six-commit set together still doesn't move wall-clock, since none of
  it is perf-motivated and `99f54e2` specifically only touches a validation branch inside
  an already-parsed hex string.

Methodology: same worktree anchor as prior runs
(`~/Sources/portage-cli-anchor`, sibling of `pkgcraft`/`brush`, pinned at `c0d64e3`),
every `-p` hyperfine comparison interleaved-by-command within one invocation
(`--warmup 3 -N -i -m 20`). **Differs from prior runs:** the machine had other unrelated
sessions active this time (a `grok` process, and repeated `check-release-binary-contract.py`
/ `agent-preflight.sh` test-script invocations, all unbound across all 128 cores) — an
unpinned first pass showed inflated variance (firefox σ up to 208 ms, ~30% System-time
inflation) consistent with cross-process CPU contention. Rebound both binaries to a
single idle NUMA node
(`numactl --cpunodebind=3 --membind=3`) for the real run below, which dropped System time
back down to the prior runs' range and tightened variance substantially. `pgrep`/`ps`
confirmed no other `em`/`cargo`/`rustc` process before every measurement.

## Result: flat to slightly favorable, no regression

### 1. `em -p` — the four standard targets

| target | anchor `c0d64e3` | current `99f54e2` | ratio |
|---|---|---|---|
| `firefox` | 1.116 s ± 0.072 | 1.085 s ± 0.090 | 1.03x (within noise) |
| `qtbase` | 1.100 s ± 0.068 | 1.042 s ± 0.097 | 1.06x (borderline, within combined σ) |
| `texlive` | 623 ms ± 96 | 479 ms ± 78 | 1.30x (see note below) |
| `@world` | 1.256 s ± 0.102 | 1.276 s ± 0.069 | 0.98x (within noise) |

Three of four targets are flat within this project's established ~5% noise floor.
`texlive` shows a larger gap in both an unpinned and the pinned run (1.18x and 1.30x
respectively) — consistently in `99f54e2`'s favor, but `texlive` is also the shortest of
the four probes (sub-second), where fixed per-run startup/syscall overhead dominates and
relative noise is highest (σ ~15-16% here vs ~7-9% for the other three). No commit in
this set plausibly speeds up resolution — `99f54e2` only adds a length check inside an
already-parsed hex string — so this reads as machine noise, not a real effect; matches
this project's prior finding that same-commit deltas can drift several percent day to day
on this box. Not promoting `texlive` alone as a signal.

Full raw output: [`hyperfine-interleaved.txt`](hyperfine-interleaved.txt).

### 2. `em regen` — flat, output byte-identical

5 runs each, real `/var/db/repos/gentoo` (32800 ebuilds), `-j20`, numactl-pinned, into
separate `-o` output directories:

| | anchor `c0d64e3` | current `99f54e2` | ratio |
|---|---|---|---|
| wall | 8.056 s ± 0.102 | 8.084 s ± 0.075 | 1.00 ± 0.02x |

Output verified byte-identical (`diff -rq`, all 32800 files, exit 0).

Full raw output: [`hyperfine-regen.txt`](hyperfine-regen.txt).

## Conclusion

No regression from `c0d64e3` across the full six-commit unpushed set, including the new
`99f54e2` truncated-checksum fix. `c0d64e3` remains the anchor going forward.
