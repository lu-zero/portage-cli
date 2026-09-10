<!-- @generated from em's usage spec; do not edit -->
# `em regen`

- **Usage:** `em regen [FLAGS] [REPOS]…`

Regenerate metadata cache

## Arguments
- **`[REPOS]…`** — Repo names or paths to regenerate (default: every repo except the main one, whose cache is normally maintained upstream)

## Flags
- **`-o --output <DIR>`** — Write cache files to this directory instead of metadata/md5-cache
- **`--repos-dir <DIR>`** — Directory containing master repositories
- **`-j --jobs <JOBS>`** — Number of parallel workers
- **`--dedup`** — Deduplicate top-level dep tokens before writing
- **`-h --help`** — Print help

## Activity

Where live progress is written.
- **`--activity-fd <N>`** — Write activity events as JSONL to file descriptor N (subprocess front-ends)

  Takes ownership of the FD.
- **`--activity-jsonl <PATH>`** — Append activity events as JSONL to PATH (not `-`; use `--activity-fd`)
- **`--emergelog`** — Dual-write Portage-compatible emerge.log lines (opt-in; qlop/genlop) Path defaults to `<merge-root>/var/log/emerge.log` (or `/var/log/emerge.log`).

## Roots
- **`--root <PATH>`** — Installation root (the offset an applet installs into / queries)

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`
