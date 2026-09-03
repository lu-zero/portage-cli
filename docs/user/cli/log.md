<!-- @generated from em's usage spec; do not edit -->
# `em log`

- **Usage:** `em log [--root <PATH>] <SUBCOMMAND>`
- **Effect:** read-only

Analyze emerge.log

## Roots
- **`--root <PATH>`** — Installation root (the offset an applet installs into / queries)

## Flags
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`

## Subcommands

- [`em log current`](/log/current.md)
- [`em log list [LIMIT]`](/log/list.md)
- [`em log predict`](/log/predict.md)
- [`em log time [ATOM]`](/log/time.md)
