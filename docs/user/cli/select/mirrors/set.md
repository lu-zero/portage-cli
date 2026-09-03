<!-- @generated from em's usage spec; do not edit -->
# `em select mirrors set`

- **Usage:** `em select mirrors set [-c --country <COUNTRY>] [-r --region <REGION>] [URL]…`

Set `GENTOO_MIRRORS`

## Arguments
- **`[URL]…`** — Explicit mirror URLs to use

  If omitted, mirrors are picked from `--country`/`--region` instead.

## Flags
- **`-c --country <COUNTRY>`** — Use every mirror in this ISO country code
- **`-r --region <REGION>`** — Use every mirror in this region
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`
