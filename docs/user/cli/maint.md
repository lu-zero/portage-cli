<!-- @generated from em's command line; do not edit -->
# em maint

`em maint` — system maintenance and health checks

## Usage

```
em maint [OPTIONS] <COMMAND>
```

## Commands

* [`binhost`](maint/binhost.md) — Generate binary package metadata index
* [`binpkg`](maint/binpkg.md) — Inspect/verify/prune local binary packages (em-only, no emaint equivalent)
* [`cleanconfmem`](maint/cleanconfmem.md) — No-op: em keeps no config-memory file to go stale
* [`cleanresume`](maint/cleanresume.md) — Discard saved resume lists
* [`logs`](maint/logs.md) — Prune the build.log files finished merges leave in the build tree
* [`merges`](maint/merges.md) — Unavailable: em keeps no failed-merge registry
* [`movebin`](maint/movebin.md) — Apply package moves to binary packages
* [`moveinst`](maint/moveinst.md) — Apply package moves to installed packages
* [`regen-use`](maint/regen-use.md) — Regenerate profiles/use.local.desc from metadata.xml
* [`revisions`](maint/revisions.md) — Purge repo revision history from repo\_revisions
* [`sync`](maint/sync.md) — Sync repositories (git, rsync)
* [`world`](maint/world.md) — Check (and optionally fix) problems in the world file

## Options

* `-v, --verbose...` — Increase verbosity: `-v` labels each build phase, `-vv`/`-vvv` add `em`'s own debug/trace logs (see also `RUST_LOG`).
* `-q, --quiet` — Suppress non-error output
* `--arch <ARCH>` — Target architecture for operations \[default: current system architecture\]
* `--repo <PATH>` — Pin search/query to a single repository

  When unset, repositories are auto-discovered from `repos.conf` (the main repo wins for single-repo applets; search walks all of them).
* `-p, --pretend` — Show what would be done without actually performing any actions
* `--vdb <PATH>` — Override VDB path (the default is the merge root's `/var/db/pkg`)
* `-h, --help` — Print help (see a summary with '-h')

## Roots

Which tree this invocation reads and writes.

* `--prefix <DIR>` — Unprivileged offset: ROOT/VDB/distfiles/build trees under DIR; config still from the host (use --root for a config offset).
* `--local [<DIR>]` — Unprivileged, standalone Gentoo-Prefix: own VDB/BROOT/config, not overlaid on the host (see --prefix for the overlay). Defaults to ~/.gentoo (EPREFIX=~/.gentoo) when no DIR is given (`--local=`).

  A bare `--local` takes the next word as DIR.
* `--config-root <PATH>` — Read config (profile, make.conf) from this root instead of `--root`
* `-T, --target <TARGET>` — Cross-build/setup for a crossdev target tuple, or a spec file describing one

  A value containing `/` or ending in `.toml` is a spec file, in the form `em crossdev --print-spec` prints; its `tuple` is the target.

  The single source for "which tuple" everywhere: `em crossdev --target T --init-target` sets T up; `em stages --target T --stage1` (or any plain atom build) resolves/installs into the target sysroot `<EROOT>/usr/<TUPLE>` — sugar for `--config-root <sysroot> --root <sysroot>`.

  Cross context (CHOST/CBUILD, `--root-deps=rdeps`) is read from the sysroot make.conf.
* `--root <PATH>` — Installation root (the offset an applet installs into / queries)
* `--sysroot <PATH>` — Build-against tree (`SYSROOT`).

  Also read from the `SYSROOT` environment variable when this flag is absent. Does not move `--root` or `--config-root`.

## Global options

* `--color <WHEN>` — \[default: auto\]
  * `auto`: Colour when stdout is a terminal.
  * `always`: Always colour.
  * `never`: Never colour.
