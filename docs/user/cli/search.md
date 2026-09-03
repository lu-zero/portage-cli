<!-- @generated from em's usage spec; do not edit -->
# `em search`

- **Usage:** `em search [FLAGS] [PATTERN]`
- **Effect:** read-only

Search package names and descriptions

## Arguments
- **`[PATTERN]`** — Pattern to search (required unless --all)

## Flags
- **`-a --all`** — List all packages (no pattern required)
- **`-S --desc`** — Search package descriptions instead of names
- **`-N --name-only`** — Show only package name, no description
- **`-H --homepage`** — Show homepage instead of description
- **`-h --help`** — Print help

## Roots
- **`--root <PATH>`** — Installation root (the offset an applet installs into / queries)

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`

## Examples

```
em search firefox
```
