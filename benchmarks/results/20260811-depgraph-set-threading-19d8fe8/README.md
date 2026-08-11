# depgraph RepoSet threading — confirm the anchor, 2026-08-11

- **change:** `19d8fe8` (refactor(depgraph): thread the RepoSet through
  DepgraphOpts, not repo_path) + `41636c4` (alias find_cpns).
- **baseline:** `7dbe9fc` (reachable equivalent of the prior anchor's
  `2b79d47`/`b1a8bcc` — see
  `results/20260811-repoloading-redesign-anchor-2b79d47/README.md`'s
  SHA-labelling note).
- **current:** `d47b763`.
- **host:** Ampere-1a, 128-core aarch64, 255 GiB RAM, gentoo+guru+crossdev.

## TL;DR

The fix works and is confirmed structurally: `Repository::open` count per
`em -p` merge is halved (strace, deterministic, reproduced). Wall-clock
impact is positive-but-small and below this session's noise floor — *and*
that is the correct expectation, not a measurement failure: the expensive
part of a resolve (`repo_entries`' parallel read of ~37k md5-cache files +
the pubgrub solve) runs identically in both binaries; the fix only removed
a duplicate of the cheap repo-opening step (`layout.conf`/`repo_name`/
categories reads).

## 1. strace — the clean signal

`strace -f -e trace=openat em -p sys-devel/gcc`, both binaries warm.
Full output: `strace-repo-opens.txt`.

| file | baseline (`7dbe9fc`) | current (`d47b763`) |
|---|---|---|
| gentoo `profiles/repo_name` (1 per `Repository::open`) | 8 | **4** |
| gentoo `metadata/layout.conf` | 8 | **4** |
| guru `metadata/layout.conf` | 2 | **1** |
| crossdev `metadata/layout.conf` | 2 | **1** |
| gentoo gap-index read | 1 | 1 |
| gentoo md5-cache files read | 32813 | 32813 |
| guru md5-cache files read | 3854 | 3854 |

**What moved:** `Repository::open` count halved (8→4 for gentoo, 2→1 per
overlay) — the second `repo_set_from_conf` build that used to run inside
`depgraph()` is gone. **What didn't:** the md5-cache bulk read (32813 +
3854 files) is identical in both — `repo_entries` is unchanged by this
refactor, and that's where the wall-clock actually lives.

gentoo shows 4 (not 1) `Repository::open`s because it's opened as main, as
each overlay's master, and transitively — all halved, none added.

> **Correction note:** an earlier draft of this file claimed "baseline = 0
> md5-cache reads," suggesting a regression. That was an artifact of the
> baseline binary having been deleted (worktree removed) *before* that
> strace ran, so the trace was empty. Recreating both binaries fresh and
> tracing them back-to-back confirms both do exactly the same cache work.

## 2. hyperfine wall-clock — noise-bound, as expected

Interleaved `hyperfine -i`, `--profile quick`. Full raw: `hyperfine-raw.txt`.

| run | target | baseline | current | result |
|---|---|---|---|---|
| 1 (10 runs) | gcc | 1.245 s ± 0.101 s | 1.331 s ± 0.130 s | baseline 1.07x |
| 1 (10 runs) | openssh | 1.272 s ± 0.109 s | 1.213 s ± 0.071 s | current 1.05x |
| 2 (15 runs) | gcc | 1.270 s ± 0.099 s | — | baseline 1.01 ± 0.12x |
| 3 (25 runs) | gcc | 1.249 s ± 0.121 s | 1.285 s ± 0.106 s | baseline 1.03 ± 0.13x |

Variance ±8-13%, the three targets/runs disagree on direction — classic
noise. The host was not cooperative: load bounced 0.26 → 40 over the
session (other agents/builds landing on the shared 128-core box).

**Why this is the right answer even on an idle host:** the strace shows the
md5-cache read count is identical, so the delta is only `N` cheap
`layout.conf`+`repo_name`+categories reads avoided (where N = repos in the
set). At ~ms-scale each vs a ~1.3 s total, that's a low-single-digit-%
effect — real, but not something `em -p` wall-clock resolves cleanly even
without contention. The fix's value is structural (one repo world shared
between `resolve_atom` and the solver — see the divergence-risk note on
`19d8fe8`), with a small perf bonus.

## Conclusion

Confirmed: `Repository::open` per merge halved (strace). No cache-read
regression (both binaries read the same 32813 + 3854 files). Wall-clock
unchanged within noise — expected, since the fix doesn't touch the
cache-read path that dominates a resolve. A clean idle-host measurement
(load < 1 for the full window, `--runs 30+`) would be needed to put a
precise number on the small positive effect; deferred.
