<!-- @generated from em's usage spec; do not edit -->
# `em maint`

- **Usage:** `em maint [--root <PATH>] <SUBCOMMAND>`

System maintenance and health checks

## Roots
- **`--root <PATH>`** — Installation root (the offset an applet installs into / queries)

## Flags
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`

## Subcommands

- [`em maint binhost`](/maint/binhost.md)
- [`em maint binpkg <SUBCOMMAND>`](/maint/binpkg.md)
- [`em maint cleanconfmem`](/maint/cleanconfmem.md)
- [`em maint cleanresume [-f --fix]`](/maint/cleanresume.md)
- [`em maint logs [--fix] [-t --older-than <AGE>]`](/maint/logs.md)
- [`em maint merges`](/maint/merges.md)
- [`em maint movebin`](/maint/movebin.md)
- [`em maint moveinst`](/maint/moveinst.md)
- [`em maint regen-use [-o --output <PATH>]`](/maint/regen-use.md)
- [`em maint revisions [REPO]…`](/maint/revisions.md)
- [`em maint sync [REPOS]…`](/maint/sync.md)
- [`em maint world [-f --fix]`](/maint/world.md)
