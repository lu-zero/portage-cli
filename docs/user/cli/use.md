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
- **`--root <PATH>`** — Installation root (the offset an applet installs into / queries)

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`
