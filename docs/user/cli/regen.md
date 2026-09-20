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
- **`-v --verbose`** — Increase verbosity: `-v` labels each build phase, `-vv`/`-vvv` add `em`'s own debug/trace logs (see also `RUST_LOG`).
- **`-q --quiet`** — Suppress non-error output
- **`-h --help`** — Print help

## Activity

Where live progress is written.
- **`--activity-fd <N>`** — Write activity events as JSONL to file descriptor N (subprocess front-ends)

  Takes ownership of the FD.
- **`--activity-jsonl <PATH>`** — Append activity events as JSONL to PATH (not `-`; use `--activity-fd`)
- **`--emergelog`** — Dual-write Portage-compatible emerge.log lines (opt-in; qlop/genlop) Path defaults to `<merge-root>/var/log/emerge.log` (or `/var/log/emerge.log`).

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
