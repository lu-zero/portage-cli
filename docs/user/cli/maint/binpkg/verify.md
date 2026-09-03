<!-- @generated from em's usage spec; do not edit -->
# `em maint binpkg verify`

- **Usage:** `em maint binpkg verify [--fix] [--require-signature]`

Check each indexed binpkg's size/MD5/SHA1 against the file on disk

## Flags
- **`--fix`** — Quarantine corrupt containers (rename to `.corrupt`) and drop missing/corrupt entries from the index by regenerating it.
- **`--require-signature`** — Reject a container with no OpenPGP signature at all (matches FEATURES=binpkg-request-signature); with a verify keyring present (`em maint binpkg gpg-import`), signatures are always cryptographically checked regardless of this flag.
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`
