# portage-bench

Benchmarks for the Gentoo Portage Rust ecosystem.

## What's Benchmarked

### Criterion (microbenchmarks)

| Bench file | What it measures |
|------------|-----------------|
| `dep_parsing` | Atom/dependency parsing (portage-atom, portage-metadata) |
| `dep_parsing_pkgcraft` | Same, vs pkgcraft — needs `--features pkgcraft-compare` |
| `realworld_dep_parsing` | Real ebuild RDEPEND strings, portage-atom vs pkgcraft — needs `--features pkgcraft-compare` |
| `resolve` | PubGrub dependency resolution on full repo |
| `dedup` | Deduplication of parsed dep/license/required-use trees |
| `trim_scaling` | BDEPEND/DEPEND trim scaling over synthetic plans and VDBs |
| `graph_scaling` | Dependency graph and install-order scaling |
| `vdb_snapshot` | Shared versus independent BROOT/prefix VDB enumeration |
| `policy_facts` | Uncached, cold/warm cached, and multi-generation policy facts |
| `policy_lists` | Full acceptance filtering with CPN-indexed policy lists |
| `repo_load` | Real-tree repository discovery, cache parsing, and freshness validation |
| `cache_workers` | Real-tree cache read/decode across worker counts |
| `repo_cache_lookup` | Exact CPV cache construction and lookup scaling |
| `provider_postprocess` | Provider construction with complete versus missing dependency branches |
| `source_reuse` | Worker-local shell reuse during synthetic ebuild sourcing |
| `eclass_memo` | Eclass digest/path memo during fresh-repository live sourcing |
| `regen_reads` | Ebuild content reuse during synthetic cache regeneration |
| `metadata_serialize` | Direct-buffer metadata cache serialization |
| `gap_index` | Synthetic gap-index fast path, and the per-resolve cost of a repo with no sync marker |

The `portage-vdb` crate also has the per-crate `ownership` benchmark, which compares repeated full `CONTENTS` scans with a reusable collision index on synthetic package sets.

`pkgcraft-compare` is off by default (it pulls in `gix`, a sizeable compile) —
enable it explicitly whenever you actually want the pkgcraft comparison
numbers, e.g. `cargo bench --features pkgcraft-compare`.

### Wall-clock (CLI)

| Tool | What it measures |
|------|-----------------|
| `em regen` | Full metadata-cache regeneration |
| `em search` | Package search by name/description |
| `pk repo metadata regen` | pkgcraft baseline (regen) |
| `emerge -s` / `qsearch` | Portage/portage-utils baselines (Gentoo only) |

## Variables

| Dimension | Values |
|-----------|--------|
| Interner backend | papaya (default), lasso, symbol-table |
| Allocator | system (default), mimalloc |

## Setup

```sh
git clone https://github.com/lu-zero/portage-bench
cd portage-bench

# Clone sibling crates
../portage-bench/scripts/maint.sh setup

# Shallow Gentoo tree for regen/search benchmarks (~200 MB)
git clone --depth 1 https://github.com/gentoo/gentoo.git gentoo

# pkgcraft baseline (optional, for comparison)
git clone https://github.com/pkgcraft/pkgcraft ../pkgcraft
```

## Running

```sh
# Full sweep: 6 configs × (criterion + regen + search)
./scripts/bench-sweep.sh

# Single config, quick
./scripts/bench-sweep.sh --configs papaya-mimalloc

# Criterion only (skips the pkgcraft-comparison benches, see above)
cargo bench

# Include the pkgcraft comparisons
cargo bench --features pkgcraft-compare

# Specific bench
cargo bench --bench resolve
cargo bench --bench policy_facts

# With alternative interner
cargo bench --no-default-features --features lasso

# CLI comparison (standalone, needs pre-built em)
./scripts/compare-regen.sh
./scripts/compare-search.sh
```

## Evaluation

```sh
# Auto-evaluates latest sweep
./scripts/bench-eval.sh

# Specific run
./scripts/bench-eval.sh bench-results/20250523-120000/summary.tsv

# Write to file
./scripts/bench-eval.sh -o report.md
```

## Libraries Under Test

- [portage-atom](https://crates.io/crates/portage-atom) — PMS atom and dependency parser
- [portage-metadata](https://crates.io/crates/portage-metadata) — ebuild metadata cache types
- [portage-repo](https://github.com/lu-zero/portage-repo) — ebuild repository layout reader
- [portage-atom-pubgrub](https://github.com/lu-zero/portage-atom-pubgrub) — PubGrub solver bridge
- [pkgcraft](https://github.com/pkgcraft/pkgcraft) — baseline comparison library

## Data & Blogpost Material

See [`BENCHMARKS.md`](./BENCHMARKS.md) for a consolidated collection of all tables, raw data from historical runs, descriptions of every benchmark, and up-to-date reproduction instructions tailored to this workspace.

**Per-machine info**: Hardware details, NUMA, characterization commands, and notes live in `machines/` (one `.md` per machine, e.g. `machines/thalia.md`, `machines/mneme.md`). Always link the relevant machine file when publishing results for reproducibility.

## License

[MIT](../LICENSE-MIT)
