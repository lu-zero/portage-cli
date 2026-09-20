<!-- @generated from em's usage spec; do not edit -->
# `em query`

- **Usage:** `em query [FLAGS] <SUBCOMMAND>`
- **Effect:** read-only

Query package information

## Global Flags
- **`-v --verbose`** — Increase verbosity: `-v` labels each build phase, `-vv`/`-vvv` add `em`'s own debug/trace logs (see also `RUST_LOG`).
- **`--arch <ARCH>`** — Target architecture for operations (default: current system architecture)
- **`--repo <PATH>`** — Pin search/query to a single repository

  When unset, repositories are auto-discovered from `repos.conf` (the main repo wins for single-repo applets; search walks all of them).
- **`--vdb <PATH>`** — Override VDB path (default: $ROOT/var/db/pkg)

## Roots
- **`--prefix <DIR>`** — Unprivileged offset: ROOT/VDB/distfiles/build trees under DIR; config still from the host (use --root for a config offset).
- **`--local [DIR]`** — Unprivileged, standalone Gentoo-Prefix: own VDB/BROOT/config, not overlaid on the host (see --prefix for the overlay). Defaults to ~/.gentoo (EPREFIX=~/.gentoo) when no DIR is given (`--local=`).

  A bare `--local` takes the next word as DIR.
- **`--config-root <PATH>`** — Read config (profile, make.conf) from this root instead of `--root`
- **`-T --target <TUPLE>`** — Cross-build/setup for a crossdev target tuple

  The single source for "which tuple" everywhere: `em crossdev --target T --init-target` sets T up; `em stages --target T --stage1` (or any plain atom build) resolves/installs into the target sysroot `<EROOT>/usr/<TUPLE>` — sugar for `--config-root <sysroot> --root <sysroot>`.

  Cross context (CHOST/CBUILD, `--root-deps=rdeps`) is read from the sysroot make.conf. One flag for both roles — `crossdev` no longer has its own `-t`/`--target`.
- **`--root <PATH>`** — Installation root (the offset an applet installs into / queries)

## Flags
- **`-h --help`** — Print help

## Examples

```
em query depgraph zlib
```

```
em query belongs /usr/bin/python
```

```
em query list -I
```

## Subcommands

- [`em query belongs <FILE>…`](query/belongs.md)
- [`em query check [--format <FORMAT>] <ATOM>…`](query/check.md)
- [`em query depends <ATOM>…`](query/depends.md)
- [`em query depgraph [FLAGS] <ATOM>…`](query/depgraph.md)
- [`em query files <ATOM>…`](query/files.md)
- [`em query has <FIELD> [VALUE]`](query/has.md)
- [`em query hasuse <FLAG>…`](query/hasuse.md)
- [`em query keywords [--format <FORMAT>] <ATOM>…`](query/keywords.md)
- [`em query list [-I --installed] [PATTERN]…`](query/list.md)
- [`em query meta [--format <FORMAT>] <ATOM>…`](query/meta.md)
- [`em query size <ATOM>…`](query/size.md)
- [`em query uses <ATOM>…`](query/uses.md)
- [`em query which <ATOM>…`](query/which.md)
