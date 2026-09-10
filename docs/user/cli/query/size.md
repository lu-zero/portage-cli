<!-- @generated from em's usage spec; do not edit -->
# `em query size`

- **Usage:** `em query size <ATOM>…`

Display total file size of a package

## Arguments
- **`<ATOM>…`** — Atom(s) whose installed file size to sum

## Flags
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON

  **Framing:** `json`
- **`pretty`** (default) — emerge -p style pretend output

  **Framing:** `text`
- **`tree`** — cargo tree style dependency tree

  **Framing:** `text`
