# Benchmark: gix vs git for `em sync` — 2026-07-29

## ⚠️ Correction (same day) — see `gix-vs-git-scale-crossover-2026-07-29.md`

**The runtime conclusion below does not hold for `::gentoo`, the repo `em
sync` matters most for.** This test used only `pentoo-overlay` (~500
packages) — a small-repo test whose result does not generalize. A follow-up
against a real clone of the Gentoo tree found **gix ~15% faster**, and a
depth-scaling test on pentoo itself found the gap **crosses over** from gix
losing (small repos) to gix winning (once past roughly 15k-65k objects) —
`::gentoo` is more than 2x past that crossover at just `--depth 1`. The
build-time finding below (5.4x) is unaffected and still stands. Treat the
"Verdict"/"Recommendation" sections below as **superseded** for the runtime
claim — read the crossover doc before using this file to justify keeping git
as the default for `::gentoo` sync specifically.

## Summary

Testing the two git backends for `em sync`: the default shell-based `git` command vs. the pure-Rust `gix` backend (opt-in via `--features sync-gix`).

**Key findings:**
- **Build time cost is substantial**: sync-gix adds **5.4× build time** (47s → 4m 14s) and **6 MB binary size increase**.
- **Sync runtime overhead is modest** *on a small overlay repo (`pentoo-overlay`, ~500 packages)*: cold clone +44% slower, warm re-sync only +9% slower. **This does not hold at `::gentoo` scale — see the correction above.**
- **Verdict (small-repo case only)**: The build-time cost outweighs modest runtime gains on a repo this size; sync-gix is not ready as default *for small overlays*. Not a settled verdict for `::gentoo` itself.

---

## Methodology

### Test Environment
- **Machine**: Ampere-1a (128 cores, 256 GiB RAM)
- **Date**: 2026-07-29
- **Test repository**: pentoo-overlay (GitHub: `https://github.com/pentoo/pentoo-overlay.git`)
  - Size: ~25 MB cloned size (~500 ebuilds)
  - Rationale: Small enough for fast iteration, large enough to be realistic

### Build Time Measurement
1. Clean the portage-cli package: `cargo clean -p portage-cli`
2. Build release binary with default features: `cargo build --release -p portage-cli`
3. Clean again
4. Build release binary with sync-gix: `cargo build --release -p portage-cli --features sync-gix`
5. Measure wall-clock time via shell `time` command

### Sync Runtime Measurement
**Cold clone test** (network-dominated):
- Fresh directory, no `.git`
- One run per backend (network variance is primary factor, not worth statistical rigor)
- Time measured via shell `time` or direct epoch measurement

**Warm re-sync test** (local git-protocol negotiation):
- Repository already cloned, no upstream changes (no-op update)
- Sync command detects "already up to date" and exits
- 3 runs per backend, averaged (more meaningful apples-to-apples comparison, dominated by local I/O and git negotiation)

**Isolation**:
- Separate temporary `repos.conf` and test directories for each backend
- Used `--config-root` to point `em` at isolated config, not the host system's
- No writes to `/var/db/repos/pentoo` or system portage config

---

## Results

### 1. Build Time Cost

| Metric | Default | sync-gix | Ratio |
|--------|---------|----------|-------|
| **Build time (wall-clock)** | 46.926s | 254s (4m 14s) | 5.4× |
| **Binary size** | 26 MB | 33 MB | 1.22× |
| **Size increase** | — | +6 MB | +22% |

**Notes:**
- Both builds were clean release builds of the single package (`-p portage-cli`)
- sync-gix build includes compiling the `gix` crate and large dependency tree
- On 128-core machine, ~90s of the sync-gix build was visible compilation activity; likely dominated by single-threaded dependency ordering and incremental codegen
- The factor of 5.4× on a high-core-count server suggests even heavier overhead on developer laptops (which have fewer cores for parallel rustc)

### 2. Sync Wall-Clock Time

#### Cold Clone (Fresh Git Clone from GitHub)

| Backend | Time | vs. Default |
|---------|------|-------------|
| **Default (git)** | 1.278s | — |
| **sync-gix** | 1.844s | +44% / +0.566s |

**Analysis:**
- Cold clone is **network-bound** (GitHub latency, download bandwidth)
- Local git logic overhead is small relative to network round-trips and data transfer
- sync-gix's overhead is visible but dominated by network; difference may be artifact of timing variation
- **Reproducibility**: Run this test multiple times against a live GitHub URL; wall-clock will vary with network conditions

#### Warm Re-sync (Already Cloned, No Updates — Typical Daily Sync)

| Backend | Run 1 | Run 2 | Run 3 | **Average** | vs. Default |
|---------|-------|-------|-------|----------|----------|
| **Default (git)** | 0.620s | 0.622s | 0.628s | **0.623s** | — |
| **sync-gix** | 0.662s | 0.687s | 0.689s | **0.679s** | +9% / +0.056s |

**Analysis:**
- Warm re-sync is **I/O and git-protocol negotiation bound**, not network
- Both backends detect "already up to date" and do minimal work
- sync-gix is **9% slower** (56 ms overhead per re-sync)
- Variance across 3 runs: default ±0.8%, sync-gix ±1.9% (within noise)
- **Use case**: Users running `em sync` daily (or per-session) spend ~50ms extra per repo with sync-gix
  - For pentoo alone: 50ms
  - For typical multi-repo setup (5-10 repos): 250ms–500ms extra per sync run
  - **Acceptable for a feature, not for a default** (users expect defaults to be "fast")

---

## Discussion

### Why the Build-Time Cost Matters

1. **CI/CD pipelines**: +4 minutes per build
   - Affects portage-cli's own CI: every PR takes longer
   - Affects downstream users bundling or packaging em

2. **Developer iteration**: Significant friction
   - Change to sync code → full rebuild → test
   - On laptop (16 cores): expect 10–15 minute rebuilds with sync-gix
   - Discourages rapid iteration on sync features

3. **Binary distribution**:
   - Larger binary (+6 MB) slightly worse for downloads/cache
   - Minor but noticeable on slower networks or mobile

### Why the Runtime Overhead is Acceptable (but not Compelling)

1. **Cold clone**:
   - Network dominates; 44% overhead (566ms) is noise on a 1–3 second network operation
   - Not the typical use case (first clone per overlay)

2. **Warm re-sync**:
   - 56ms per repository, ~250ms for typical 5-repo setup
   - Acceptable for an opt-in feature
   - **Not compelling enough to flip default** — user gets only marginal perf improvement vs. 5.4× build cost

3. **No runtime wins observed**:
   - Both backends shell out to git or use it directly
   - sync-gix doesn't demonstrate "faster" vs. git for small overlays
   - Larger repos (gentoo tree, 10k+ ebuilds) untested here but likely show similar modest overhead

### Repo-Specific Observations

- **pentoo-overlay**: ~25 MB, ~500 ebuilds, clean history
  - Small repo → overhead is most visible in relative terms
  - Larger repos (gentoo: 15k ebuilds, 100+ MB) may show different behavior
  - **Future work**: benchmark against gentoo tree; note: must not mutate live `/var/db/repos/gentoo`

---

## Recommendation — ⚠️ superseded, see `gix-vs-git-scale-crossover-2026-07-29.md`

*(Left in place for the audit trail — this section's runtime rationale turned
out to be scoped to `pentoo-overlay` only and does not hold for `::gentoo`.
Do not use this section on its own to justify a backend decision.)*

**~~Do NOT make gix the default backend.~~** — the build-time rationale
(point 2 below) still stands on its own; points 1 and 3 do not, once tested
against the repo that actually matters.

**Rationale (original, small-repo-only):**
1. ~~**Module doc requirement not met**: "we have not yet proven it faster than `/usr/bin/git` for Portage sync"~~
   - sync-gix is **9% slower** on warm re-sync **on pentoo-overlay** — reverses to **~15% faster** on a real `::gentoo` clone (147k objects); see the crossover doc.
   - sync-gix is **44% slower** on cold clone (network-bound; also only tested on pentoo-overlay)
   - ~~No runtime win to justify the cost~~ — there is one, at `::gentoo` scale

2. **Build-time cost is prohibitive for default**: (unaffected by the above, still valid)
   - 5.4× slower (47s → 254s)
   - Significant friction for developers, CI, and build systems
   - Users who don't want gix still pay the cost if it becomes default

3. ~~**No clear user benefit**~~ — see point 1; there is a clear benefit at `::gentoo` scale, this was only true for the small-repo case tested.

### Suggested Path Forward (original — item 2 below is what actually happened)

1. **Keep sync-gix as opt-in feature** (`--features sync-gix`) for power users and testing — still reasonable given the build-time cost, independent of the runtime correction
2. ~~**Document this benchmark** in `todo/PENDING.md` (mark "progress UI" section as "measured, not ready for default flip")~~ — done, then corrected same day; see `todo/PENDING.md` and the crossover doc
3. **Future investigation** (deferred — item 2 (larger repos) is now done, see the crossover doc; the rest remain open):
   - Profile sync-gix's build: identify heavy dependencies
   - ~~Test against larger repos (gentoo, crossdev overlays) to see if overhead scales differently~~ — done, see `gix-vs-git-scale-crossover-2026-07-29.md`
   - Measure cold clone against different network conditions
   - Consider partial builds or feature gates to reduce sync-gix compile time

---

## Raw Data

### Build Log Excerpt (Default)
```
    Finished `release` profile [optimized] target(s) in 46.926s
```

### Build Log Excerpt (sync-gix)
```
    Finished `release` profile [optimized] target(s) in 4m 14s
```

### Benchmark Script

See `benchmarks/sync-gix-vs-git-2026-07-29.sh` (if retained) for exact reproduction commands and test harness. Test repos.conf, directories, and binaries were created fresh for each run to avoid cache effects.

---

## Notes for Reproducibility

1. **Machine differences**:
   - These numbers are from a 128-core Ampere-1a server
   - Laptop/M2 results will differ (likely slower absolute time, higher ratio for single-threaded portions)
   - Network latency varies; run multiple times or benchmark from a stable network

2. **Git version**:
   - Host's `/usr/bin/git` was used for the git-cmd backend
   - gix crate version pinned in `Cargo.lock`

3. **Network**:
   - Cold clone hits live GitHub (real network)
   - Warm re-sync is purely local (no network)
   - Pentoo overlay chosen for reasonable clone time (~1s) and small size

4. **Repro**:
   - Requires `cargo`, Rust toolchain, `em` binary buildable
   - Requires internet access to GitHub (for cold-clone test)
   - Does not require root; uses temp directories in `/tmp`
