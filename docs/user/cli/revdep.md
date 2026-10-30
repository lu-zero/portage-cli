<!-- @generated from em's command line; do not edit -->
# em revdep

`em revdep` — rebuild packages with broken shared library deps

## Usage

```
em revdep [OPTIONS]
```

## Options

* `-L, --library <NAME>` — Only consider consumers of libraries whose soname contains NAME
* `-p, --pretend` — Show what would be done without actually performing any actions
* `--vdb <PATH>` — Override VDB path (default: $ROOT/var/db/pkg)
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

  Cross context (CHOST/CBUILD, `--root-deps=rdeps`) is read from the sysroot make.conf. One flag for both roles — `crossdev` no longer has its own `-t`/`--target`.
* `--root <PATH>` — Installation root (the offset an applet installs into / queries)

## Merge

How the solver and build scheduler behave.

* `-a, --ask` — Ask for confirmation before performing actions
* `-u, --update` — Update installed packages to newest available versions
* `--autounmask-write` — Write required USE changes to /etc/portage/package.use/
* `-1, --oneshot` — Build and install packages but do not add them to the world file
* `-f, --fetchonly` — Only fetch distfiles, do not build or install
* `-F, --fetch-all-uri` — Instead of building, just fetch every SRC\_URI file (regardless of USE setting) for the resolved packages.
* `-b, --buildpkg` — Build binary packages for all merged packages
* `-B, --buildpkgonly` — Build binary packages without merging/installing them

  All build-time dependencies must already be satisfied on the system -- this does not resolve or install anything to make that true.
* `-k, --usepkg` — Use binary packages if available, otherwise fall back to source
* `-K, --usepkgonly` — Only use binary packages, fail if none available
* `-g, --getbinpkg` — Fetch binary packages for all requested packages
* `-G, --getbinpkgonly` — Only fetch binary packages, do not install
* `-e, --emptytree` — Treat every atom as not-yet-installed, rebuilding the whole dependency tree from scratch rather than only what is missing or outdated.
* `-t, --tree` — Show the dependency tree, indenting each package under the one that pulled it in, before merging.
* `--json` — Emit the depgraph as machine-parsable JSON instead of pretend text

  Takes precedence over `--tree`. Works with `-p` (including `-e`).
* `-o, --onlydeps` — Only merge dependencies, not the specified packages themselves
* `-n, --noreplace` — Do not replace installed packages that are already the same version
* `--selective[=<y|n>]` — Leave a named package alone when an installed version satisfies it, like `-n`

  `-u`, `-N` and `-U` imply it; `--selective=n` turns it off whatever else is given.
  * `y`: Yes
  * `n`: No
* `-j, --jobs <N>` — Build up to N packages in parallel, respecting build-dependency order (merges are still serialised)

  Default 1 (sequential).
* `-l, --load-average <LOAD>` — Maximum 1-minute load average allowed when starting additional parallel builds (`--jobs` \> 1)

  Once at least one job is running, further starts wait until load drops below LOAD (matches real Portage's own load-average throttling). The first concurrent job is always allowed. Displayed on the `Jobs:` status line regardless.
* `--keep-going` — Continue merging as much as possible even if some packages fail. Warning: Exists for portage parity; do not use it. A failed package must stop the run.
* `--autounmask` — Accepted so existing command lines keep parsing. Config writes follow `--autounmask-write` and whether this run is pretend or `--ask`; this flag is not consulted.
* `--autosolve-use` — Let the solver choose USE flags to satisfy REQUIRED\_USE (Level C) rather than only reporting violations

  Off by default; flips are reported.
* `--eta` — With `-p`/`--pretend` or `-a`/`--ask`, print an "Expected time of completion" for the plan alongside the merge list, estimated from activity history (median of recent successful merges per package; wall uses the build graph + `--jobs` when blockers are available). Shown even when the plan needs USE/mask changes to proceed.
* `--complete-graph` — Re-solve with the installed packages whose dependencies the plan breaks, even with `--complete-graph-if-new-ver n`

  A broken installed package (e.g. `clang` pinned to the `llvm` being upgraded) is updated along with it, or holds the update back when no version of it fits. This is the default.
* `--complete-graph-if-new-ver <y|n>` — `n`: do not re-solve a plan that breaks installed packages' dependencies, only report them
  * `y`: Yes
  * `n`: No
* `--with-bdeps <y|n>` — Follow the build-time dependencies (DEPEND, BDEPEND) of installed packages that are kept

  With `--deep`, their upgrades and missing build tools enter the plan; `em depclean` keeps them installed. Unset, it is on for depclean, and for installs unless binary packages are used or `--with-bdeps-auto n` is given.
  * `y`: Yes
  * `n`: No
* `--with-bdeps-auto <y|n>` — `n`: an install does not turn `--with-bdeps` on by itself
  * `y`: Yes
  * `n`: No
* `-X, --exclude <ATOM>...` — Exclude the specified atom from being merged
* `--root-deps` — Only require RDEPEND (not DEPEND) to be satisfied in the merge target. Work-around for cross-compilation bootstrap: a still-empty target sysroot cannot yet satisfy plain DEPEND (e.g. virtual/os-headers, acct-group/root) while its own toolchain is being built. `em crossdev --setup` always applies this unconditionally; elsewhere it defaults off.

## Global options

* `--color <WHEN>` — \[default: auto\]
  * `auto`: Colour when stdout is a terminal.
  * `always`: Always colour.
  * `never`: Never colour.
