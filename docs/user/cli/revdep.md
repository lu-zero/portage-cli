<!-- @generated from em's usage spec; do not edit -->
# `em revdep`

- **Usage:** `em revdep [FLAGS]`

Rebuild packages with broken shared library deps

## Flags
- **`-L --library <NAME>`** — Only consider consumers of libraries whose soname contains NAME
- **`-h --help`** — Print help

## Roots
- **`--root <PATH>`** — Installation root (the offset an applet installs into / queries)

## Merge

How the solver and build scheduler behave.
- **`-a --ask`** — Ask for confirmation before performing actions
- **`-u --update`** — Update installed packages to newest available versions
- **`--autounmask-write`** — Write required USE changes to /etc/portage/package.use/
- **`-1 --oneshot`** — Build and install packages but do not add them to the world file
- **`-f --fetchonly`** — Only fetch distfiles, do not build or install
- **`-F --fetch-all-uri`** — Instead of building, just fetch every SRC_URI file (regardless of USE setting) for the resolved packages.
- **`-b --buildpkg`** — Build binary packages for all merged packages
- **`-B --buildpkgonly`** — Build binary packages without merging/installing them

  All build-time dependencies must already be satisfied on the system -- this does not resolve or install anything to make that true.
- **`-k --usepkg`** — Use binary packages if available, otherwise fall back to source
- **`-K --usepkgonly`** — Only use binary packages, fail if none available
- **`-g --getbinpkg`** — Fetch binary packages for all requested packages
- **`-G --getbinpkgonly`** — Only fetch binary packages, do not install
- **`-e --emptytree`** — Treat every atom as not-yet-installed, rebuilding the whole dependency tree from scratch rather than only what is missing or outdated.
- **`-t --tree`** — Show the dependency tree, indenting each package under the one that pulled it in, before merging.
- **`--json`** — Emit the depgraph as machine-parsable JSON instead of pretend text

  Takes precedence over `--tree`. Works with `-p` (including `-e`).
- **`-o --onlydeps`** — Only merge dependencies, not the specified packages themselves
- **`-n --noreplace`** — Do not replace installed packages that are already the same version
- **`-j --jobs <N>`** — Build up to N packages in parallel, respecting build-dependency order (merges are still serialised)

  Default 1 (sequential).
- **`-l --load-average <LOAD>`** — Maximum 1-minute load average allowed when starting additional parallel builds (`--jobs` > 1)

  Once at least one job is running, further starts wait until load drops below LOAD (Portage `PollScheduler._can_add_job`). The first concurrent job is always allowed. Displayed on the `Jobs:` status line regardless.
- **`--keep-going`** — Continue merging as much as possible even if some packages fail

  > **Warning:** Exists for portage parity; do not use it. A failed package must stop the run.
- **`--autounmask`** — Automatically add required USE flags and package unmask entries to config files
- **`--autosolve-use`** — Let the solver choose USE flags to satisfy REQUIRED_USE (Level C) rather than only reporting violations

  Off by default; flips are reported.
- **`--eta`** — With `-p`/`--pretend` or `-a`/`--ask`, print an "Expected time of completion" for the plan alongside the merge list, estimated from activity history (median of recent successful merges per package; wall uses the build graph + `--jobs` when blockers are available). Shown even when the plan needs USE/mask changes to proceed.
- **`--complete-graph`** — With `-u`/`--update` `-D`/`--deep`: when moving a version-pinned family (e.g. upgrading `llvm` pulls `clang` along) would leave a retained package's pin broken (e.g. `lldb` still pinned to the old `llvm`), pull that package into the plan too instead of stopping halfway. Off by default: this can revert the upgrade instead if the retained package has no version satisfying the new pin.
- **`--with-bdeps`** — Include build-time dependencies (BDEPEND) in the resolution. Default is false (exclude BDEPEND), matching emerge's default. When enabled, BDEPEND are included but filtered by what's already installed on the build host (BROOT).
- **`-X --exclude <ATOM>`** — Exclude the specified atom from being merged
- **`--root-deps`** — Only require RDEPEND (not DEPEND) to be satisfied in the merge target. Work-around for cross-compilation bootstrap: a still-empty target sysroot cannot yet satisfy plain DEPEND (e.g. virtual/os-headers, acct-group/root) while its own toolchain is being built. `em crossdev --setup` always applies this unconditionally; elsewhere it defaults off.

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`
