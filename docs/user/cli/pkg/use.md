<!-- @generated from em's usage spec; do not edit -->
# `em pkg use`

- **Usage:** `em pkg use [FLAGS] <ATOM>`

Edit per-package USE flags in package.use

## Arguments
- **`<ATOM>`** — Package atom (e.g. sys-boot/grub or >=dev-libs/foo-1.0)

## Flags
- **`-E --add <FLAG>`** — Add flags (written verbatim, e.g. truetype) — euse calls this --enable/-E

  **Aliases:** `-a`
- **`-D --subtract <FLAG>`** — Subtract flags (written with leading '-', e.g. -themes) — euse calls this --disable/-D

  **Aliases:** `-s`
- **`-R --drop <FLAG>`** — Drop flags entirely (removes both flag and -flag forms) — euse calls this --remove/-R or --prune/-P

  **Aliases:** `-P`, `-d`
- **`-n --dry-run`** — Preview the resulting entry without writing package.use
- **`-i --info <FLAG>`** — Show descriptions for the given USE flags on this package (metadata.xml/use.local.desc first, falling back to the global profiles/use.desc)
- **`--path <FILE>`** — Target file inside package.use/ (default: `<cat>-<pkg>`)
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`
