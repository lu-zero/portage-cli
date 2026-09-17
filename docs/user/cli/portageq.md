<!-- @generated from em's usage spec; do not edit -->
# `em portageq`

- **Usage:** `em portageq <SUBCOMMAND>`
- **Effect:** read-only

Query Portage internal variables and data

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

## Examples

```
em portageq has_version / sys-apps/portage
```

## Subcommands

- [`em portageq best_version <EROOT> <ATOM>`](portageq/best_version.md)
- [`em portageq has_version <EROOT> <ATOM>`](portageq/has_version.md)
- [`em portageq mass_best_version <EROOT> [ATOM]…`](portageq/mass_best_version.md)
- [`em portageq match <EROOT> <ATOM>`](portageq/match.md)
