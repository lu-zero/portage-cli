<!-- @generated from em's usage spec; do not edit -->
# `em etc`

- **Usage:** `em etc [FLAGS] [SUBCOMMAND]`

Reconcile pending config files (etc-update / dispatch-conf)

## Global Flags
- **`-q --quiet`** — Suppress non-error output
- **`-p --pretend`** — Show what would be done without actually performing any actions

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
- **`--use-new`** — Install every pending file over its target
- **`--use-old`** — Discard every pending file, keeping what is installed
- **`--auto`** — Resolve only what needs no decision: identical files, and those differing from the installed one in comments or whitespace alone
- **`-h --help`** — Print help

## Subcommands

- [`em etc diff [PATH]`](etc/diff.md)
- [`em etc merge`](etc/merge.md)
