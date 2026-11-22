<!-- @generated from em's command line; do not edit -->
# em pkg mask

Add/remove a package from package.mask

## Usage

```
em pkg mask [OPTIONS] <ATOM>
```

## Arguments

* `<ATOM>` — Package atom (e.g. sys-boot/grub or \>=dev-libs/foo-1.0)

## Options

* `-a, --add` — Add the atom to package.mask
* `-d, --drop` — Remove the atom from package.mask
* `--path <FILE>` — Target file inside package.mask/ (default: `<cat>-<pkg>`)
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
* `--arch <ARCH>` — Target architecture for operations \[default: current system architecture\]
