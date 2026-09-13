<!-- @generated from em's usage spec; do not edit -->
# `em select`

- **Usage:** `em select [FLAGS] <SUBCOMMAND>`

Native config selectors (profile, repos) — eselect-like

## Global Flags
- **`--arch <ARCH>`** — Target architecture for operations (default: current system architecture)
- **`--repo <PATH>`** — Pin search/query to a single repository

  When unset, repositories are auto-discovered from `repos.conf` (the main repo wins for single-repo applets; search walks all of them).
- **`-p --pretend`** — Show what would be done without actually performing any actions

## Roots
- **`--root <PATH>`** — Installation root (the offset an applet installs into / queries)

## Flags
- **`-h --help`** — Print help

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`

## Subcommands

- [`em select binutils <SUBCOMMAND>`](select/binutils.md)
- [`em select clang <SUBCOMMAND>`](select/clang.md)
- [`em select compiler <SUBCOMMAND>`](select/compiler.md)
- [`em select glsa [SUBCOMMAND]`](select/glsa.md)
- [`em select linker <SUBCOMMAND>`](select/linker.md)
- [`em select mirrors <SUBCOMMAND>`](select/mirrors.md)
- [`em select news [SUBCOMMAND]`](select/news.md)
- [`em select pkgconf <SUBCOMMAND>`](select/pkgconf.md)
- [`em select profile <SUBCOMMAND>`](select/profile.md)
- [`em select repository <SUBCOMMAND>`](select/repository.md)
