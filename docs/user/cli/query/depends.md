<!-- @generated from em's usage spec; do not edit -->
# `em query depends`

- **Usage:** `em query depends <ATOM>…`

List packages depending on an atom

## Arguments
- **`<ATOM>…`** — Atom(s) whose dependents to list

## Flags
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON

  **Framing:** `json`
- **`pretty`** (default) — emerge -p style pretend output

  **Framing:** `text`
- **`tree`** — cargo tree style dependency tree

  **Framing:** `text`
