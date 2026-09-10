<!-- @generated from em's usage spec; do not edit -->
# `em query which`

- **Usage:** `em query which <ATOM>…`

Print full path to the ebuild for a package

## Arguments
- **`<ATOM>…`** — Atom(s) to resolve to an ebuild path

## Flags
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON

  **Framing:** `json`
- **`pretty`** (default) — emerge -p style pretend output

  **Framing:** `text`
- **`tree`** — cargo tree style dependency tree

  **Framing:** `text`
