<!-- @generated from em's usage spec; do not edit -->
# `em maint`

- **Usage:** `em maint [FLAGS] <SUBCOMMAND>`

System maintenance and health checks

## Global Flags
- **`--root <PATH>`** — Installation root (the offset an applet installs into / queries)
- **`-v --verbose`** — Increase verbosity: `-v` labels each build phase, `-vv`/`-vvv` add `em`'s own debug/trace logs (see also `RUST_LOG`).
- **`-q --quiet`** — Suppress non-error output
- **`--arch <ARCH>`** — Target architecture for operations (default: current system architecture)
- **`--repo <PATH>`** — Pin search/query to a single repository

  When unset, repositories are auto-discovered from `repos.conf` (the main repo wins for single-repo applets; search walks all of them).
- **`-p --pretend`** — Show what would be done without actually performing any actions

## Flags
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`

## Subcommands

- [`em maint binhost`](maint/binhost.md)
- [`em maint binpkg <SUBCOMMAND>`](maint/binpkg.md)
- [`em maint cleanconfmem`](maint/cleanconfmem.md)
- [`em maint cleanresume [-f --fix]`](maint/cleanresume.md)
- [`em maint logs [--fix] [-t --older-than <AGE>]`](maint/logs.md)
- [`em maint merges`](maint/merges.md)
- [`em maint movebin`](maint/movebin.md)
- [`em maint moveinst`](maint/moveinst.md)
- [`em maint regen-use [-o --output <PATH>]`](maint/regen-use.md)
- [`em maint revisions [REPO]…`](maint/revisions.md)
- [`em maint sync [REPOS]…`](maint/sync.md)
- [`em maint world [-f --fix]`](maint/world.md)
