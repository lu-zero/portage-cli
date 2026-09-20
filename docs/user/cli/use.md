<!-- @generated from em's usage spec; do not edit -->
# `em use`

- **Usage:** `em use [FLAGS]`

Enable/disable/query USE flags in make.conf

## Flags
- **`-E --add <FLAG>`** — Add (enable) flags — euse calls this --enable/-E

  **Aliases:** `-a`
- **`-D --subtract <FLAG>`** — Subtract flags (written with leading '-', e.g. -themes) — euse calls this --disable/-D

  **Aliases:** `-s`
- **`-R --drop <FLAG>`** — Drop flags entirely (removes both flag and -flag forms) — euse calls this --remove/-R or --prune/-P

  **Aliases:** `-P`, `-d`
- **`-n --dry-run`** — Preview the resulting value without writing make.conf
- **`-e --expand <VAR>`** — Target a USE_EXPAND variable (e.g. VIDEO_CARDS) instead of USE — -a/-s/-d then edit that variable's value the same way
- **`-L --list-expand`** — List every USE_EXPAND variable known to the active profile, each with its current make.conf value
- **`-i --info <FLAG>`** — Show descriptions for the given USE flags (profiles/use.desc and use.local.desc, searching both unless -g/-l restricts it). With no flags given, lists every flag in scope
- **`-g --global`** — Restrict -i to global flags only (profiles/use.desc)
- **`-l --local-desc`** — Restrict -i to per-package local flags only (profiles/use.local.desc, searched across every package — see `em query uses <atom>` for a single package's flags instead)
- **`--make-conf <PATH>`** — Path to make.conf (default: resolved like other config commands, following --config-root/--local/--prefix)
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
