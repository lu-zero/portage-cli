<!-- @generated from em's command line; do not edit -->
# em query

`em query` — query package information

**Effect:** read-only.

## Usage

```
em query [OPTIONS] <COMMAND>
```

## Commands

* [`belongs`](query/belongs.md) — Find which package owns a file
* [`check`](query/check.md) — Verify checksums of installed package
* [`depends`](query/depends.md) — List packages depending on an atom
* [`depgraph`](query/depgraph.md) — Display full dependency tree
* [`files`](query/files.md) — List files installed by a package
* [`has`](query/has.md) — List installed packages by a VDB field value
* [`hasuse`](query/hasuse.md) — List packages with a given USE flag in IUSE
* [`keywords`](query/keywords.md) — Display keyword status across architectures
* [`list`](query/list.md) — List installed/available packages matching a pattern
* [`meta`](query/meta.md) — Display package metadata (maintainer, homepage, etc.)
* [`size`](query/size.md) — Display total file size of a package
* [`uses`](query/uses.md) — Display USE flags for a package
* [`which`](query/which.md) — Print full path to the ebuild for a package

## Options

* `-v, --verbose...` — Increase verbosity: `-v` labels each build phase, `-vv`/`-vvv` add `em`'s own debug/trace logs (see also `RUST_LOG`).
* `--arch <ARCH>` — Target architecture for operations \[default: current system architecture\]
* `--repo <PATH>` — Pin search/query to a single repository

  When unset, repositories are auto-discovered from `repos.conf` (the main repo wins for single-repo applets; search walks all of them).
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

## Global options

* `--color <WHEN>` — \[default: auto\]
  * `auto`: Colour when stdout is a terminal.
  * `always`: Always colour.
  * `never`: Never colour.

## Examples

```
$ em query depgraph zlib
```

```
$ em query belongs /usr/bin/python
```

```
$ em query list -I
```
