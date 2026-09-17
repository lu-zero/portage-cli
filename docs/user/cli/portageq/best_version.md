<!-- @generated from em's usage spec; do not edit -->
# `em portageq best_version`

- **Usage:** `em portageq best_version <EROOT> <ATOM>`

Print the highest installed version matching ATOM under EROOT

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
