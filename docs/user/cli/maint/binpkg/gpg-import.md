<!-- @generated from em's usage spec; do not edit -->
# `em maint binpkg gpg-import`

- **Usage:** `em maint binpkg gpg-import <KEYFILE>`

Import an armored OpenPGP public key into the GPG verify keyring

## Arguments
- **`<KEYFILE>`** — Path to an armored public-key file (e.g. exported via `gpg --armor --export <key-id>`).

## Flags
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`
