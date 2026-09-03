<!-- @generated from em's usage spec; do not edit -->
# `em etc`

- **Usage:** `em etc [FLAGS] <SUBCOMMAND>`

Reconcile pending config files (etc-update / dispatch-conf)

## Roots
- **`--root <PATH>`** — Installation root (the offset an applet installs into / queries)

## Flags
- **`--use-new`** — Install every pending file over its target
- **`--use-old`** — Discard every pending file, keeping what is installed
- **`--auto`** — Resolve only what needs no decision: identical files, and those differing from the installed one in comments or whitespace alone
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`

## Subcommands

- [`em etc diff [PATH]`](/etc/diff.md)
- [`em etc merge`](/etc/merge.md)
