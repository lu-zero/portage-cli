<!-- @generated from em's usage spec; do not edit -->
# `em query belongs`

- **Usage:** `em query belongs <FILE>…`

Find which package owns a file

## Arguments
- **`<FILE>…`** — File path(s) to look up in the VDB contents records

## Flags
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON

  **Framing:** `json`
- **`pretty`** (default) — emerge -p style pretend output

  **Framing:** `text`
- **`tree`** — cargo tree style dependency tree

  **Framing:** `text`
