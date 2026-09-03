<!-- @generated from em's usage spec; do not edit -->
# `em ebuild`

- **Usage:** `em ebuild [-w --work-dir <DIR>] [--root <PATH>] <EBUILD_PATH> <PHASE>…`

Execute ebuild phases

## Arguments
- **`<EBUILD_PATH>`** — Path to the `.ebuild` file to execute
- **`<PHASE>…`** — Phase(s) to run in order (e.g. `compile`, `install`, `qmerge`)

## Flags
- **`-w --work-dir <DIR>`** — Override the build work directory (default: `/var/tmp/portage/<cat>/<pf>`)
- **`-h --help`** — Print help

## Roots
- **`--root <PATH>`** — Installation root (the offset an applet installs into / queries)

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`
