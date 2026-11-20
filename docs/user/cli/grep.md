<!-- @generated from em's command line; do not edit -->
# em grep

`em grep` — search inside ebuilds and eclasses (qgrep workalike)

**Effect:** read-only.

## Usage

```
em grep [OPTIONS] <PATTERN> [TARGETS]...
```

## Arguments

* `<PATTERN>` — Pattern to search for (a literal substring unless -e/-x)
* `[TARGETS]...` — Restrict the search to these atoms (`cat/pkg`, `cat/`, a bare package name, or a full dependency atom like `>=cat/pkg-1.2`); default: every package in scope

## Options

* `-I, --invert-match` — Invert the match: print lines that do NOT match
* `-i, --ignore-case` — Case-insensitive match
* `-N, --atom-name` — Label matches with `cat/pkg-ver` instead of the ebuild's path
* `-c, --count` — Print each matching file's label and its match count instead of the matching lines
* `-l, --list` — Print only the labels of files that match
* `-L, --invert-list` — Print only the labels of files that do NOT match
* `-e, -x, --regexp` — Treat PATTERN (and -S) as a regular expression instead of a literal substring
* `-J, --installed` — Search installed packages' own ebuild copies (under --root/--prefix/--local) instead of the repo tree
* `-E, --eclass` — Search eclasses instead of ebuilds (targets are ignored)
* `-s, --skip-comments` — Skip lines that are comments (first non-blank character is `#`)
* `-R, --show-repo` — Label matches with `cat/pkg-ver::repo` instead of a path
* `-S, --skip <PATTERN>` — Also skip lines matching this pattern (same literal/regex/case mode as PATTERN)
* `-B, --before <N>` — Print N lines of context before each match
* `-A, --after <N>` — Print N lines of context after each match
* `-q, --no-filename` — Print bare matching lines with no label
* `--repo <PATH>` — Pin search/query to a single repository

  When unset, repositories are auto-discovered from `repos.conf` (the main repo wins for single-repo applets; search walks all of them).
* `-v, --verbose...` — Increase verbosity: `-v` labels each build phase, `-vv`/`-vvv` add `em`'s own debug/trace logs (see also `RUST_LOG`).
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
$ em grep PYTHON_COMPAT dev-python/setuptools
```
