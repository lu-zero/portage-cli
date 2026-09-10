<!-- @generated from em's usage spec; do not edit -->
# `em maint binpkg`

- **Usage:** `em maint binpkg <SUBCOMMAND>`

Inspect/verify/prune local binary packages (em-only, no emaint equivalent)

## Flags
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`

## Subcommands

- [`em maint binpkg fingerprint [--full] [--host]`](binpkg/fingerprint.md)
- [`em maint binpkg gpg-import <KEYFILE>`](binpkg/gpg-import.md)
- [`em maint binpkg list`](binpkg/list.md)
- [`em maint binpkg prune [--dry-run]`](binpkg/prune.md)
- [`em maint binpkg verify [--fix] [--require-signature]`](binpkg/verify.md)
