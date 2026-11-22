<!-- @generated from em's command line; do not edit -->
# em clean dist

Remove distfiles no ebuild references

## Usage

```
em clean dist [OPTIONS]
```

## Options

* `-d, --deep` — Keep only what installed packages still reference, rather than everything any ebuild in the tree references
* `-s, --size-limit <SIZE>` — Skip files smaller than this (e.g. `10M`, `1G`) — clears the big wins without touching a long tail of small files
* `-t, --time-limit <AGE>` — Keep files modified more recently than this (e.g. `2weeks`, `30d`)
* `-h, --help` — Print help (see a summary with '-h')

## Global options

* `--color <WHEN>` — \[default: auto\]
  * `auto`: Colour when stdout is a terminal.
  * `always`: Always colour.
  * `never`: Never colour.
* `--prefix <DIR>` — Unprivileged offset: ROOT/VDB/distfiles/build trees under DIR; config still from the host (use --root for a config offset).
* `--local [<DIR>]` — Unprivileged, standalone Gentoo-Prefix: own VDB/BROOT/config, not overlaid on the host (see --prefix for the overlay). Defaults to ~/.gentoo (EPREFIX=~/.gentoo) when no DIR is given (`--local=`).

  A bare `--local` takes the next word as DIR.
* `--config-root <PATH>` — Read config (profile, make.conf) from this root instead of `--root`
* `-T, --target <TARGET>` — Cross-build/setup for a crossdev target tuple, or a spec file describing one

  A value containing `/` or ending in `.toml` is a spec file, in the form `em crossdev --print-spec` prints; its `tuple` is the target.

  The single source for "which tuple" everywhere: `em crossdev --target T --init-target` sets T up; `em stages --target T --stage1` (or any plain atom build) resolves/installs into the target sysroot `<EROOT>/usr/<TUPLE>` — sugar for `--config-root <sysroot> --root <sysroot>`.

  Cross context (CHOST/CBUILD, `--root-deps=rdeps`) is read from the sysroot make.conf.
* `--root <PATH>` — Installation root (the offset an applet installs into / queries)
* `--sysroot <PATH>` — Build-against tree (`SYSROOT`).

  Also read from the `SYSROOT` environment variable when this flag is absent. Does not move `--root` or `--config-root`.
* `-p, --pretend` — Show what would be done without actually performing any actions
