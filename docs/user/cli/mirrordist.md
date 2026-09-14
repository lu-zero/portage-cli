<!-- @generated from em's usage spec; do not edit -->
# `em mirrordist`

- **Usage:** `em mirrordist <FLAGS> [REPO]`
- **Effect:** modifies state

Walks every ebuild in a repository, fetches every distfile its SRC_URI references (all versions, all USE branches), and verifies each against the repo Manifest — the server side of a Gentoo mirror.

Not to be confused with `em select mirrors`, which chooses which mirrors *this* machine fetches from.

Requires an up-to-date metadata cache: run `em regen <repo>` first for overlays.

## Arguments
- **`[REPO]`** — repos.conf name or path

  Defaults to the main repo (opposite default from `em regen`, which excludes it).

## Flags
- **`--repos-dir <DIR>`** — Directory containing master repositories
- **`--distfiles <DIR>`** — Distfiles directory to populate
- **`-j --jobs <JOBS>`** — Concurrent downloads
- **`--delete`** — Delete distfiles no longer referenced by any ebuild

  **Effect:** destructive — may delete or irreversibly overwrite
- **`--deletion-delay <DURATION>`** — Grace period before an orphaned file is deleted (e.g. `7d`, `72h`)

  **Default:** `7d`
- **`--deletion-db <FILE>`** — Deletion-grace state file (default: `$XDG_STATE_HOME/em/mirrordist/<repo>-*.json`)
- **`--success-log <FILE>`** — Tab-delimited log of fetched files (appended)
- **`--failure-log <FILE>`** — Tab-delimited log of fetch failures (appended)
- **`--scheduled-deletion-log <FILE>`** — Report of files scheduled for deletion, grouped by date (rewritten)
- **`--whitelist-from <FILE>`** — File(s) listing distfile names --delete must never remove (one name per line, `#`-comments ignored).
- **`--verify-existing-digest`** — Re-hash already-present files instead of trusting their size
- **`--gentoo-mirrors-fallback`** — Also try GENTOO_MIRRORS after the ebuild's own URIs (real emirrordist never does this — off by default).
- **`--delete-allow-incomplete`** — Allow --delete even when some ebuilds had no metadata cache entry
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
