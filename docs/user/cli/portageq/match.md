<!-- @generated from em's usage spec; do not edit -->
# `em portageq match`

- **Usage:** `em portageq match <EROOT> <ATOM>`

List every installed CPV matching ATOM under EROOT, one per line

## Arguments
- **`<EROOT>`**
- **`<ATOM>`** — Empty string matches every installed package

## Flags
- **`-h --help`** — Print help

## Exit Status

| Code | Meaning |
| ---- | ------- |
| `0` | query succeeded |
| `1` | no match / unset variable |
| `2` | invalid atom |
| `64` | <EROOT> is not a directory |
