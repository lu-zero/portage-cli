<!-- @generated from em's usage spec; do not edit -->
# `em query meta`

- **Usage:** `em query meta <ATOM>…`

Display package metadata (maintainer, homepage, etc.)

## Arguments
- **`<ATOM>…`** — Atom(s) whose metadata to display

## Flags
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON

  **Framing:** `json`
- **`pretty`** (default) — emerge -p style pretend output

  **Framing:** `text`
- **`tree`** — cargo tree style dependency tree

  **Framing:** `text`
