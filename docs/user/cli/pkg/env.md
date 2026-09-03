<!-- @generated from em's usage spec; do not edit -->
# `em pkg env`

- **Usage:** `em pkg env [FLAGS] <ATOM>`

Edit per-package env files in package.env

## Arguments
- **`<ATOM>`** — Package atom (e.g. sys-boot/grub or >=dev-libs/foo-1.0)

## Flags
- **`-a --add <ENVFILE>`** — Add env file name(s) (from `/etc/portage/env/`) to apply to this package
- **`-d --drop <ENVFILE>`** — Drop env file name(s) from this package's entry
- **`--path <FILE>`** — Target file inside package.env/ (default: `<cat>-<pkg>`)
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`
