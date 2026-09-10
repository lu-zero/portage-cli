<!-- @generated from em's usage spec; do not edit -->
# `em maint binpkg fingerprint`

- **Usage:** `em maint binpkg fingerprint [--full] [--host]`

Print the build-env key for the current roots' make.conf flags

## Flags
- **`--full`** — Print the full key (space-joined sokgi hashes) instead of the short path-safe slug.
- **`--host`** — Fingerprint the host (BROOT) config instead of the target roots (only differs under --target).
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`
