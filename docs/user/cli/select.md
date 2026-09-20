<!-- @generated from em's usage spec; do not edit -->
# `em select`

- **Usage:** `em select [FLAGS] <SUBCOMMAND>`

Native config selectors (profile, repos) — eselect-like

## Global Flags
- **`--arch <ARCH>`** — Target architecture for operations (default: current system architecture)
- **`--repo <PATH>`** — Pin search/query to a single repository

  When unset, repositories are auto-discovered from `repos.conf` (the main repo wins for single-repo applets; search walks all of them).
- **`-p --pretend`** — Show what would be done without actually performing any actions
- **`--vdb <PATH>`** — Override VDB path (default: $ROOT/var/db/pkg)

## Roots
- **`--prefix <DIR>`** — Unprivileged offset: ROOT/VDB/distfiles/build trees under DIR; config still from the host (use --root for a config offset).
- **`--local [DIR]`** — Unprivileged, standalone Gentoo-Prefix: own VDB/BROOT/config, not overlaid on the host (see --prefix for the overlay). Defaults to ~/.gentoo (EPREFIX=~/.gentoo) when no DIR is given (`--local=`).

  A bare `--local` takes the next word as DIR.
- **`--config-root <PATH>`** — Read config (profile, make.conf) from this root instead of `--root`
- **`-T --target <TUPLE>`** — Cross-build/setup for a crossdev target tuple

  The single source for "which tuple" everywhere: `em crossdev --target T --init-target` sets T up; `em stages --target T --stage1` (or any plain atom build) resolves/installs into the target sysroot `<EROOT>/usr/<TUPLE>` — sugar for `--config-root <sysroot> --root <sysroot>`.

  Cross context (CHOST/CBUILD, `--root-deps=rdeps`) is read from the sysroot make.conf. One flag for both roles — `crossdev` no longer has its own `-t`/`--target`.
- **`--root <PATH>`** — Installation root (the offset an applet installs into / queries)

## Flags
- **`-h --help`** — Print help

## Subcommands

- [`em select binutils <SUBCOMMAND>`](select/binutils.md)
- [`em select clang <SUBCOMMAND>`](select/clang.md)
- [`em select compiler <SUBCOMMAND>`](select/compiler.md)
- [`em select glsa [SUBCOMMAND]`](select/glsa.md)
- [`em select linker <SUBCOMMAND>`](select/linker.md)
- [`em select mirrors <SUBCOMMAND>`](select/mirrors.md)
- [`em select news [SUBCOMMAND]`](select/news.md)
- [`em select pkgconf <SUBCOMMAND>`](select/pkgconf.md)
- [`em select profile <SUBCOMMAND>`](select/profile.md)
- [`em select repository <SUBCOMMAND>`](select/repository.md)
