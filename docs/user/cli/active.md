<!-- @generated from em's usage spec; do not edit -->
# `em active`

- **Usage:** `em active [FLAGS] [SUBCOMMAND]`

Register a default `--prefix` / `--local` so bare `em <pkg>` picks it up (dogfooding)

Explicit `--prefix`/`--local`/`--root` still win. State is stored under `$XDG_STATE_HOME/em/active`.

## Roots
- **`--prefix <DIR>`** — Unprivileged offset: ROOT/VDB/distfiles/build trees under DIR; config still from the host (use --root for a config offset).
- **`--local [DIR]`** — Unprivileged, standalone Gentoo-Prefix: own VDB/BROOT/config, not overlaid on the host (see --prefix for the overlay). Defaults to ~/.gentoo (EPREFIX=~/.gentoo) when no DIR is given (`--local=`).

  A bare `--local` takes the next word as DIR.
- **`--config-root <PATH>`** — Read config (profile, make.conf) from this root instead of `--root`
- **`-T --target <TUPLE>`** — Cross-build/setup for a crossdev target tuple

  The single source for "which tuple" everywhere: `em crossdev --target T --init-target` sets T up; `em stages --target T --stage1` (or any plain atom build) resolves/installs into the target sysroot `<EROOT>/usr/<TUPLE>` — sugar for `--config-root <sysroot> --root <sysroot>`.

  Cross context (CHOST/CBUILD, `--root-deps=rdeps`) is read from the sysroot make.conf. One flag for both roles — `crossdev` no longer has its own `-t`/`--target`.

## Flags
- **`-h --help`** — Print help

## Examples

**Register ~/.gentoo**

```
em active set --local=
```

**Flags may precede set**

```
em active --local= set
```

## Subcommands

- [`em active add [NAME]`](active/add.md)
- [`em active clear [--all]`](active/clear.md)
- [`em active env`](active/env.md)
- [`em active list`](active/list.md)
- [`em active remove <REF>`](active/remove.md)
- [`em active set [REF]`](active/set.md)
- [`em active show`](active/show.md)
