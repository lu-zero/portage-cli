# Benchmarks

- **Date:** 2026-03-04
- **Machine:** macOS Darwin 23.6.0, 12 logical CPUs
- **Corpus:** `gentoo/` repo (32,138 ebuilds, 763 missing cache entries)

## Porting bash builtins to Rust — dev-libs slice

Repeatable target: `dev-libs/*` (1,236 ebuilds).

```
cargo run --release --example regen_cache -- gentoo 'dev-libs/*'
```

| Phase                    | real   | user   | sys   | maxrss |
|--------------------------|--------|--------|-------|--------|
| Baseline (all bash)      | 2.36s  | 12.12s | 8.31s | 86 MB  |
| +has/use/in_iuse (Rust)  | 2.15s  | 11.26s | 5.85s | 82 MB  |
| +EXPORT_FUNCTIONS (Rust) | 2.18s  | 11.06s | 5.90s | 80 MB  |
| +die (Rust)              | 2.10s  | 10.94s | 6.04s | 79 MB  |
| **Cumulative gain**      | **-11%** | **-10%** | **-27%** | **-8%** |

## Full regen — regen_cache baseline

32,138 ebuilds, 12 workers, all Rust builtins enabled:

| Metric        | Value |
|---------------|-------|
| Total         | 32138 |
| Sourced OK    | 32138 |
| Errors        | 0     |
| Mismatches    | 0     |
| Missing cache | 763   |

## Comparison vs pkgcraft

32,138 ebuilds, `-j 12`, source + write cache, run sequentially on same machine.

```
# portage-repo
./target/release/examples/regen_only gentoo -j 12 -o /tmp/portage-cache

# pkgcraft
pk repo metadata regen -p /tmp/pkgcraft-cache -n -f -j 12 gentoo/
```

pkgcraft uses [scallop](https://github.com/pkgcraft/pkgcraft/tree/main/crates/scallop)
(a wrapper around bash). portage-repo uses
[brush](https://github.com/reubeno/brush) (a pure-Rust bash reimplementation).

| Tool                      | real   | user    | sys    | maxrss  |
|---------------------------|--------|---------|--------|---------|
| pkgcraft  source+write    | 33.0s  | 186.1s  | 73.3s  | 35 MB   |
| portage-repo source+write | 31.1s  | 262.8s  | 39.0s  | 124 MB  |
| **portage-repo vs pkgcraft** | **-6%** | **+41%** | **-47%** | **+254%** |

brush pays ~41% more user CPU (younger Rust interpreter vs mature C bash) but
saves ~47% sys time (in-process vs fork/exec per ebuild). Net: ~6% faster wall time.
