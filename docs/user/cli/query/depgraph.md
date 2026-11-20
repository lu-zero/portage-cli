<!-- @generated from em's command line; do not edit -->
# em query depgraph

Display full dependency tree

## Usage

```
em query depgraph [OPTIONS] <ATOM>...
```

## Arguments

* `<ATOM>...` — Atom(s) to resolve and display the dependency tree for

## Options

* `--autosolve-use` — Let the solver choose USE flags to satisfy REQUIRED\_USE (Level C)
* `-e, --emptytree` — Treat every atom as not-yet-installed (emerge's `-e`/`--emptytree`)
* `-o, --onlydeps` — Only show dependencies, excluding the given atoms themselves from the tree
* `--with-bdeps` — With `--deep`, also follow the build-time deps of installed packages that are kept
* `--root-deps` — emerge's `--root-deps[=rdeps]`: only require RDEPEND (not DEPEND) to be satisfiable in the merge target.
* `-f, --format <FORMAT>` — Output format \[default: pretty\]
  * `pretty`: emerge -p style pretend output
  * `json`: Machine-parsable JSON
  * `tree`: cargo tree style dependency tree
* `-h, --help` — Print help (see a summary with '-h')

## Depgraph

How far to re-examine installed dependencies.

* `-D, --deep` — Re-examine transitive dependencies

  With `--update` (`-uD`), upgrades installed packages in the depgraph to the newest accepted in-slot version (emerge `-uD`). Alone, still bumps `:*` any-slot deps to the newest slot rather than keeping a satisfying installed slot.
* `-N, --newuse` — Reinstall installed packages when their planned USE or IUSE differs from the VDB (emerge `--newuse`)

  Applies to packages that appear in the depgraph; pairs with `--deep` for a full-tree USE recheck.
* `-U, --changed-use` — Like `--newuse`, but only rebuild when an *enabled* USE flag changed among flags present in both installed and current IUSE (ignore pure IUSE add/drop). Emerge's `--changed-use` / `-U`.

## Global options

* `--color <WHEN>` — \[default: auto\]
  * `auto`: Colour when stdout is a terminal.
  * `always`: Always colour.
  * `never`: Never colour.
* `--prefix <DIR>` — Unprivileged offset: ROOT/VDB/distfiles/build trees under DIR; config still from the host (use --root for a config offset).
* `--local [<DIR>]` — Unprivileged, standalone Gentoo-Prefix: own VDB/BROOT/config, not overlaid on the host (see --prefix for the overlay). Defaults to ~/.gentoo (EPREFIX=~/.gentoo) when no DIR is given (`--local=`).

  A bare `--local` takes the next word as DIR.
* `--config-root <PATH>` — Read config (profile, make.conf) from this root instead of `--root`
* `-T, --target <TARGET>` — Cross-build/setup for a crossdev target tuple, or a spec file describing one

  A value containing `/` or ending in `.toml` is a spec file, in the form `em crossdev --print-spec` prints; its `tuple` is the target.

  The single source for "which tuple" everywhere: `em crossdev --target T --init-target` sets T up; `em stages --target T --stage1` (or any plain atom build) resolves/installs into the target sysroot `<EROOT>/usr/<TUPLE>` — sugar for `--config-root <sysroot> --root <sysroot>`.

  Cross context (CHOST/CBUILD, `--root-deps=rdeps`) is read from the sysroot make.conf.
* `--root <PATH>` — Installation root (the offset an applet installs into / queries)
* `-v, --verbose...` — Increase verbosity: `-v` labels each build phase, `-vv`/`-vvv` add `em`'s own debug/trace logs (see also `RUST_LOG`).
* `--arch <ARCH>` — Target architecture for operations \[default: current system architecture\]
* `--repo <PATH>` — Pin search/query to a single repository

  When unset, repositories are auto-discovered from `repos.conf` (the main repo wins for single-repo applets; search walks all of them).
* `--vdb <PATH>` — Override VDB path (the default is the merge root's `/var/db/pkg`)

## Output formats

* `pretty` — emerge -p style pretend output. The default.
* `json` — Machine-parsable JSON. Framing: `json`.
* `tree` — cargo tree style dependency tree

## Examples

```
$ em query depgraph zlib
```
