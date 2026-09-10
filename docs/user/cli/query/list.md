<!-- @generated from em's usage spec; do not edit -->
# `em query list`

- **Usage:** `em query list [-I --installed] [PATTERN]…`

List installed/available packages matching a pattern

## Arguments
- **`[PATTERN]…`** — Glob or substring pattern(s); omit to list all packages

## Flags
- **`-I --installed`** — List only installed packages (from VDB), not available ones
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON

  **Framing:** `json`
- **`pretty`** (default) — emerge -p style pretend output

  **Framing:** `text`
- **`tree`** — cargo tree style dependency tree

  **Framing:** `text`
