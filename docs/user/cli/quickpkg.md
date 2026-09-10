<!-- @generated from em's usage spec; do not edit -->
# `em quickpkg`

- **Usage:** `em quickpkg [FLAGS] <ATOMS>…`

Create binary packages from installed files

## Arguments
- **`<ATOMS>…`** — Atoms, package sets (`@system`), or VDB paths (`/var/db/pkg/cat/pf`)

## Flags
- **`--include-config`** — Include CONFIG_PROTECT files
- **`--include-unmodified-config`** — Include unmodified CONFIG_PROTECT files
- **`-h --help`** — Print help

## Roots
- **`--root <PATH>`** — Installation root (the offset an applet installs into / queries)

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`
