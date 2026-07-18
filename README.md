# portage-cli

[![LICENSE](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE-MIT)
[![Build Status](https://github.com/lu-zero/portage-cli/workflows/CI/badge.svg)](https://github.com/lu-zero/portage-cli/actions?query=workflow:CI)
[![dependency status](https://deps.rs/repo/github/lu-zero/portage-cli/status.svg)](https://deps.rs/repo/github/lu-zero/portage-cli)

A Rust reimplementation of the Gentoo Portage command-line tools, built on a
family of purpose-built crates for parsing atoms, metadata, repositories, and
the installed package database.

> **Note**: For a more mature Rust-based alternative, see
> [Pkgcraft](https://pkgcraft.github.io/).

> **Warning**: This codebase is currently mainly slop-coded and has not yet been
> thoroughly audited, some crates are already polished up to a degree and perform
> correctly. Use at your own risk.

> **Pre-release git checkout**: This is development source from `git` before the
> first release of `portage-cli` / the `em` binary on crates.io. The Applet status
> table below (and the per-crate "Published" vs "Local only" table) documents the
> current implementation state. See the warning above.

## The `em` binary

`em` is a unified front-end for the Portage tool suite. It dispatches to
subcommands corresponding to the traditional tools.

### Applet status

| Applet | Maps to | Status |
|--------|---------|--------|
| `atom` | — | Working |
| `query` | `equery` | Partial — see below |
| `use` | `euse` | Partial — see below |
| `maint` | `emaint` | Partial — see below |
| `regen` | `emerge --regen` | Working |
| `search` | `emerge --search` | Working |
| *(default)* | `emerge` | Working — resolve → build loop; `-uD` in-slot upgrades; `--prefix` / multi-root |
| `ebuild` | `ebuild` | Working — fetch, unpack, phases, merge, VDB registration |
| `depclean` | `emerge --depclean` | Working — reverse-dep orphan clean (`-c`); world-aware |
| `quickpkg` | `quickpkg` | Stub |
| `mirror` | `emirrordist` | Stub |
| `clean` | `eclean` | Stub |
| `revdep` | `revdep-rebuild` | Stub |
| `news` | `eselect news` | Stub |
| `glsa` | `glsa-check` | Stub |
| `log` | `genlop` | Stub |
| `grep` | `egreplite` | Stub |
| `select` | `eselect` | Partial — `profile`, `repository`, `compiler`, `binutils`, `linker`, `clang`, … |
| `crossdev` | `crossdev` | Working — cross sysroot + staged toolchain bootstrap (`--target`) |
| `toolchain` | — | Working — native self-hosting toolchain bootstrap into `--root` |
| `dispatch` | `dispatch-conf` | Stub |
| `etc` | `etc-update` | Stub |
| `env` | `env-update` | Working — `profile.env` + `ld.so.conf` from `etc/env.d` |

---

### `em query` (equery)

| Subcommand | Alias | Status |
|---|---|---|
| `belongs` | `b` | Working — file → owning package via VDB CONTENTS |
| `check` | `k` | Working — MD5 checksum + mtime verification |
| `depends` | `d` | Working — reverse-dep search in metadata cache |
| `depgraph` | `g` | Working — full dep tree via PubGrub solver, portage-compatible output |
| `files` | `f` | Working — all files installed by a package |
| `has` | `a` | Working — VDB field search across installed packages |
| `hasuse` | `h` | Working — packages with a given USE flag in IUSE |
| `keywords` | `y` | Working — keyword status across architectures |
| `list` | `l` | Working — available packages; `-I` for installed only |
| `meta` | `m` | Working — maintainers, homepage, longdesc, installed info |
| `size` | `s` | Working — installed size + build timestamp |
| `uses` | `u` | Working — IUSE flags with descriptions + installed status |
| `which` | `w` | Working — path to best matching ebuild |

**`em query depgraph` / default resolve feature summary:**

- **VDB awareness** — installed packages use `InstalledPolicy::Favor` (keep satisfying versions); already-installed exact CPVs are filtered from the merge list; installed-and-kept packages expand runtime deps only
- **`-uD` / `--update --deep`** — transitive **in-slot upgrades** (`prefer_update`); host-satisfied build tools still enter the graph so they can upgrade (emerge deep-update). `-u` alone does not mass-upgrade deps; `-D` alone still bumps `:*` slots (`prefer_newest_slot`). See `todo/deep-in-slot-upgrades.md`
- **`-N` / `--newuse` and `-U` / `--changed-use`** — same-CPV rebuild when planned USE/IUSE differs from the VDB (`todo/newuse.md`); with `-uD`, prefer newest when USE drift forces a rebuild
- **Profile USE flags** — `make.defaults` / `make.conf` through brush with portage-style incremental USE stacking (see `docs/architecture.md`)
- **USE_EXPAND** — `PYTHON_TARGETS`, `CPU_FLAGS_*`, `ABI_X86`, etc. expanded and grouped in output
- **OR-group branch selection** — prefer branches whose USE deps are already satisfied (avoids gratuitous rebuilds)
- **Post-solve USE-dep rebuilds** — violated USE deps on installed packages force rebuild / `upgrade_to` fixpoint (not full `--newuse`)
- **Action tags** — `N` new, `NS` new slot, `U` upgrade (`[old_ver]`), `D` downgrade, `R` reinstall
- **Preserve-libs** — NEEDED.ELF.2–driven orphan library keep/reclaim (parallel install-image ELF scan)
- **Cycle handling** — soft edges broken after SCC / Kahn order rather than silently dropping packages

**Performance** (AmpereOne aarch64, warm cache, hyperfine 2026-07-18; exit 1 ignored when the plan needs config changes):

| Target / mode | `emerge` | `em` |
|---------------|---------:|-----:|
| `www-client/firefox` `-p` | ~3.65 s | **~1.4 s** |
| `www-client/firefox` `-uDp` | ~6.3 s | **~1.45 s** (plan size ≈ emerge) |
| `app-office/libreoffice` `-p` | ~4.0 s | **~1.75 s** |

Older micro-tables (sub-second depgraph on lighter hosts) live in
[`benchmarks/BENCHMARKS.md`](./benchmarks/BENCHMARKS.md). Metadata cache regen
and install-image ELF scan benches are also there (`benchmarks/bench-elfscan.sh`).

**Gaps vs `emerge`:**
- Shallow `-p` package-set can still differ slightly from emerge on some hosts (emerge may list extra BDEPEND upgrades without `-u`); `-uDp` / `-uNDp` are near-parity on firefox-class targets
- Wrapper packages for old-slot BDEPEND (`autoconf-wrapper`, `gcc-config`, …) not fully modelled
- Flag ordering / `(-flag)` USE_EXPAND_IMPLICIT display polish
- Upgrade display shows full USE rather than only changed flags

**Gaps vs equery:**
- `uses` descriptions come from `profiles/use.desc` + `profiles/use.local.desc`.
  Overlay packages not yet regen'd fall back to empty description (metadata.xml
  per-package lookup is not yet wired as a fallback).
- No `stats` subcommand.

---

### `em use` (euse)

| Flag | Status |
|---|---|
| `-a FLAG` | Working — add USE flag to `make.conf` |
| `-r FLAG` | Working — remove USE flag from `make.conf` |
| *(no flags)* | Working — print current USE value |
| `--make-conf PATH` | Working — override make.conf path |

**Gaps vs euse:**
- No `-p pkg` for package-specific USE flags (`/etc/portage/package.use`).
- `get()` returns the raw unexpanded value; `${COMMON_FLAGS}` references are
  not evaluated (brush-backed expansion is possible but not wired yet).

---

### `em maint` (emaint)

| Subcommand | Status | Notes |
|---|---|---|
| `world` | Working | Checks `world` + `world_sets`; validates `@set` refs against known sets from `/usr/share/portage/config/sets/`, `/etc/portage/sets.conf`, and `/etc/portage/sets/`; `--fix` rewrites both files |
| `revisions` | Working | Purges `repo_revisions` JSON (sync commit history); optional per-repo targeting |
| `moveinst` | Partial | Detects packages needing rename from `profiles/updates/`; does not apply moves or scan installed dependency metadata |
| `regen` | Working | Available as `em regen` |

**Gaps vs emaint:**

- `moveinst` — missing the second pass that walks every installed package's
  `DEPEND`/`RDEPEND`/etc. fields for stale atom references, and the `--fix`
  mode that writes to the VDB.
- `world` — `@set` references are validated by name but not by content (e.g.
  `@preserved-rebuild` is accepted as long as the name is known).
- `all`, `binhost`, `cleanconfmem`, `cleanresume`, `logs`, `merges`,
  `movebin`, `sync` — not implemented.

---

## Cross-compilation & toolchains

`em` understands the multi-root model (`docs/root-model.md`): a build reads its
config from one root (`--config-root`) and installs into another (`--root`),
with build tools resolved against the host (`BROOT`). On top of that it can
bootstrap toolchains and assemble stages.

- **`em --target <tuple> crossdev --init-target`** lays down a cross sysroot + overlay
  (a `crossdev` workalike); **`--setup`** then runs the staged
  `binutils → headers → gcc-stage1 → libc → gcc-stage2` bootstrap into
  `/usr/<tuple>`. Validated end-to-end for `riscv64-unknown-linux-gnu`.
- **`em toolchain --setup --root <dir>`** bootstraps a *native* self-hosting
  toolchain (`CHOST == CBUILD`) into an empty root —
  `baselayout → binutils → os-headers → glibc → gcc`. Unlike cross there is no
  two-stage gcc: the host (seed) compiler builds full glibc directly and a single
  full gcc links against it. Verified: a fully automated run produces a
  `gcc-16.1` in the root that compiles and links a working binary against the
  root's own libc.

The native toolchain and the cross bootstrap share one staged driver
(`crossdev::stages`), differing only in atom naming and how the `glibc ↔ gcc`
cycle is broken. Stage *production* (stage1 `packages.build`, stage3
`--emptytree @system`) is the next layer — see `todo/em-stages-and-binhosts.md`.

---

## Architecture

See [`docs/architecture.md`](./docs/architecture.md) for the full crate
dependency graph, per-crate API catalog, and design reference.

### Crate family

| Crate | Purpose | Status |
|-------|---------|--------|
| `gentoo-interner` | String interning | Published |
| `gentoo-core` | Architecture and variant types | Published |
| `gentoo-stages` | Stage3 tarball fetch/cache | Published |
| `portage-atom` | PMS atom parser (`Cpn`, `Cpv`, `Dep`, `Version`) | Published |
| `portage-metadata` | md5-cache entry parser, EAPI, phases, keywords | Published |
| `portage-solver` | Solver-agnostic trait and shared vocabulary | Published |
| `portage-atom-pubgrub` | PubGrub solver bridge (default in `em`) | Published |
| `portage-atom-resolvo` | SAT dependency solver (resolvo bridge) | Published |
| `portage-vdb` | Installed package database (`/var/db/pkg`) | Published |
| `portage-binpkg` | GPKG binary package read/write | Published |
| `portage-repo` | Repo layout, profiles, metadata cache, ebuild sourcing | Local only |
| `portage-distfiles` | Distfile fetch and mirror resolution | Local only |
| `portage-cli` | The `em` binary | Local only |
| `portage-bench` | Benchmark harness (`benchmarks/`) | Local only |

Further reading: [`docs/architecture.md`](./docs/architecture.md),
[`docs/build-roadmap.md`](./docs/build-roadmap.md),
[`docs/benchmarks.md`](./docs/benchmarks.md),
[`todo/PENDING.md`](./todo/PENDING.md) (stage/binhost arc),
[`todo/deep-in-slot-upgrades.md`](./todo/deep-in-slot-upgrades.md) (`-uD`),
[`todo/newuse.md`](./todo/newuse.md) (`-N` still open).

### brush integration

`portage-repo` embeds [brush](https://github.com/lu-zero/brush) (the
`for-portage-repo` fork branch) — a Rust bash interpreter — for ebuild
sourcing and `make.conf` parsing. Additions to the fork:

- `Program.comments: Vec<SourceSpan>` — comment spans from the winnow parser,
  used by `MakeConf` for byte-precise round-trip editing.
- `ParseContext.comments` accumulator and comment-tracking whitespace parsers
  (`spaces_tracking`, `linebreak_tracking`, `newline_list_tracking`).

## Installation

```bash
cargo install --path portage-cli
```

## Local Development

The project expects sibling checkouts:

```
portage-cli/    # this workspace
brush/          # brush fork at for-portage-repo branch
```

The `.cargo/config.toml` at workspace root patches the brush crates to use the
local checkout.

```bash
cargo build
cargo test --workspace --exclude portage-bench
cargo clippy --workspace --exclude portage-bench -- -D warnings
cargo fmt --all -- --check
cargo msrv verify --rust-version 1.95 --path portage-cli
```

## License

[MIT](LICENSE-MIT)

## Contributing

See [AGENTS.md](./AGENTS.md) for project conventions (Conventional Commits,
style, checks).

## Author

Luca Barbato <lu_zero@gentoo.org>
