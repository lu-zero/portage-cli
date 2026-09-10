<!-- @generated from em's usage spec; do not edit -->
# `em query hasuse`

- **Usage:** `em query hasuse <FLAG>…`

List packages with a given USE flag in IUSE

## Arguments
- **`<FLAG>…`** — USE flag name(s) to search for in IUSE

## Flags
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON

  **Framing:** `json`
- **`pretty`** (default) — emerge -p style pretend output

  **Framing:** `text`
- **`tree`** — cargo tree style dependency tree

  **Framing:** `text`
