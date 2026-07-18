# Benchmarks

How to measure performance across the workspace: per-crate microbenchmarks,
the central cross-crate harness, and ad hoc wall-clock comparisons against
real `emerge`.

For historical data, tables, and machine-specific reproduction notes, see
[`benchmarks/BENCHMARKS.md`](../benchmarks/BENCHMARKS.md) (the canonical,
continuously-updated record) and [`benchmarks/README.md`](../benchmarks/README.md).
This page is the quick-start map of *where* things live and *what to run*;
it intentionally doesn't duplicate historical numbers.

## Three layers

### 1. Per-crate criterion microbenchmarks

Each crate with a hot path worth isolating owns its own `benches/`:

| Crate | Bench | What it measures |
|---|---|---|
| `gentoo-interner` | `interner` | intern/resolve throughput across backends (papaya/lasso/symbol-table) |
| `portage-atom` | `parsing` | atom/dep-string parsing, vs pkgcraft |
| `portage-atom-resolvo` | `parsing` | same, resolvo-side types |
| `portage-vdb` | `vdb` | VDB open/iterate/category-scan against a real `/var/db/pkg` |
| `portage-cli` | `elfscan` | install-image ELF scan serial vs parallel (merge-time `NEEDED.ELF.2`) |

Run one directly with `cargo bench -p <crate> --bench <name>`, e.g.:

```sh
cargo bench -p gentoo-interner --bench interner
cargo bench -p portage-atom --bench parsing
cargo bench -p portage-vdb --bench vdb
cargo bench -p portage-cli --bench elfscan
# optional tree (default: /usr/lib64)
ELFSCAN_BENCH_DIR=/path/to/image cargo bench -p portage-cli --bench elfscan
```

Some support alternative interner/allocator features (see each crate's
`Cargo.toml`):

```sh
cargo bench -p gentoo-interner --bench interner --no-default-features --features symbol-table
```

No benches in `portage-repo`, `portage-metadata`, `gentoo-core`,
`gentoo-stages`, `portage-distfiles`, `portage-solver`, `portage-binpkg` —
they're exercised via the central harness below or via end-to-end CLI
comparisons instead.

### 2. Central harness (`benchmarks/`, workspace member `portage-bench`)

Cross-crate microbenchmarks that need a real repo (parsing at scale, full
dependency resolution) and the wall-clock comparison scripts against real
`emerge`/pkgcraft:

```sh
# All 4 criterion benches (dep_parsing, realworld_dep_parsing, resolve, dedup)
cargo bench -p portage-bench

# One
cargo bench -p portage-bench --bench resolve

# Alternative interner (see benchmarks/Cargo.toml's [features])
cargo bench -p portage-bench --no-default-features --features lasso
```

`resolve` and the wall-clock scripts need a real ebuild tree — see
`benchmarks/README.md`'s Setup section for the shallow Gentoo clone.

Full sweeps (interner × allocator configs) and evaluation into tables:

```sh
cd benchmarks
./scripts/bench-sweep.sh                     # all 6 configs
./scripts/bench-eval.sh -o report.md         # tables from the latest sweep
```

### 3. Wall-clock comparisons against real `emerge` (`benchmarks/bench-*.sh`)

These drive the actual `em` binary against real `emerge`/pkgcraft on real
targets — the most representative numbers, since they include I/O, VDB
scans, and the full resolve, not just an isolated function.

```sh
cargo build --release -p portage-cli

# Package-set parity + hyperfine timing vs real emerge
./benchmarks/bench-em-vs-emerge.sh                 # EM=target/release/em, RUNS=5
SKIP_TIMING=1 ./benchmarks/bench-em-vs-emerge.sh    # parity only, fast — use this for
                                                     # quick "did I break something" checks

# BDEPEND-trim-specific and crossdev-specific comparisons
./benchmarks/bench-bdepend-trim.sh
./benchmarks/bench-cross-emerge.sh

# Install-image ELF scan: em serial/parallel vs Portage scanelf (pax-utils)
./benchmarks/bench-elfscan.sh                  # default /usr/lib64
./benchmarks/bench-elfscan.sh /path/to/image
SKIP_SCANELF=1 ./benchmarks/bench-elfscan.sh   # em only
```

`SKIP_TIMING=1` is the day-to-day check after touching resolver/USE code: it
diffs `em -p`'s resolved package set against real `emerge -p` for a fixed
target list (qtbase, texlive-core, firefox, qtwebengine, thunderbird,
libreoffice, qemu, a crossdev target) and reports per-target diff counts —
seconds, not minutes, and catches package-set regressions before they reach
the slower timing runs.

## Before/after comparisons for a specific change

For "did my change actually help", the cheapest reliable method (no
dedicated bench needed) is two binaries + `hyperfine`:

```sh
cargo build --release -p portage-cli && cp target/release/em /tmp/em_after
git stash && cargo build --release -p portage-cli && cp target/release/em /tmp/em_before && git stash pop

hyperfine --ignore-failure --warmup 2 -m 10 \
  -n before '/tmp/em_before -p <heavy-target>' \
  -n after  '/tmp/em_after -p <heavy-target>'
```

Pick a target with a large, IUSE-rich dependency closure (`dev-qt/qtwebengine`,
`app-office/libreoffice`) — small plans (a handful of packages, sparse IUSE)
sit below hyperfine's noise floor on most changes to the resolver's USE/mask
handling, since the per-version cost these changes touch is proportional to
plan size × IUSE richness, not a fixed overhead. `--ignore-failure` is needed
because `em -p` exits 1 whenever the plan needs a config change (autounmask),
which is expected, not a benchmark failure.

For allocation-level attribution (not just wall time), build with the
off-by-default `dhat-heap` feature and load the resulting `dhat-heap.json` at
[dh_view.html](https://nnethercote.github.io/dh_view/dh_view.html) — see
`benchmarks/BENCHMARKS.md`'s 2026-07-11 update for a worked example of using
this to find an O(n²) hot path.

## When adding a new benchmark or reporting results

- New per-crate microbenchmark: add `benches/<name>.rs` + a `[[bench]]`
  entry in that crate's `Cargo.toml`, and list it in the table above.
- New historical data point: append to `benchmarks/BENCHMARKS.md` (dated
  section, like the existing 2026-07-11/2026-07-12 updates), not here —
  this page should stay a stable map, not a running log.
- Always record which machine (see `benchmarks/machines/`) — numbers from
  different hardware classes (laptop vs. server-class NUMA) aren't
  comparable.
