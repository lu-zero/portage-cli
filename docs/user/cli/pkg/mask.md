<!-- @generated from em's usage spec; do not edit -->
# `em pkg mask`

- **Usage:** `em pkg mask [FLAGS] <ATOM>`

Add/remove a package from package.mask

## Arguments
- **`<ATOM>`** — Package atom (e.g. sys-boot/grub or >=dev-libs/foo-1.0)

## Flags
- **`-a --add`** — Add the atom to package.mask
- **`-d --drop`** — Remove the atom from package.mask
- **`--path <FILE>`** — Target file inside package.mask/ (default: `<cat>-<pkg>`)
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`
