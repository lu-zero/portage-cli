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
- **`-h --help`** — Print help

## Roots
- **`--root <PATH>`** — Installation root (the offset an applet installs into / queries)

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`
