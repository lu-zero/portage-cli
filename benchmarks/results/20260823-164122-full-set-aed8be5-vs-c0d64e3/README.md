# Full unpushed patch set: confirm no regression, 2026-08-23

- **baseline (anchor):** `c0d64e3` (lazy DEPEND-family parsing, 2026-08-21 —
  unchanged worktree anchor, `~/Sources/portage-cli-anchor`)
- **current:** `aed8be5` (HEAD — the full unpushed set, 15 commits after
  folding pure-refinement follow-ups into their parent):
  `9096768` REQUIRED_USE merge gating, `8accdb0` CONFIG_PROTECT docs,
  `b4c9273` `package.provided` modeled as an installed package (not an edge
  filter), `925f157` `--json` order-violation info + PDEPEND/IDEPEND
  direction fix, `3e1d534` soft-cycle repair BDEPEND priority +
  `dependency_graph` consistency fix, `fd5dcea` TIER1 additions
  (python/meson-format-array) to break a real bootstrap cycle, `ed31b46`
  real `dev-lang/perl` `--local` bootstrap step, `aed8be5` single dual-mode
  builtin registry (fixes `eapply`'s `PATCHES` silently no-oping, replaces
  bash-stub shadowing with `commands::dual_mode::set_tool_mode`) — plus docs
  commits recording each.
- **machine:** thalia (AmpereOne, 128 cores, 4 NUMA nodes, 256 GiB) — see
  [`machines/thalia.md`](../../machines/thalia.md).
- **why:** last anchor comparison was `20260822-181720` (`99f54e2` vs
  `c0d64e3`, flat). A full day of further work landed since: REQUIRED_USE
  gating, the `package.provided` solver redesign, RDEPEND scheduling fixes,
  and the `eapply`/dual-mode builtin registry rework — none perf-motivated,
  but `package.provided` and the `graph.rs` changes touch the solver's hot
  path, so re-confirming against the same fixed anchor before pushing.

Methodology: same worktree anchor as prior runs (`~/Sources/portage-cli-anchor`,
sibling of `pkgcraft`/`brush`, pinned at `c0d64e3`), both sides built
`--release`. Every `-p` hyperfine comparison interleaved-by-command within one
invocation (`--warmup 3 -N -i -m 20`). NUMA-pinned to an idle node
(`numactl --cpunodebind=1 --membind=1`; node 1 had the most free memory of the
4 at benchmark time). `pgrep` confirmed no other `em`/`cargo`/`rustc` process
before every measurement.

## Result: consistently faster, no regression

### 1. `em -p` — the four standard targets

| target | anchor `c0d64e3` | current `aed8be5` | ratio |
|---|---|---|---|
| `firefox` | 1.083 s ± 0.089 | 973.2 ms ± 65.1 | 1.11x |
| `qtbase` | 1.017 s ± 0.088 | 944.3 ms ± 67.0 | 1.08x |
| `texlive` | 1.074 s ± 0.087 | 952.1 ms ± 71.6 | 1.13x |
| `@world` | 1.243 s ± 0.090 | 1.156 s ± 0.088 | 1.08x |

All four targets land in `aed8be5`'s favor, consistently in the 1.08-1.13x
range with tight, overlapping-but-directionally-consistent error bars —
unlike the single-outlier pattern seen in prior noise ("`texlive` alone
faster", not promoted as signal), here *every* target moved the same
direction by a similar margin. Plausibly real (`package.provided`'s
`InstalledPolicy::Provided` and the `graph.rs` soft-cycle-repair changes
both touch code every resolve runs through), but this run doesn't isolate
which commit — noted as an observation, not investigated further here since
the goal was confirming no regression, not chasing a win.

Full raw output: [`hyperfine-interleaved.txt`](hyperfine-interleaved.txt).

### 2. `em regen` — faster, output byte-identical

Real `/var/db/repos/gentoo` (32,800 ebuilds), `-j20`, NUMA-pinned:

| | anchor `c0d64e3` | current `aed8be5` | ratio |
|---|---|---|---|
| wall (hyperfine, 5 runs) | 8.396 s ± 0.109 | 7.728 s ± 0.105 | 1.09x |

Output verified byte-identical on a separate, untimed pass into isolated
output directories (`diff -rq`, all 32,800 files each side, exit 0) — the
first interleaved attempt shared a `--prepare` step between both benchmarks
that wiped the other side's directory before its own last run; redone with
per-side directories and no shared prepare for the identity check.

Full raw output: [`hyperfine-regen.txt`](hyperfine-regen.txt).

## Conclusion

No regression from `c0d64e3` across the full 15-commit unpushed set.
`em -p` and `em regen` both land consistently faster (~8-13%) with
byte-identical `regen` output — likely attributable to the solver-path
changes (`package.provided`, `graph.rs`) rather than any of this set being
perf-motivated. `c0d64e3` remains the anchor going forward.
