<!-- @generated from em's usage spec; do not edit -->
# `em sync`

- **Usage:** `em sync [FLAGS] [REPOS]…`

Sync ebuild repositories from `repos.conf` (`git` and `rsync`)

With no names, syncs every entry with `auto-sync = yes` (Portage default) and a usable `sync-type`/`sync-uri`. Named repos are synced regardless of `auto-sync`.

Uses `git` / `rsync` (Portage parity) for the actual transfer.

Same command as `em maint sync` — this top-level form exists only because `sync` is common enough to deserve a short invocation, matching real Portage having both `emerge --sync` and `emaint sync`.

## Arguments
- **`[REPOS]…`** — Repo names from repos.conf (default: auto-sync enabled repos)

## Flags
- **`-v --verbose`** — Increase verbosity: `-v` labels each build phase, `-vv`/`-vvv` add `em`'s own debug/trace logs (see also `RUST_LOG`).
- **`-q --quiet`** — Suppress non-error output
- **`-p --pretend`** — Show what would be done without actually performing any actions
- **`-h --help`** — Print help

## Roots

Which tree this invocation reads and writes.
- **`--prefix <DIR>`** — Unprivileged offset: ROOT/VDB/distfiles/build trees under DIR; config still from the host (use --root for a config offset).
- **`--local [DIR]`** — Unprivileged, standalone Gentoo-Prefix: own VDB/BROOT/config, not overlaid on the host (see --prefix for the overlay). Defaults to ~/.gentoo (EPREFIX=~/.gentoo) when no DIR is given (`--local=`).

  A bare `--local` takes the next word as DIR.
- **`--config-root <PATH>`** — Read config (profile, make.conf) from this root instead of `--root`
- **`-T --target <TUPLE>`** — Cross-build/setup for a crossdev target tuple

  The single source for "which tuple" everywhere: `em crossdev --target T --init-target` sets T up; `em stages --target T --stage1` (or any plain atom build) resolves/installs into the target sysroot `<EROOT>/usr/<TUPLE>` — sugar for `--config-root <sysroot> --root <sysroot>`.

  Cross context (CHOST/CBUILD, `--root-deps=rdeps`) is read from the sysroot make.conf. One flag for both roles — `crossdev` no longer has its own `-t`/`--target`.
- **`--root <PATH>`** — Installation root (the offset an applet installs into / queries)

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`
