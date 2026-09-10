<!-- @generated from em's usage spec; do not edit -->
# `em active add`

- **Usage:** `em active add [NAME]`

Add a new entry without activating it

Examples:
  `em active add --prefix /home/me/prefix my-prefix`
  `em active add --local /home/me/.gentoo my-gentoo`
  `em active add --local=`  # adds ~/.gentoo with auto-generated name

## Arguments
- **`[NAME]`** — Optional name for the entry. If not provided, uses path basename

## Flags
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`
