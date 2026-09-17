<!-- @generated from em's usage spec; do not edit -->
# `em portageq has_version`

- **Usage:** `em portageq has_version <EROOT> <ATOM>`

Exit 0 if ATOM is installed under EROOT, 1 otherwise

## Arguments
- **`<EROOT>`**
- **`<ATOM>`**

## Flags
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`

## Exit Status

| Code | Meaning |
| ---- | ------- |
| `0` | query succeeded |
| `1` | no match / unset variable |
| `2` | invalid atom |
| `64` | <EROOT> is not a directory |
