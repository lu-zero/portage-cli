<!-- @generated from em's command line; do not edit -->
# em active set

Register the invocation's `--prefix` or `--local` as the active context

Without arguments, reads from `--prefix`/`--local` flags:
  `em active set --prefix /home/me/prefix`
  `em active set --local=`           (default `~/.gentoo`)
  `em active --local= set`           (same; flags may precede set)
  `em active set --local /other`

With a reference argument, activates an existing entry:
  `em active set my-name`     # by name
  `em active set 0`           # by index
  `em active set /path/to/dir` # by exact path

## Usage

```
em active set [OPTIONS] [REF]
```

## Arguments

* `[REF]` — Reference to an existing entry (name, index, or path) to activate

  If not provided, creates a new entry from --prefix/--local flags.

## Options

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
