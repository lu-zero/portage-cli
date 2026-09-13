<!-- @generated from em's usage spec; do not edit -->
# `em clean`

- **Usage:** `em clean [--root <PATH>] [-p --pretend] <SUBCOMMAND>`
- **Effect:** destructive — may delete or irreversibly overwrite

Clean distfiles and/or binary packages

## Global Flags
- **`-p --pretend`** — Show what would be done without actually performing any actions

## Roots
- **`--root <PATH>`** — Installation root (the offset an applet installs into / queries)

## Flags
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`

## Subcommands

- [`em clean all [FLAGS]`](clean/all.md)
- [`em clean dist [FLAGS]`](clean/dist.md)
- [`em clean pkg [FLAGS]`](clean/pkg.md)
