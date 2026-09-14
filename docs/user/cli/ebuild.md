<!-- @generated from em's usage spec; do not edit -->
# `em ebuild`

- **Usage:** `em ebuild [FLAGS] <EBUILD_PATH> <PHASE>…`

Execute ebuild phases

## Arguments
- **`<EBUILD_PATH>`** — Path to the `.ebuild` file to execute
- **`<PHASE>…`** — Phase(s) to run in order (e.g. `compile`, `install`, `qmerge`)

## Flags
- **`-w --work-dir <DIR>`** — Override the build work directory (default: `/var/tmp/portage/<cat>/<pf>`)
- **`--root <PATH>`** — Installation root (the offset an applet installs into / queries)
- **`--repo <PATH>`** — Pin search/query to a single repository

  When unset, repositories are auto-discovered from `repos.conf` (the main repo wins for single-repo applets; search walks all of them).
- **`-p --pretend`** — Show what would be done without actually performing any actions
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`
