<!-- @generated from em's usage spec; do not edit -->
# `em query check`

- **Usage:** `em query check <ATOM>…`

Verify checksums of installed package

## Arguments
- **`<ATOM>…`** — Installed package atom(s) to verify

## Flags
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON

  **Framing:** `json`
- **`pretty`** (default) — emerge -p style pretend output

  **Framing:** `text`
- **`tree`** — cargo tree style dependency tree

  **Framing:** `text`
