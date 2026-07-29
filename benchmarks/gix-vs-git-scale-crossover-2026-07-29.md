# gix vs git: performance crosses over with repo size — 2026-07-29

## Correction to `sync-gix-vs-git-2026-07-29.md`

That benchmark tested only `pentoo-overlay` (~500 packages, the smallest
practical `em sync` target) and concluded gix has no runtime advantage over
git, only a build-time cost. **That conclusion does not hold for the repo
`em sync` actually matters most for: `::gentoo` itself.** Testing against a
real clone of the Gentoo tree reverses the runtime picture entirely — gix is
faster there, not slower. The build-time finding (gix costs 5.4x more to
compile) stands unchanged; the runtime recommendation does not.

This file exists because the original test picked its repo for convenience
(small, fast to iterate) and that choice silently became the basis for a
recommendation about the wrong case. `pentoo-overlay` was never the repo the
sync-backend decision should have hinged on.

## Methodology

All tests: local `file://` clones (network removed as a variable — see below
for why), 3 runs each direction, both orderings run to rule out
filesystem-cache bias. `gix` = gitoxide CLI, library version ~0.83-0.86
(matching the workspace's pinned `gix` dependency). Test machine: Ampere-1a
(aarch64, 128 cores).

### Why `file://`, not the network test

The original benchmark's `+44%`/`+9%` runtime numbers were measured over a
real HTTPS clone from GitHub. Removing the network (`file://` against a local
bare mirror) cut the gap from `+35-45%` down to `+23%` on the same small
repo — most of that original gap was network/transport-layer (TLS handshake,
HTTP connection setup), not compute. Ruled out separately, and not the
subject of this file: ARM SIMD/hardware-acceleration gaps (checked
`zlib-rs`/`crc32fast`/RustCrypto `sha1` source directly; only a minor,
sub-10%, not-the-explanation gap found in an unused ARM SHA1 hardware path),
and thread-count starvation (`--threads 1/4/128` measured with no difference,
confirmed via `git verify-pack` that the test pack had 3669+ independent root
objects — plenty of parallelism available regardless).

## Results: the crossover

`pentoo-overlay`, `file://`, increasing `--depth` (more history = more objects):

| Depth | Objects | Pack size | `git` avg | `gix` avg | Result |
|-------|---------|-----------|-----------|-----------|--------|
| 1 | 4,509 | 3.6MB | 0.383s | 0.471s | gix **+23% slower** |
| 10 | 4,638 | 3.7MB | 0.396s | 0.483s | gix **+22% slower** |
| 500 | 13,415 | 6.5MB | 0.605s | 0.638s | gix **+5.5% slower** |
| 5000 | 64,717 | 23MB | 1.723s | 1.252s | gix **~27% faster** |

The real repo, fresh `git clone --bare --depth 5` of
`https://github.com/gentoo-mirror/gentoo.git` (78MB, 5 commits) as the local
`file://` source, then `--depth 1` from that:

| Repo | Objects | `git` avg (6 runs) | `gix` avg (6 runs) | Result |
|------|---------|---------------------|----------------------|--------|
| **real `::gentoo`, depth 1** | **147,147** | **8.323s** | **7.090s** | **gix ~15% faster** |

Both pentoo-depth-5000 and the real-gentoo result were verified with the
tools run in both orders (git-first and gix-first) to rule out
filesystem-cache warming bias — the result holds regardless of order.

## Why this happens

gix appears to have a real, roughly constant per-clone overhead (process/
library startup, negotiation) that dominates wall-clock for small repos —
this is what the pentoo depth-1/10 tests are actually measuring. As object
count grows, that fixed cost becomes a smaller fraction of the total, and
gix's actual bulk-processing throughput — which profiling did not find to be
SIMD- or thread-limited — turns out to be competitive with or better than
git's C implementation. The crossover for this specific test pack shape
lands somewhere between depth 500 (13k objects) and depth 5000 (65k
objects); the real `::gentoo` tree at just depth 1 is already more than 2x
past that upper bound (147k objects) and shows the same win.

## Revised recommendation

- The build-time cost (5.4x, independently confirmed, unaffected by any of
  this) is real and applies regardless of which repo `em sync` is pointed
  at. That part of the original recommendation is unchanged.
- The runtime recommendation needs to be repo-size-aware, not a single
  global default. For small overlays (the `pentoo-overlay`-shaped case), git
  is faster. For `::gentoo` itself — the repo where sync wall-clock time
  actually matters to a user waiting on it — gix wins.
- This is not yet a recommendation to flip the default. It **is** a
  correction to the earlier "not proven faster" framing, and a case for
  treating the backend choice as a real open design question (e.g.
  repo-size-aware backend selection, or re-evaluating whether the build-time
  cost is worth paying given the `::gentoo` win) rather than a settled "keep
  git" decision.

## What's not yet done

- This tested a cold shallow clone only, not the warmer/no-op re-sync case
  that dominates real day-to-day `em sync` usage once a repo is already
  cloned — that comparison (at real `::gentoo` scale) hasn't been run yet
  and could tell a different story again.
- Only `file://` (local disk) was tested at the real-`::gentoo` scale — the
  network-layer effect confirmed at small scale hasn't been re-checked
  against a large repo, where it might matter less (fixed TLS/negotiation
  cost against a much longer transfer) or more.
- Test used a shallow, 5-commit-deep mirror of the real tree, not the full
  multi-year history `::gentoo` actually has on disk once synced normally.
