<!-- @generated from em's usage spec; do not edit -->
# `em pkg keyword`

- **Usage:** `em pkg keyword [FLAGS] <ATOM>`

Edit per-package keywords in package.accept_keywords

## Arguments
- **`<ATOM>`** — Package atom (e.g. sys-boot/grub or >=dev-libs/foo-1.0)

## Flags
- **`-a --add <KW>`** — Add keyword tokens (e.g. `~amd64`, `-*`)
- **`-s --subtract <KW>`** — Subtract keyword tokens (written with leading '-', e.g. `-~amd64`)
- **`-d --drop <KW>`** — Drop keyword tokens entirely (removes both the token and its negated form)
- **`--path <FILE>`** — Target file inside package.accept_keywords/ (default: `<cat>-<pkg>`)
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`
