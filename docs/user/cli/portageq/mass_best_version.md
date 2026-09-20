<!-- @generated from em's usage spec; do not edit -->
# `em portageq mass_best_version`

- **Usage:** `em portageq mass_best_version <EROOT> [ATOM]…`

Print `atom:cpv` for each of several atoms' best installed version

## Arguments
- **`<EROOT>`**
- **`[ATOM]…`**

## Flags
- **`-h --help`** — Print help

## Exit Status

| Code | Meaning |
| ---- | ------- |
| `0` | query succeeded |
| `1` | no match / unset variable |
| `2` | invalid atom |
| `64` | <EROOT> is not a directory |
