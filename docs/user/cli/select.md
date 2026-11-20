<!-- @generated from em's command line; do not edit -->
# em select

`em select` — native config selectors (profile, repos)

## Usage

```
em select [OPTIONS] <COMMAND>
```

## Commands

* [`profile`](select/profile.md) — Select the system/sysroot profile (cross-aware)
* [`repository`](select/repository.md), `repos` — Manage local repositories (overlays)
* [`compiler`](select/compiler.md), `gcc` — Select the active compiler profile (gcc-config/eselect gcc workalike)
* [`binutils`](select/binutils.md) — Select the active binutils profile (binutils-config/eselect binutils workalike)
* [`linker`](select/linker.md) — Select the active linker profile
* [`clang`](select/clang.md) — Select the active LLVM/clang slot
* [`pkgconf`](select/pkgconf.md) — Select the pkg-config backend and create the \<CTARGET\>-pkg-config wrapper
* [`mirrors`](select/mirrors.md), `mirror` — Manage Gentoo distfile mirrors (mirrorselect workalike)
* [`news`](select/news.md) — Read/manage GLEP 42 news items (eselect news workalike)
* [`glsa`](select/glsa.md) — Check/fix Gentoo Linux Security Advisories (glsa-check workalike)

## Options

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

## Global options

* `--color <WHEN>` — \[default: auto\]
  * `auto`: Colour when stdout is a terminal.
  * `always`: Always colour.
  * `never`: Never colour.
