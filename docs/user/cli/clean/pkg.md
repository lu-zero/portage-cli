<!-- @generated from em's usage spec; do not edit -->
# `em clean pkg`

- **Usage:** `em clean pkg [FLAGS]`

Remove binary packages no ebuild references

## Flags
- **`-d --deep`** — Keep only what installed packages still reference, rather than everything any ebuild in the tree references
- **`-s --size-limit <SIZE>`** — Skip files smaller than this (e.g. `10M`, `1G`) — clears the big wins without touching a long tail of small files
- **`-t --time-limit <AGE>`** — Keep files modified more recently than this (e.g. `2weeks`, `30d`)
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`
