<!-- @generated from em's usage spec; do not edit -->
# `em maint`

- **Usage:** `em maint [FLAGS] <SUBCOMMAND>`

System maintenance and health checks

## Global Flags
- **`-v --verbose`** — Increase verbosity: `-v` labels each build phase, `-vv`/`-vvv` add `em`'s own debug/trace logs (see also `RUST_LOG`).
- **`-q --quiet`** — Suppress non-error output
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

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`

## Subcommands

- [`em maint binhost`](maint/binhost.md)
- [`em maint binpkg <SUBCOMMAND>`](maint/binpkg.md)
- [`em maint cleanconfmem`](maint/cleanconfmem.md)
- [`em maint cleanresume [-f --fix]`](maint/cleanresume.md)
- [`em maint logs [--fix] [-t --older-than <AGE>]`](maint/logs.md)
- [`em maint merges`](maint/merges.md)
- [`em maint movebin`](maint/movebin.md)
- [`em maint moveinst`](maint/moveinst.md)
- [`em maint regen-use [-o --output <PATH>]`](maint/regen-use.md)
- [`em maint revisions [REPO]…`](maint/revisions.md)
- [`em maint sync [REPOS]…`](maint/sync.md)
- [`em maint world [-f --fix]`](maint/world.md)
