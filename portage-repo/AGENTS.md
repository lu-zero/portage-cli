# Project Conventions

## Build Commands

```bash
cargo test                        # Run all tests (unit + doc)
cargo clippy -- -D warnings       # Lint — must be warning-free
cargo fmt --check                 # Format check — must pass
cargo doc --no-deps               # Build docs — must have no warnings
cargo run --example enumerate_repo -- /path/to/repo  # Smoke-test the example
cargo run --release --example regen_cache -- gentoo  # Regenerate metadata cache
cargo run --release --example regen_only -- gentoo   # Regenerate (no comparison)
```

## Architecture

- One primary type per module (`layout.rs` -> `LayoutConf`, `repository.rs` -> `Repository`, etc.)
- Modules are private (`mod`, not `pub mod`); public API is flat re-exports in `lib.rs`
- Shell integration uses [brush-core](https://crates.io/crates/brush-core) for bash evaluation
- Depends on [portage-atom](https://crates.io/crates/portage-atom) for atom parsing and
  [portage-metadata](https://crates.io/crates/portage-metadata) for cache entry types

## Dependencies

- `portage-atom` — PMS atom parsing (Cpn, Cpv, Dep, etc.)
- `portage-metadata` — metadata cache types (CacheEntry, EbuildMetadata, Eapi)
- `brush-core` + `brush-builtins` — Rust bash shell for sourcing ebuilds/eclasses
- `tokio` — async runtime required by brush
- `thiserror` — error derive macros

## PMS Compliance

This library implements the [Package Manager Specification (PMS)](https://projects.gentoo.org/pms/9/pms.html).
All public types must reference the relevant PMS section in their doc comments
(e.g. `See [PMS 4](...)`).

## Coding Style

- `rustfmt` — all code must be formatted
- No dead code, no unused dependencies
- Doc comments on all public types, fields, and enum variants
- Tests live in a `#[cfg(test)] mod tests` block at the bottom of each module

## Commits

[Conventional Commits](https://www.conventionalcommits.org/):

- `feat:` — new functionality
- `fix:` — bug fix
- `refactor:` — code restructuring without behaviour change
- `docs:` — documentation only
- `test:` — adding or updating tests
- `ci:` — CI/CD changes
- `chore:` — maintenance (dependencies, tooling)

## MSRV

MSRV follows the workspace floor (**1.95**, `rust-version.workspace = true` in
`Cargo.toml`). See root [`AGENTS.md`](../AGENTS.md).
CI tests against both stable and MSRV. Do not use features that require a newer
version without updating `rust-version` in `Cargo.toml` and the CI matrix.

## Slop Warning

This codebase was largely AI-generated. Be skeptical of existing code — it may
contain bugs, incomplete PMS coverage, or surprising edge-case behaviour.
Do not assume existing patterns are correct; verify against the PMS.

Enforced anti-slop rules (comment length, test quality, scope discipline):
see root [`AGENTS.md`](../AGENTS.md) § Unslop Rules.

## Benchmarking

### Scripts

- **`benchmark.sh`** — full benchmark driver. Clones the Gentoo mirror if absent
  (`./gentoo/`), builds release binaries, runs `regen_cache` for correctness
  verification, then times `regen_only` against the full tree and the `dev-util/*`
  subset at 1/2/4/8 jobs. Results are written to `/tmp/benchmark_results.csv`.

- **`benchmark_baseline.txt`** — historical timing table (real/user/sys/RSS) for
  key phases, measured against `dev-libs/*` (≈1,236 ebuilds) on macOS. Update this
  file whenever a change intentionally affects performance so regressions are visible.

- **`bench-regen.sh`** — benchmark `regen_only` at multiple thread counts, reporting
  wall time and peak RSS. Accepts optional list of job counts as arguments.

- **`bench-pk.sh`** — same measurement for `pk repo metadata regen` (pkgcraft).
  Expects `../pkgcraft/target/release/pk` or `PK=<path>` in the environment.

- **`bench.sh`** — head-to-head comparison: runs both `regen_only` and `pk` at the
  same job counts using `hyperfine`, then produces a combined timing+RSS table.
  Requires `hyperfine` to be installed.

### Quick setup

The examples expect a Gentoo ebuild tree at `./gentoo/`.  If it is missing,
clone the mirror with a shallow fetch (≈200 MB):

```bash
git clone --depth 1 https://github.com/gentoo/gentoo.git gentoo
```

### Running a quick comparison

```bash
# Build release and time a subset (no full-tree clone needed)
cargo build --release --example regen_only
/usr/bin/time -l ./target/release/examples/regen_only ./gentoo 'dev-libs/*' -j 1
```

Use single-threaded (`-j 1`) for per-change comparisons — variance is tighter than
the parallel run. Record results in `benchmark_baseline.txt` with the date and a
short phase description.

### portage-repo vs pkgcraft head-to-head

Both scripts write to a temporary output directory and measure peak RSS via
`/proc/PID/status` polling (more accurate than `ru_maxrss` on Linux):

```bash
# portage-repo only (all default job counts: 4 8 16 20 24 32 40)
GENTOO_REPO=/var/db/repos/gentoo ./bench-regen.sh

# pkgcraft only
GENTOO_REPO=/var/db/repos/gentoo PK=../pkgcraft/target/release/pk ./bench-pk.sh

# head-to-head (requires hyperfine)
GENTOO_REPO=/var/db/repos/gentoo ./bench.sh 4 8 16 32

# build pkgcraft if needed
cargo build --release --manifest-path ../pkgcraft/Cargo.toml --bin pk
```

The combined table from `bench.sh` looks like:

```
   j   regen real   regen RSS     pk real      pk RSS
------------------------------------------------------
   4      84.12s      117 MB      12.34s        89 MB
   8      48.67s      167 MB       7.10s       143 MB
```

## Cache Comparison

After generating caches with different tools, use the `compare_caches` example to
compare them field by field. It uses the `portage-metadata` parsers for semantic
comparison (parsed dep trees, token sets) rather than raw text diff, so ordering
differences do not produce false positives.

### Generating reference caches

```bash
REPO=/var/db/repos/gentoo

# portage-repo
OUT_PR=$(mktemp -d) && cargo run --release --example regen_only -- "$REPO" -o "$OUT_PR"

# pkgcraft
OUT_PK=$(mktemp -d) && pk repo metadata regen -j 16 -p "$OUT_PK" -n -f "$REPO"

# portage (egencache) — writes into the repo itself unless --external-cache-only is used
OUT_PORTAGE=$(mktemp -d)
egencache --update --repo gentoo --external-cache-only \
    --repositories-configuration "[gentoo]
location = $REPO" \
    --cache-dir "$OUT_PORTAGE" -j 16
# Note: egencache requires portage to be configured with the repo. Alternatively:
#   emerge --metadata --jobs 16  (writes to REPO/metadata/md5-cache/)
#   then OUT_PORTAGE=$REPO/metadata/md5-cache
```

### Comparing two caches

```bash
cargo build --release --example compare_caches

# portage-repo vs pkgcraft
./target/release/examples/compare_caches "$OUT_PR" "$OUT_PK"

# portage-repo vs portage reference
./target/release/examples/compare_caches "$OUT_PR" "$OUT_PORTAGE"

# pkgcraft vs portage reference
./target/release/examples/compare_caches "$OUT_PK" "$OUT_PORTAGE"
```

Output for each field difference:

```
DIFF cat/pkg-1.0 IUSE:
  a: nls ssl threads
  b: nls threads ssl
```

Exit code is 0 only when all fields match and no entries are missing from either side.

### Comparison strategy per field

| Field(s)                                            | How compared                         |
|-----------------------------------------------------|--------------------------------------|
| EAPI, DESCRIPTION, SLOT, HOMEPAGE                   | Exact string equality                |
| IUSE, KEYWORDS, DEFINED_PHASES                      | Token set (order ignored)            |
| DEPEND, RDEPEND, BDEPEND, PDEPEND, IDEPEND,         | Parsed dep tree, nodes sorted at     |
|   LICENSE, RESTRICT, PROPERTIES, REQUIRED_USE       | each level for order-independence    |
| SRC_URI                                             | Parsed SrcUriEntry tree              |
| _eclasses_                                          | Eclass-name set (checksums ignored)  |
| INHERIT, INHERITED, _md5_                           | Excluded (implementation-specific)   |

### Deduplication semantics

The three implementations differ in how they handle duplicate tokens in incremental
metadata variables (IUSE, DEPEND, RDEPEND, etc.):

| Implementation | Approach                                                  |
|----------------|-----------------------------------------------------------|
| **portage**    | No deduplication — raw concatenation of eclass strings   |
| **pkgcraft**   | Deduplicates via `IndexSet`, first-occurrence wins        |
| **portage-repo** | Same as pkgcraft: `IndexSet` in `InheritState::accumulated` |

Because `compare_caches` parses DEPEND/RDEPEND/… as dep trees and IUSE/KEYWORDS as
token sets, duplicate tokens in portage output do not cause false-positive diffs.
However they are visible in the raw `a:` / `b:` lines.

To flag ebuilds that contribute duplicate tokens themselves (as opposed to
duplicates arising from eclass accumulation), portage-repo emits a `QA:` warning
during sourcing:

```
QA: cat/pkg-1.0: IUSE has duplicate tokens: nls
```

This fires only when the ebuild's own IUSE/DEPEND/… string has repeated tokens
before merging with eclass contributions.

## Debugging parsing issues

If either an ebuild or an eclass do not parse correctly, we may have found a bug in the
parser we use, `brush`. Its sources are in `../brush` the binary is often in
`../brush/target/debug/brush`. You might have to rebuild it to make sure it matches
the current codebase.

To confirm the problem and create a minimal test case:

1. First, verify that bash can parse the file: `bash -n {the problematic file}`
2. If bash accepts it but brush fails, use `brush -n` to minimize the test case:
   - Create a copy of the file without any functions
   - If it parses without functions, add functions back one at a time until parsing fails
   - If it fails without any functions, the issue is in the global scope
3. Once isolated to a specific function or global scope:
   - Remove complete bash commands one at a time
   - Continue until you isolate the single problematic command

When reporting the issue:
- Include the minimal problematic command
- Note the brush and bash versions used
- Specify whether the issue occurs in function scope or global scope

Do not attempt to fix brush issues yourself - report the minimal test case and stop.
