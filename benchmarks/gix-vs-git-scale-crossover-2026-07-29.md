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

## Follow-up: warm re-sync and real network, both at `::gentoo` scale (same day)

Both items above are now closed, with a further wrinkle each.

### Warm re-sync (the actual day-to-day `em sync` case)

Set up a "stale" client 5 commits behind tip (a realistic multi-day drift) on
the real 147k-object `::gentoo` mirror, `file://` (local), and timed the
actual incremental update:

| Step | git | gix |
|------|-----|-----|
| fetch (`--depth 1`, negotiate + transfer) | 0.268s | **0.174s (~35% faster)** |
| worktree update (`reset --hard`) | 0.531s | not measured — no `reset`/`checkout` subcommand in the `gix` CLI |
| **total** | ~0.72-0.8s | incomplete |

Even on a *small* increment (5 commits), gix's fetch step is faster — this
reverses the earlier prediction that gix would lose here the way it lost on
the pentoo depth-1/10 tests. But git's own `reset --hard` step is the bigger
cost (0.53s > the 0.27s fetch) and it scales with the size of the *whole*
checked-out tree (147k files), not the size of the diff (a handful of
changed files) — a naive full-tree reset is expensive regardless of how
small the actual change was. Whether gix's equivalent (`gix_ext::hard_reset_to`
in this codebase, which similarly rebuilds the whole index from the target
tree and does a full checkout pass) has the same scaling problem is not
directly measured here — worth instrumenting `em`'s own sync path directly
rather than the bare `gix` CLI, which doesn't expose this step standalone.

### Real network, at `::gentoo` scale

Repeated the original network test (`git clone --depth 1` vs `gix clone
--depth 1`, real HTTPS against `github.com/gentoo-mirror/gentoo`), 2 runs
each (scaled down from 3+3 given the ~74MB-per-run bandwidth cost against a
public mirror):

| | Run 1 | Run 2 | Avg |
|---|---|---|---|
| `git` | 19.19s | 19.63s | 19.41s |
| `gix` | 20.14s | 22.13s | 21.14s |

**Gix is ~9% slower over real network at this scale** — the opposite of the
local `file://` result (gix +15% faster) at the same repo size. Real network
latency/bandwidth reintroduces enough transport-layer overhead (the same
effect found dominating the original small-repo network test) to outweigh
gix's local-processing advantage, even once that advantage is large enough
to win decisively with the network removed. Sample size here is smaller (2
runs, not reordered) than the rest of this investigation — a real signal
given its size and direction, but less rigorously confirmed than the other
findings in this doc.

### Revised picture

The `::gentoo`-scale story is not simply "gix wins for large repos" — it
depends on which part of the operation dominates:
- **Cold clone over the real network**: git wins (transport-layer cost
  reasserts itself at scale).
- **Cold clone locally / already-fetched data**: gix wins (its bulk
  processing throughput is genuinely better once fixed overhead stops
  dominating).
- **Warm re-sync's fetch step**: gix wins, even for a small increment.
- **Warm re-sync's worktree-update step**: dominated by total tree size for
  git; gix's equivalent cost is still unmeasured.

Given real `em sync` usage is overwhelmingly warm re-syncs over a real
network connection (not local, not cold), the two most representative
numbers — network cold-clone (git wins) and reset/checkout cost (unmeasured
for gix) — are the ones actually needed to settle the recommendation, and
one of them isn't answered yet. Not a basis for a default flip either way.

## Remaining gaps

- gix's worktree-update (reset/checkout) cost for a warm re-sync at
  `::gentoo` scale — needs direct instrumentation of `em`'s own
  `gix_ext::hard_reset_to`, not the bare CLI.
- Warm re-sync has not been tested over the real network (only fetch+reset
  locally, and cold-clone over network — not the combination that most
  resembles real daily usage).
- Test used a shallow, handful-of-commits-deep mirror of the real tree, not
  the full multi-year history `::gentoo` actually has on disk once synced
  normally.
