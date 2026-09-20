<!-- @generated from em's usage spec; do not edit -->
# `em search`

- **Usage:** `em search [FLAGS] [PATTERN]`
- **Effect:** read-only

Search package names and descriptions

## Arguments
- **`[PATTERN]`** — Pattern to search (required unless --all)

## Flags
- **`-a --all`** — List all packages (no pattern required)
- **`-S --desc`** — Search package descriptions instead of names
- **`-N --name-only`** — Show only package name, no description
- **`-H --homepage`** — Show homepage instead of description
- **`--repo <PATH>`** — Pin search/query to a single repository

  When unset, repositories are auto-discovered from `repos.conf` (the main repo wins for single-repo applets; search walks all of them).
- **`-h --help`** — Print help

## Roots

Which tree this invocation reads and writes.
- **`--prefix <DIR>`** — Unprivileged offset: ROOT/VDB/distfiles/build trees under DIR; config still from the host (use --root for a config offset).
- **`--local [DIR]`** — Unprivileged, standalone Gentoo-Prefix: own VDB/BROOT/config, not overlaid on the host (see --prefix for the overlay). Defaults to ~/.gentoo (EPREFIX=~/.gentoo) when no DIR is given (`--local=`).

  A bare `--local` takes the next word as DIR.
- **`--config-root <PATH>`** — Read config (profile, make.conf) from this root instead of `--root`
- **`-T --target <TUPLE>`** — Cross-build/setup for a crossdev target tuple

  The single source for "which tuple" everywhere: `em crossdev --target T --init-target` sets T up; `em stages --target T --stage1` (or any plain atom build) resolves/installs into the target sysroot `<EROOT>/usr/<TUPLE>` — sugar for `--config-root <sysroot> --root <sysroot>`.

  Cross context (CHOST/CBUILD, `--root-deps=rdeps`) is read from the sysroot make.conf. One flag for both roles — `crossdev` no longer has its own `-t`/`--target`.
- **`--root <PATH>`** — Installation root (the offset an applet installs into / queries)

## Examples

```
em search firefox
```
