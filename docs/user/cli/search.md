<!-- @generated from em's command line; do not edit -->
# em search

`em search` — search package names and descriptions

**Effect:** read-only.

## Usage

```
em search [OPTIONS] [PATTERN]
```

## Arguments

* `[PATTERN]` — Pattern to search (required unless --all)

## Options

* `-a, --all` — List all packages (no pattern required)
* `-S, --desc` — Search package descriptions instead of names
* `-N, --name-only` — Show only package name, no description
* `-H, --homepage` — Show homepage instead of description
* `--search-index <y|n>` — `n`: match descriptions from the md5-cache, not `metadata/pkg_desc_index`
  * `y`: Yes
  * `n`: No
* `--repo <PATH>` — Pin search/query to a single repository

  When unset, repositories are auto-discovered from `repos.conf` (the main repo wins for single-repo applets; search walks all of them).
* `-h, --help` — Print help (see a summary with '-h')

## Roots

Which tree this invocation reads and writes.

* `--prefix <DIR>` — Unprivileged offset: ROOT/VDB/distfiles/build trees under DIR; config still from the host (use --root for a config offset).
* `--local [<DIR>]` — Unprivileged, standalone Gentoo-Prefix: own VDB/BROOT/config, not overlaid on the host (see --prefix for the overlay). Defaults to ~/.gentoo (EPREFIX=~/.gentoo) when no DIR is given (`--local=`).

  A bare `--local` takes the next word as DIR.
* `--config-root <PATH>` — Read config (profile, make.conf) from this root instead of `--root`
* `-T, --target <TARGET>` — Cross-build/setup for a crossdev target tuple, or a spec file describing one

  A value containing `/` or ending in `.toml` is a spec file, in the form `em crossdev --print-spec` prints; its `tuple` is the target.

  The single source for "which tuple" everywhere: `em crossdev --target T --init-target` sets T up; `em stages --target T --stage1` (or any plain atom build) resolves/installs into the target sysroot `<EROOT>/usr/<TUPLE>` — sugar for `--config-root <sysroot> --root <sysroot>`.

  Cross context (CHOST/CBUILD, `--root-deps=rdeps`) is read from the sysroot make.conf. One flag for both roles — `crossdev` no longer has its own `-t`/`--target`.
* `--root <PATH>` — Installation root (the offset an applet installs into / queries)

## Global options

* `--color <WHEN>` — \[default: auto\]
  * `auto`: Colour when stdout is a terminal.
  * `always`: Always colour.
  * `never`: Never colour.

## Examples

```
$ em search firefox
```
