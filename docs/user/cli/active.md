<!-- @generated from em's command line; do not edit -->
# em active

`em active` — register a default `--prefix`/`--local` for bare invocations

## Usage

```
em active [OPTIONS] [COMMAND]
```

## Commands

* [`show`](active/show.md) — Show the registered active prefix/local
* [`set`](active/set.md) — Register --prefix/--local as active or activate an existing entry
* [`clear`](active/clear.md) — Clear the active context (or all entries with --all)
* [`env`](active/env.md) — Print shell exports for the active context
* [`list`](active/list.md) — List all registered prefix/local entries
* [`add`](active/add.md) — Add a new prefix/local entry
* [`remove`](active/remove.md) — Remove a registered entry

## Options

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

  Cross context (CHOST/CBUILD, `--root-deps=rdeps`) is read from the sysroot make.conf.

## Global options

* `--color <WHEN>` — \[default: auto\]
  * `auto`: Colour when stdout is a terminal.
  * `always`: Always colour.
  * `never`: Never colour.

## Examples

**Register ~/.gentoo**

```
$ em active set --local=
```

**Flags may precede set**

```
$ em active --local= set
```
