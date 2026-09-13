<!-- @generated from em's usage spec; do not edit -->
# `em sync`

- **Usage:** `em sync [FLAGS] [REPOS]…`

Sync ebuild repositories from `repos.conf` (`git` and `rsync`)

With no names, syncs every entry with `auto-sync = yes` (Portage default) and a usable `sync-type`/`sync-uri`. Named repos are synced regardless of `auto-sync`.

Default backends shell out to `git` / `rsync` (Portage parity). Build with `--features sync-gix` for the experimental pure-gix git path.

Identical implementation to `em maint sync` — this top-level form exists only because `sync` is common enough to deserve a short invocation, matching real Portage having both `emerge --sync` and `emaint sync`.

## Arguments
- **`[REPOS]…`** — Repo names from repos.conf (default: auto-sync enabled repos)

## Flags
- **`-v --verbose`** — Increase verbosity: `-v` labels each build phase, `-vv`/`-vvv` add `em`'s own debug/trace logs (see also `RUST_LOG`).
- **`-q --quiet`** — Suppress non-error output
- **`-p --pretend`** — Show what would be done without actually performing any actions
- **`-h --help`** — Print help

## Roots
- **`--root <PATH>`** — Installation root (the offset an applet installs into / queries)

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`
