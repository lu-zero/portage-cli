<!-- @generated from em's usage spec; do not edit -->
# `em query has`

- **Usage:** `em query has <FIELD> [VALUE]`

List installed packages by a VDB field value

## Arguments
- **`<FIELD>`** — VDB field to match, e.g. `SLOT`, `USE`, `repository`
- **`[VALUE]`** — Value the field must contain; omit to list every package whose field is set at all

## Flags
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON

  **Framing:** `json`
- **`pretty`** (default) — emerge -p style pretend output

  **Framing:** `text`
- **`tree`** — cargo tree style dependency tree

  **Framing:** `text`
