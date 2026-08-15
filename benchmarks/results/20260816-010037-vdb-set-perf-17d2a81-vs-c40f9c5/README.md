# VDB read path + set resolution: new anchor, 2026-08-16

- **baseline:** `c40f9c5` (master before this session's perf work)
- **current:** `17d2a81` (HEAD after the 11-commit VDB/set-resolution pass)
- **machine:** thalia (AmpereOne, 128 cores, 4 NUMA nodes, 256 GiB) — see
  [`machines/thalia.md`](../../machines/thalia.md). Not comparable to the
  M2 Max numbers in `machines/mneme.md`.
- **why:** `em --info -v` was 1.9s and half a gigabyte of RSS. The pass that
  followed touched the whole VDB read path (CONTENTS reading and caching,
  slot/USE/IUSE interning) as well as `@module-rebuild`/`@x11-module-rebuild`
  /`@security` resolution, so it needs an anchor on both the CLI paths it
  targeted and the solver paths it merely passes through.

Methodology per [[benchmark-baseline-worktree]] and
[[feedback-criterion-internal-delta-not-a-real-ab]]: baseline built in a
worktree at `/home/lu_zero/Sources/portage-cli-bench-baseline` (sibling of
`pkgcraft`/`brush`, so the relative `../../pkgcraft` path dep resolves),
criterion run separately in each tree with deltas computed by hand from the
absolute point estimates — never criterion's own `change:` line — and every
hyperfine comparison interleaved within a single invocation, since baseline
drifts ±30ms run-to-run here. `pgrep` checked for builds before each
measurement; load average was 1.10 at the start of the run.

## Result: the targeted path is 7.9x faster and 16x leaner; nothing else moved

### 1. `em --info -v` — the path the work targeted

Interleaved hyperfine, 20 runs, `--warmup 2 -N`:

| | baseline `c40f9c5` | current `17d2a81` | delta |
|---|---|---|---|
| wall | 1.877 s ± 0.029 | **237.5 ms ± 17.6** | **7.90 ± 0.60x faster** |

Peak RSS, 5 interleaved runs (`/usr/bin/time -f %M`):

| | baseline | current |
|---|---|---|
| peak RSS | 229–659 MB (varies per run) | **32.8 MB every run** |

The baseline's RSS swing is itself a finding: it memoized every package's
`CONTENTS` in the process-wide field cache, so peak depended on how much of
the 149 MB it happened to touch. Current is flat because `CONTENTS` no longer
enters that cache.

Resolved atom lists are byte-identical between the two binaries (only the
deliberate "not implemented" rewording of the nine unimplemented Portage sets
differs, which is excluded from the diff).

### 2. `em -p` — depgraph paths the work only passes through

Interleaved hyperfine, 10 runs each, `-i` (these targets exit non-zero on
USE-change output):

| target | baseline `c40f9c5` | current `17d2a81` | ratio |
|---|---|---|---|
| `@world` | 1.456 s ± 0.072 | 1.482 s ± 0.097 | 1.02 ± 0.08 |
| `firefox` | 1.260 s ± 0.066 | 1.279 s ± 0.113 | 1.02 ± 0.10 |
| `qtbase` | 1.236 s ± 0.128 | 1.267 s ± 0.078 | 1.03 ± 0.12 |
| `texlive` | 1.309 s ± 0.110 | 1.374 s ± 0.073 | 1.05 ± 0.10 |

All four ratios have error bars spanning 1.0, and all four moved in the same
direction by a similar small amount — the signature of environment noise
rather than a code effect. These runs are system-time dominated (sys 7.0–11.6 s
against user 2.7–3.1 s), so the CPU the interning saves does not surface in
wall clock; `@world`'s user time did drop 3.104 s → 2.943 s.

**No regression, and no demonstrable improvement, on the solver paths.**

### 3. Criterion `resolve` — microbench

Separate full runs per tree; deltas computed from the absolute point
estimates in `resolve-bench-baseline-c40f9c5.txt` /
`resolve-bench-current-17d2a81.txt`.

| Target | baseline `c40f9c5` | current `17d2a81` | delta |
|---|---|---|---|
| `load/load_repo` | 1.2957 s | 1.2805 s | -1.2% |
| `load/build_provider` | 554.36 ms | 539.42 ms | -2.7% |
| `targets/firefox` | 12.237 ms | 12.000 ms | -1.9% |
| `targets/gcc` | 4.5232 ms | 4.3911 ms | -2.9% |
| `targets/rust` | 8.0595 ms | 8.1749 ms | +1.4% |
| `targets/openssh` | 4.2368 ms | 4.1503 ms | -2.0% |
| `targets/python` | 5.8682 ms | 5.7483 ms | -2.0% |

`load_repo` and `build_provider` touch none of the changed code — they are
repo loading and provider construction, not the VDB — yet they moved by the
same -1.2%/-2.7% as the solve targets. That uniformity across touched and
untouched targets is the noise signature, so this table should be read as
**flat, not as a 2% win**. `rust`'s +1.4% comes from a visibly noisy sample
(current CI [8.0212, 8.3709] ms against a baseline CI of [8.0504, 8.0679] ms).

### 4. Harness check: the same commit, re-measured five days later

`12c8a98` is the previous anchor's "current" commit
(`results/20260811-repoloading-redesign-anchor-2b79d47/`), still an ancestor
of HEAD, so it can be rebuilt and re-run. Doing that gives a direct read on
how much of any delta above is the machine rather than the code
(`resolve-recheck-12c8a98.txt`).

| target | `12c8a98` on 08-11 | `12c8a98` on 08-16 | drift |
|---|---|---|---|
| `load_repo` | 1.2746 s | 1.2934 s | +1.5% |
| `build_provider` | 553.91 ms | 547.15 ms | -1.2% |
| `firefox` | 12.763 ms | 11.943 ms | **-6.4%** |
| `gcc` | 4.5747 ms | 4.4139 ms | -3.5% |
| `rust` | 8.1426 ms | 7.8987 ms | -3.0% |
| `openssh` | 4.2627 ms | 4.1358 ms | -3.0% |
| `python` | 5.9242 ms | 5.7081 ms | -3.6% |

**The same binary, on the same machine, moves -6.4% to +1.5% across five
days — a wider spread than the -1.2% to -2.9% measured in section 3 between
two different commits on the same day.** So section 3's table is not a small
win being reported as noise out of caution; it is genuinely below this
host's reproducibility floor for `cargo bench --bench resolve`.

Same-day cross-check, three commits spanning 35+ commits of history:

| target | `12c8a98` | `c40f9c5` | `17d2a81` (HEAD) |
|---|---|---|---|
| `load_repo` | 1.2934 s | 1.2957 s | 1.2805 s |
| `build_provider` | 547.15 ms | 554.36 ms | 539.42 ms |
| `firefox` | 11.943 ms | 12.237 ms | 12.000 ms |
| `gcc` | 4.4139 ms | 4.5232 ms | 4.3911 ms |
| `rust` | 7.8987 ms | 8.0595 ms | 8.1749 ms |
| `openssh` | 4.1358 ms | 4.2368 ms | 4.1503 ms |
| `python` | 5.7081 ms | 5.8682 ms | 5.7483 ms |

All three sit within ~2% of each other, with no ordering by commit date.
The resolve path has been flat across this whole stretch of work.

**This closes the question the previous anchor left open** — it noted that
"a third interleaved run would be needed to fully rule out a small genuine
effect under ~2%" behind its own uniform +1.5-4.3%. The answer is that
effects under ~2% are not resolvable by this bench on this host, so that
anchor's reading of its numbers as noise was right, and future runs should
not claim wins in that range from `cargo bench --bench resolve` alone.

**Noise floor for future runs: treat criterion `resolve` deltas under ~5%
as unresolved** (section 5 rules out the compiler and the dependency set as
the cause). Wall-clock hyperfine is far tighter when interleaved —
section 1's 7.90x carries a +/-0.60 error bar on a 20-run pair — so effects
too small for criterion here may still be measurable that way.

### 5. Ruling out the toolchain and the dependency set

Section 4's drift invites an obvious objection: the old commit was *rebuilt*
today, so the drift could be a compiler or dependency change rather than
environment. Both were checked, and both are ruled out.

**Compiler — ruled out by a natural experiment.** `rust-1.97.1` became the
active toolchain on 2026-08-10 20:30 (`/usr/bin/rustc` symlink mtime; the
rustup default `gentoo` toolchain is a symlink to `/usr`, so it follows
eselect). The two prior anchors straddle that date: the 2026-08-11 anchor was
measured under 1.97.1, the 2026-08-07 one under 1.95.0. Rebuilding *both* today
under 1.97.1 therefore isolates the compiler:

| target | `42e4042` 1.95.0 -> 1.97.1 | `12c8a98` 1.97.1 -> 1.97.1 |
|---|---|---|
| | *(compiler changed)* | *(compiler same)* |
| `load_repo` | +1.6% | +1.5% |
| `build_provider` | +4.7% | -1.2% |
| `firefox` | +2.1% | -6.4% |
| `gcc` | +2.4% | -3.5% |
| `rust` | -1.4% | -3.0% |
| `openssh` | -0.2% | -0.2%* |
| `python` | -0.1% | -3.6% |

\* `openssh` reads -3.0% in section 4's table; the -0.2% column here is the
`42e4042` pair.

A compiler effect would be systematic and would show up **larger** on the pair
that crossed the upgrade. Instead that pair drifts *less* (+4.7%..-1.4%) than
the same-compiler pair (+1.5%..-6.4%), and in the opposite direction — the
same-compiler commit measured *faster* today, the cross-compiler one *slower*.
Two commits re-measured on one day drifting opposite ways against their own
historical recordings is variance, not a toolchain trend.

**Dependencies — ruled out by the registry cache.** `Cargo.lock` is
gitignored (`.gitignore:2`), so a fresh worktree resolves dependencies anew
rather than reproducing the old resolution; this is a genuine gap in the
two-tree method and worth knowing. It did not bite here: of the 798 packages
in the current lock, **zero** were downloaded to `~/.cargo/registry/cache`
since 2026-08-10, so no newer version existed for cargo to resolve to in the
window. Git dependencies (`brush`, `pkgcraft`, `fakeroost`, `hakoniwa`,
`pseudoroot`) are pinned by `rev` and cannot drift at all.

**What did change:** the host rebooted 2026-08-15 06:59 (between the 08-11
anchor and this run), and load average differed across this session's own
measurements — ~1.1 during sections 1-4, ~4.1 during the `42e4042` re-run,
which may itself be part of why that one measured slightly slow. That is the
character of the variance: page-cache and NUMA placement after a reboot,
scheduling, and whatever else shares a 128-core box.

**Methodological note for future runs:** copy one `Cargo.lock` into both trees
before an A/B, rather than letting each resolve independently. It cost nothing
this time, but it is uncontrolled by default.

### 6. The interning commits on their own

Sections 1-3 span eleven commits, which is too coarse to say anything about
the interning specifically. Isolating it: `91196a9` (before) against
`17d2a81` (after) — exactly `dbc31b9` + `17d2a81`, the two interning
commits, nothing else. Both trees built from the **same `Cargo.lock`**
(copied in, per the gap noted in section 5; the only resulting difference is
pkgcraft resolving as git-vs-path, a benchmarks-only dep not linked into
`em`).

Interleaved hyperfine, `em -p @world`, 20 runs:

| | pre-interning `91196a9` | interned `17d2a81` |
|---|---|---|
| wall | 1.699 s ± 0.356 | 1.668 s ± 0.121 |
| user | 2.857 s | 2.865 s |

1.02 ± 0.23x — the error bar swallows it, and the load average was 4.5-7.0
during this run. Wall clock is the wrong instrument here anyway: these runs
spend 10-13 s in **system** time against ~3 s of user, so an allocation
saving cannot surface in it.

User CPU measured directly instead, 12 interleaved pairs
(`usertime.txt`):

| | mean user CPU |
|---|---|
| pre-interning `91196a9` | 3.052 s |
| interned `17d2a81` | 3.035 s |
| delta | **-0.55%** |

Per-pair scatter was ±0.3 s (±10%), so -0.55% is not a result.

**Why no benchmark on this host could resolve it.** A full VDB load of 723
packages allocates, on the old code, one `String` per USE token (3257), per
IUSE token (5161), and two per package for `slot_main` (1446) — about 9,900
short-lived allocations:

| alloc+free cost | total | share of the 3050 ms user CPU |
|---|---|---|
| 20 ns | 0.20 ms | 0.0065% |
| 40 ns | 0.39 ms | 0.0129% |
| 100 ns | 0.99 ms | 0.0323% |

Against ±10% measurement scatter that is roughly **three orders of magnitude
below the noise**. Even a microbenchmark of just the VDB load (~tens of ms of
the total) would put it under 1%, still beneath section 4's ~5% criterion
floor. The interning is therefore **unmeasurable by construction on this
host, not merely unmeasured** — it is a correctness-of-representation change
that removes ten resolve-then-intern round trips, and it should never have
been expected to show up in a wall-clock number.

## Conclusion

The work did what it targeted and nothing more: `em --info -v` went from
1.877 s / up to 659 MB to 237 ms / 32.8 MB, with identical output, while every
solver-path measurement — four CLI targets and seven criterion targets — sits
inside noise. That is the expected shape: the changes are in VDB reading and
set resolution, and `em -p` spends its time in repo metadata and the solver.

The interning work (`slot_main`, `use_flags`, `iuse`, `keywords`,
`repository`) is recorded here as **not measurably faster on this host**, and
section 6 isolates it and shows why: its effect is ~0.2-1.0 ms of avoided
allocation against 3050 ms of user CPU, three orders of magnitude below the
measurement scatter. It stands on the removed round trips, not on a number. It removed ten resolve-then-intern round trips, six of them
pre-existing, and its justification stays the removed redundancy rather than a
number.

New anchor for future comparisons: `17d2a81`.
