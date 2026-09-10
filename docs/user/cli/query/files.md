<!-- @generated from em's usage spec; do not edit -->
# `em query files`

- **Usage:** `em query files <ATOM>…`

List files installed by a package

## Arguments
- **`<ATOM>…`** — Atom(s) whose installed file list to show

## Flags
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON

  **Framing:** `json`
- **`pretty`** (default) — emerge -p style pretend output

  **Framing:** `text`
- **`tree`** — cargo tree style dependency tree

  **Framing:** `text`
