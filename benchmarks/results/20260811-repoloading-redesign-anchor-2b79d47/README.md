# Repo/overlay abstraction redesign: fresh anchor + quick-vs-release, 2026-08-11

- **current:** `2b79d47` (HEAD after the full RepoSet redesign — see
  [[repo-overlay-abstraction-redesign]] / `todo/repo-overlay-abstraction-redesign.md`)
- **why:** after landing the redesign and its post-landing perf follow-ups
  (`1a01a0f`, `684ad7a`, `7a354eb`), record a fresh criterion anchor point
  and confirm quick vs release show no meaningful difference for the
  actual CLI paths touched by the redesign.

## 1. Criterion `resolve` bench — controlled two-tree A/B, `4ea725c` vs `b1a8bcc`

**Correction:** an earlier version of this section relied on criterion's
own internal delta, which compares against whatever was last saved locally
under `target/criterion/` (a ~2.5 week-old run from 2026-07-25) — not a
deliberate A/B of this session's changes, and part of that run's output was
also lost to a `tail -80` truncation mistake. Redone properly per the
established methodology (see
[[benchmark-baseline-worktree]] and
`results/20260807-145520-softorder-732d604-vs-bf35c79/README.md`): a
baseline worktree (`/home/lu_zero/Sources/portage-cli-bench-baseline`,
sibling of `pkgcraft`/`brush`) checked out at `4ea725c` (pre-session,
origin/master), `cargo bench --bench resolve` run to completion there with
full output captured, then the same bench run separately in the main tree
at `b1a8bcc` (HEAD, post-redesign + all perf follow-ups), with no build or
other bench running concurrently in either case (`ps aux` checked before
trusting each result). Full raw output:
`resolve-bench-baseline-4ea725c.txt` / `resolve-bench-current-b1a8bcc.txt`.
Deltas below are computed directly from the two files' absolute point
estimates (`(current - baseline) / baseline`), not from criterion's own
in-run comparison (which in the current-tree run reflects vs an
irrelevant same-day prior local run, not the baseline worktree).

**Caveat:** `resolve_load`'s `load_repo`/`build_provider` targets use
`Repository::builder().in_memory_cache().open()` plus raw
`category()`/`package()`/`cache_entry()` calls directly — this bench does
**not** exercise `RepoSet`, `repo_entries()`, or `EbuildsAcross` at all.
It's the project's established "resolve" anchor, not a direct test of
this session's changes. Section 2 below is the one that actually
exercises the redesigned code path.

| target | baseline (`4ea725c`) | current (`b1a8bcc`) | delta |
|---|---|---|---|
| `load_repo` | 1.2479 s | 1.2746 s | +2.14% |
| `build_provider` | 531.32 ms | 553.91 ms | +4.25% |
| `firefox` | 12.448 ms | 12.763 ms | +2.53% |
| `gcc` | 4.4999 ms | 4.5747 ms | +1.66% |
| `rust` | 8.0064 ms | 8.1426 ms | +1.70% |
| `openssh` | 4.1633 ms | 4.2627 ms | +2.39% |
| `python` | 5.8361 ms | 5.9242 ms | +1.51% |

**Reading this:** every target regressed by a similar ~1.5-4.3%,
*including* `load_repo`/`build_provider`, which (per the caveat above)
don't touch any of the redesigned code at all. A uniform slowdown across
targets that do and don't exercise the changed code is the signature of
run-to-run environment noise (thermal/scheduling/disk-cache state between
the two sequential `cargo bench` invocations), not a real regression from
the RepoSet redesign — a genuine regression in `repo_entries()`/`RepoSet`
would show up disproportionately in the targets that actually call it, and
leave `load_repo`/`build_provider` flat. Treat this as "no evidence of a
resolve-path regression," not as a clean zero-delta result; a third
interleaved run would be needed to fully rule out a small genuine effect
under ~2%.

## 2. CLI wall-clock, quick vs release — the path that actually changed

Same-run interleaved `hyperfine` (`--warmup 2 --runs 8 -i`), real host
repo (`gentoo` + `guru` + `crossdev` + `exp-llvm-libc` configured), both
binaries built from the identical `2b79d47` tree, no concurrent cargo
builds (confirmed via `ps aux` before each run — see
[[never-benchmark-during-a-background-build]]).

| target | quick | release | delta |
|---|---|---|---|
| `www-client/firefox` | 1.059 s ± 0.039 s | 1.042 s ± 0.037 s | release 1.02x faster |
| `sys-devel/gcc` | 879.8 ms ± 22.5 ms | 889.1 ms ± 19.9 ms | quick 1.01x faster |
| `net-misc/openssh` | 878.2 ms ± 35.8 ms | 898.0 ms ± 47.8 ms | quick 1.02x faster |

All three within run-to-run noise (±2-5%), no consistent direction —
matches the [[repo-overlay-abstraction-redesign]] finding for
`which`/`depends`/`-p @world` earlier the same day. `quick` (thin LTO)
remains safe to use for iteration and even ad-hoc benchmarking in this
area; `lto = true` vs `lto = "thin"` doesn't move the needle for this
codebase's I/O- and parse-dominated workload.

## Conclusion

No evidence of a resolve-path regression from either method — the
criterion two-tree A/B shows a uniform small slowdown across *all*
targets (including two that don't touch the redesigned code at all),
consistent with environment noise rather than a code-level effect. The
redesign's actual perf story is the one already established via targeted
hyperfine work earlier the same day: `which` unchanged, `depends` ~2x
faster, `-p @world` unchanged/slightly faster.
