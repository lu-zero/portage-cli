<!-- @generated from em's usage spec; do not edit -->
# `em pkg`

- **Usage:** `em pkg [--root <PATH>] <SUBCOMMAND>`

Edit per-package configuration (package.use, .keywords, .mask, .env)

## Roots
- **`--root <PATH>`** — Installation root (the offset an applet installs into / queries)

## Flags
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`

## Subcommands

- [`em pkg env [FLAGS] <ATOM>`](pkg/env.md)
- [`em pkg keyword [FLAGS] <ATOM>`](pkg/keyword.md)
- [`em pkg mask [FLAGS] <ATOM>`](pkg/mask.md)
- [`em pkg use [FLAGS] <ATOM>`](pkg/use.md)
