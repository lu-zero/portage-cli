# Binary packages & binhosts — operator guide

`em` builds, indexes, verifies, and reuses GLEP 78 binary packages (`.gpkg.tar`
containers) locally (`-b`/`-B`, `-k`/`-K`) and over the network (`-g`/`-G`,
`binrepos.conf`/`PORTAGE_BINHOST`). This is the how-to: what makes two
binpkgs interchangeable, and the recipes for running this across boards,
cross-compilation targets, and a shared cache.

## Identity model

A candidate binpkg is reusable for a given plan entry iff all of these match:

| Axis | Rule | Empty / missing |
|------|------|------------------|
| **CPV** | exact | required |
| **USE ∩ IUSE** | equal (Portage bug #453400) | — |
| **CHOST** | equal | either side empty → skip the gate |
| **Build-env key** | equal | **asymmetric**: an unkeyed binpkg (no recorded flags — old GPKG, backward compat) always matches; an unkeyed *desired* build never matches a binpkg that *is* keyed |

The CHOST axis handles cross-compilation (host tools vs. a `--target`
sysroot). The build-env key axis handles boards that share a CHOST but
differ in ISA/ABI-relevant flags (e.g. two `riscv64-unknown-linux-gnu`
boards, one `-march=rv64gcv_zvl256b`, one `-march=rva23u64`) — CHOST alone
can't tell them apart, and full CFLAGS-string equality is too brittle
(`-O2` vs `-O3`, flag reorder, unrelated warnings flags).

### Build-env key

Computed from `CFLAGS`/`CXXFLAGS`/`LDFLAGS`/`RUSTFLAGS`: every `-m*` token
(GCC/Clang's own "machine dependent options" convention — `-march=`,
`-mcpu=`, `-mrvv-vector-bits=`, `-mno-outline-atomics`, x86 feature toggles
like `-mavx2`, …) plus Rust `-C target-cpu=`/`-C target-feature=`, everything
else (`-O*`, `-pipe`, `-g`, warnings, include/lib paths) dropped as noise,
then canonicalized and hashed. `-march=native` (or any machine-dependent
flag) collapses to a `__native__` sentinel instead of a hash, since "native"
means a different real ISA per machine.

### Short key / slug

`em maint binpkg list` and `em maint binpkg fingerprint` both display a
short, path-safe form of the key:

| Key | Slug |
|---|---|
| empty (no ISA-relevant flags — a generic build) | `generic` |
| `__native__` (`-march=native`) | `native` |
| anything else | first 12 hex chars of the key's MD5 |

This slug is stable (same flags → same slug) and safe to use directly as a
`PKGDIR` path component (Recipe 1 below).

## Recipes

### Recipe 1 — separate PKGDIR per variant (stages default, zero risk)

One PKGDIR per board/CFLAGS-fingerprint — the simplest and safest layout,
and what crossdev-stages-style automation should default to. No
multi-instance handling needed on either side; `-k` only ever sees one
board's packages.

```bash
export PKGDIR=/var/cache/em-binpkgs/${CHOST}/$(em maint binpkg fingerprint --target "$T")
em --target "$T" -b -k @system
```

### Recipe 2 — shared PKGDIR, multi-instance (one disk cache, multiple boards)

A single `PKGDIR` can hold multiple boards' binpkgs at once — the index is
multi-instance (`cpv → Vec<entry>`), and `find_reusable` picks the newest
`BUILD_ID` among the entries matching CHOST + key.

```bash
export PKGDIR=/var/cache/em-binpkgs/shared

# board A (CFLAGS in that root's own make.conf)
em --target riscv64-unknown-linux-gnu --root /boards/k1 -b -k @system
# board B
em --target riscv64-unknown-linux-gnu --root /boards/k3 -b -k @system

# both boards' binpkgs remain; -k only ever matches its own board's key
em maint binpkg list    # CHOST/KEY columns show the two boards side by side
em maint binpkg prune   # keeps one BUILD_ID per (cpv, chost, key) — not per cpv
```

Sample `list` output distinguishing two board variants of the same package:

```text
sys-libs/glibc-2.42                    3      18.2 MiB  riscv64-unknown-linux-gnu  a1b2c3d4e5f6  -O2 -march=rv64gcv_zvl256b       app-glibc/glibc-2.42-3.gpkg.tar
sys-libs/glibc-2.42                    2      18.1 MiB  riscv64-unknown-linux-gnu  9f8e7d6c5b4a  -O2 -march=rva23u64              app-glibc/glibc-2.42-2.gpkg.tar
```

### Recipe 3 — cross host tools (host BDEPEND reuse under `--target`)

A `--target` plan can carry `MergeRoot::Host` entries — unsatisfied BDEPEND
(build tools like `cmake`) scheduled onto the build host, not the target
sysroot. These accumulate in the **host**'s own PKGDIR under the host's own
CHOST, and a cross plan's per-entry dual index reuses them independently of
the target board:

```bash
# Host tools accumulate in the host PKGDIR with the host CHOST
em -b -k dev-build/cmake

# A cross plan's Host BDEPEND entries reuse them from the host PKGDIR,
# regardless of which board's target CHOST/key the rest of the plan uses
em --target "$T" -k -e @system
```

This is code-complete and unit-tested (per-entry PKGDIR dual index, no
fallback to the target index when the host index is unavailable); a live
verification against a real crossdev sandbox is still WIP.

### Recipe 4 — remote binhosts (`binrepos.conf`)

Either separate sections per board (client picks by priority, still one
instance per index — no multi-instance parsing needed):

```ini
# /etc/portage/binrepos.conf
[riscv64-rvv]
sync-uri = https://binhost.example/riscv64/rv64gcv
priority = 10

[riscv64-generic]
sync-uri = https://binhost.example/riscv64/rv64gc
priority = 5
```

or one URI serving a multi-instance `Packages` index (same CPV, multiple
CHOST/key blocks) — the client parses every block and matches USE + CHOST +
key exactly like the local case.

## `package.env` per-package overrides (S6)

A package with its own `/etc/portage/package.env` CFLAGS override (e.g. a
`WORKAROUND_CFLAGS`-style entry disabling a problem ISA extension for one
package) gets its own build-env key, folded from the env file on top of the
make.conf baseline — both append (`CFLAGS="${CFLAGS} -mno-foo"`) and override
(`CFLAGS="-bar"`) forms are handled. Known approximations:

- Slot-qualified `package.env` atoms aren't matched at plan/reuse-check time
  (a plan entry doesn't carry a slot there) — the real build still applies
  them; the worst case is a missed reuse for that package, never a wrong one.
- An env file containing real shell logic (conditionals, command
  substitution) isn't evaluated — only plain assignments are. Same safe
  direction: a missed reuse or an extra rebuild, never wrong-arch reuse.

## `em maint binpkg` reference

| Subcommand | What it does |
|---|---|
| `verify [--fix]` | Recompute each indexed container's size/MD5/SHA1 against the file on disk; `--fix` quarantines corrupt containers and reindexes |
| `list` | cpv, build-id, size, CHOST, build-env key (slug), CFLAGS (truncated), path |
| `prune [--dry-run]` | Keep only the newest `BUILD_ID` per `(cpv, chost, build_env_key)`, deleting older ones and reindexing |
| `fingerprint [--full] [--host]` | Print the build-env key for the current roots' make.conf flags — the short slug by default (script-consumable), `--full` for every flag var plus both key forms, `--host` to fingerprint `BROOT` instead of the target roots (only differs under `--target`) |

## Stages automation: default PKGDIR layout

`em` does not force a `PKGDIR` subpath in code — that stays an operator
choice (either separate-PKGDIR-per-variant or shared-multi-instance, both
fully supported). The **recommended default** for `em stages`/
crossdev-stages-style automation is Recipe 1's layout:

```text
PKGDIR=/var/cache/em-binpkgs/${CHOST}/<build-env-fingerprint>
```

using `em maint binpkg fingerprint`'s slug as `<build-env-fingerprint>` —
zero multi-instance handling required, and `-k` can never accidentally pick
up another board's binpkg even before the CHOST/key gates would catch it.
