# portage-cli

[![LICENSE](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE-MIT)
[![Build Status](https://github.com/lu-zero/portage-cli/workflows/CI/badge.svg)](https://github.com/lu-zero/portage-cli/actions?query=workflow:CI)
[![dependency status](https://deps.rs/repo/github/lu-zero/portage-cli/status.svg)](https://deps.rs/repo/github/lu-zero/portage-cli)

A Rust reimplementation of the Gentoo Portage command-line tools, built on a
family of purpose-built crates for parsing atoms, metadata, repositories, and
the installed package database. `em` is the unified front-end binary; it
dispatches to subcommands corresponding to the traditional tools (`emerge`,
`equery`, `euse`, `emaint`, …).

> **Note**: For a more mature Rust-based alternative, see
> [Pkgcraft](https://pkgcraft.github.io/).

> **Warning**: This codebase is currently mainly slop-coded and has not yet been
> thoroughly audited, some crates are already polished up to a degree and perform
> correctly. Use at your own risk.

> **Pre-release git checkout**: This is development source from `git` before the
> first release of `portage-cli` / the `em` binary on crates.io. See
> [`docs/architecture.md`](./docs/design/architecture.md) for the per-crate
> published/local-only breakdown.

## Applet status

| Applet | Maps to | Status |
|--------|---------|--------|
| `atom` | — | Working |
| `query` | `equery` | Partial — [detail](./docs/user/applets.md#em-query-equery) |
| `use` | `euse` | Partial — [detail](./docs/user/applets.md#em-use-euse) |
| `pkg` | — | Working — edit `package.use` / `.accept_keywords` / `.mask` / `.env` |
| `maint` | `emaint` | Partial — [detail](./docs/user/applets.md#em-maint-emaint) |
| `sync` | `emaint sync` / `emerge --sync` | Working — `git` and `rsync` from `repos.conf` |
| `regen` | `emerge --regen` | Working |
| `search` | `emerge --search` | Working |
| *(default)* | `emerge` | Working — resolve → build loop; `-uD` in-slot upgrades; `--prefix` / multi-root |
| `ebuild` | `ebuild` | Working — fetch, unpack, phases, merge, VDB registration |
| `depclean` | `emerge --depclean` | Working — reverse-dep orphan clean (`-c`); world-aware |
| `quickpkg` | `quickpkg` | Working — GPKG from installed files / VDB `CONTENTS`; skips `CONFIG_PROTECT` by default |
| `mirrordist` | `emirrordist` | Working — [detail](./docs/user/applets.md#em-mirrordist-emirrordist) |
| `clean` | `eclean` | Working — [detail](./docs/user/applets.md#em-clean-eclean) |
| `revdep` | `revdep-rebuild` | Working — [detail](./docs/user/applets.md#em-revdep-revdep-rebuild) |
| `log` | `genlop` | Working — `current`/`list`/`time`/`predict`; see [docs/activity.md](./docs/user/activity.md) |
| `grep` | `egreplite` | Planned — [detail](./docs/user/applets.md#em-portageq-and-em-grep-planned-user-facing) |
| `portageq` | `portageq` | Planned — [detail](./docs/user/applets.md#em-portageq-and-em-grep-planned-user-facing) |
| `read` | `elogv` / elog reader | Working — [detail](./docs/user/applets.md#em-read-elogv-and-the-elog-system) |
| `select` | `eselect` | Partial — `profile`, `repository`, `compiler`, `binutils`, `linker`, `clang`, `pkgconf`, `mirrors`, `news` (`eselect news`), `glsa` (`glsa-check`), … |
| `active` | — | Working — register default `--prefix`/`--local` for bare `em` |
| `setup` | — | Working — bootstrap a prefix layout (`--local` / `--prefix`) |
| `crossdev` | `crossdev` | Working — cross sysroot + staged toolchain bootstrap; see [docs/crossdev.md](./docs/user/crossdev.md) |
| `toolchain` | — | Working — native self-hosting toolchain bootstrap into `--root` |
| `stages` | catalyst stage1/3 | Partial — `--stage1` (`packages.build`), `--stage3` (emptytree `@system`); no stage4 yet |
| `dispatch` | `dispatch-conf` | Planned — [the open gap](./docs/user/applets.md#config-file-reconciliation-the-open-gap) |
| `etc` | `etc-update` | Planned — [the open gap](./docs/user/applets.md#config-file-reconciliation-the-open-gap) |
| `env` | `env-update` | Working — `profile.env` + `ld.so.conf` from `etc/env.d` |

## Documentation

**User guides** (`docs/user/`) — how to actually run `em`:

| Doc | Covers |
|-----|--------|
| [`docs/user/intro.md`](./docs/user/intro.md) | **Start here** — what `em` is, quick start, topologies, common workflows |
| [`docs/user/root-model.md`](./docs/user/root-model.md) | `--root`, `--prefix`, `--config-root`, and the other location flags — read this first |
| [`docs/user/prefix-toolchain.md`](./docs/user/prefix-toolchain.md) | How to bootstrap and use a `--prefix`'s own compiler |
| [`docs/user/crossdev.md`](./docs/user/crossdev.md) | Cross-compilation targets (`--target`, `em crossdev`) |
| [`docs/user/binhost.md`](./docs/user/binhost.md) | Binary packages (`-b`/`-B`/`-k`/`-g`) and binhost identity model |
| [`docs/user/activity.md`](./docs/user/activity.md) | Structured progress/history/ETA (`em log`) |
| [`docs/user/applets.md`](./docs/user/applets.md) | Per-applet detail, gaps vs. the real tool, benchmarks |
| [`docs/user/stages-and-testing.md`](./docs/user/stages-and-testing.md) | Bootstrapping and validating a ROOT with `em stages` |

There is no dedicated `--local` user guide yet — see
[`todo/local-bootstrap.md`](./todo/local-bootstrap.md), which tracks status
rather than walking through usage.

**Design & architecture** (`docs/design/`) — how `em` is built, and why:

| Doc | Covers |
|-----|--------|
| [`docs/design/architecture.md`](./docs/design/architecture.md) | Crate dependency graph, per-crate API catalog, `em -p` pipeline |
| [`docs/design/root-topology.md`](./docs/design/root-topology.md) | The root/prefix/sysroot model's implementation reference |
| [`docs/design/em-prefix-experiment.md`](./docs/design/em-prefix-experiment.md) | Why EPREFIX/multi-root bugs happen |
| [`docs/design/build-environment.md`](./docs/design/build-environment.md) | How phase env vars are resolved and read |
| [`docs/design/bash-crossdev-matrix.md`](./docs/design/bash-crossdev-matrix.md) | `em crossdev` vs. real bash-crossdev fidelity reference |
| [`docs/design/worker-build-tree.md`](./docs/design/worker-build-tree.md) | The unprivileged-build worker split |
| [`docs/design/dep-resolver.md`](./docs/design/dep-resolver.md) | Historical resolver investigation (superseded by architecture.md) |
| [`docs/design/use-flags-api-design.md`](./docs/design/use-flags-api-design.md) | Effective-USE-flag API design notes |
| [`docs/design/testing.md`](./docs/design/testing.md) | Testing strategy, benchmarking, bumping the brush fork |
| [`docs/design/benchmarks.md`](./docs/design/benchmarks.md) | How to measure performance across the workspace |
| [`AGENTS.md`](AGENTS.md) | Contributor conventions: build commands, style, commit format |

Status trackers (open work, not reference docs) live under `todo/` — start
at [`todo/PENDING.md`](./todo/PENDING.md).

## Installation

```bash
cargo install --path portage-cli
```

## Local Development

See [AGENTS.md](AGENTS.md) for build commands, local dependency overrides,
and CI-parity checks.

## License

[MIT](LICENSE-MIT)

## Contributing

See [AGENTS.md](AGENTS.md) for project conventions (Conventional Commits,
style, checks).

## Author

Luca Barbato <lu_zero@gentoo.org>
