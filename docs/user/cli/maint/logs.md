<!-- @generated from em's usage spec; do not edit -->
# `em maint logs`

- **Usage:** `em maint logs [--fix] [-t --older-than <AGE>]`

Prune the build.log files finished merges leave in the build tree

## Flags
- **`--fix`** — Remove them; without this the logs are only listed
- **`-t --older-than <AGE>`** — Only consider logs at least this old (e.g. `30d`, `2weeks`)
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`
