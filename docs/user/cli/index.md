<!-- @generated from em's usage spec; do not edit -->
# `em`

**Usage:** `em [FLAGS] <SUBCOMMAND>`

**Version:** 0.1.0

- **Usage:** `em [FLAGS] <SUBCOMMAND>`

## Global Flags
- **`--color <WHEN>`**

  **Choices:** `auto`, `always`, `never`

  **Default:** `auto`
- **`-p --pretend`** — Show what would be done without actually performing any actions
- **`-v --verbose`** — Increase verbosity: `-v` labels each build phase, `-vv`/`-vvv` add `em`'s own debug/trace logs (see also `RUST_LOG`).
- **`-q --quiet`** — Suppress non-error output
- **`--arch <ARCH>`** — Target architecture for operations (default: current system architecture)
- **`--repo <PATH>`** — Pin search/query to a single repository

  When unset, repositories are auto-discovered from `repos.conf` (the main repo wins for single-repo applets; search walks all of them).

## Roots
- **`--prefix <DIR>`** — Unprivileged offset: ROOT/VDB/distfiles/build trees under DIR; config still from the host (use --root for a config offset).
- **`--local [DIR]`** — Unprivileged, standalone Gentoo-Prefix: own VDB/BROOT/config, not overlaid on the host (see --prefix for the overlay). Defaults to ~/.gentoo (EPREFIX=~/.gentoo) when no DIR is given.

  > **Warning:** em active --local set steals set as the directory. Put the subcommand first: em active set --local=
- **`--config-root <PATH>`** — Read config (profile, make.conf) from this root instead of `--root`
- **`--vdb <PATH>`** — Override VDB path (default: $ROOT/var/db/pkg)
- **`-T --target <TUPLE>`** — Cross-build/setup for a crossdev target tuple

  The single source for "which tuple" everywhere: `em crossdev --target T --init-target` sets T up; `em stages --target T --stage1` (or any plain atom build) resolves/installs into the target sysroot `<EROOT>/usr/<TUPLE>` — sugar for `--config-root <sysroot> --root <sysroot>`.

  Cross context (CHOST/CBUILD, `--root-deps=rdeps`) is read from the sysroot make.conf. One flag for both roles — `crossdev` no longer has its own `-t`/`--target`.

## Flags
- **`--info`** — `emerge --info` workalike. Takes no atoms. Print system/build info: profile, CHOST/CFLAGS/FEATURES/USE (with USE_EXPAND groups like VIDEO_CARDS broken out), ACCEPT_KEYWORDS/ACCEPT_LICENSE, and configured repositories. Combine with `--json` for structured output, or `-v` to also list every known `@name` set and its resolved atoms (neither has a real-emerge equivalent).
- **`-h --help`** — Print help
- **`-V --version`** — Print version

## Roots

Which tree this invocation reads and writes.
- **`--root <PATH>`** — Prefix-position `--root` for default emerge. Not global; must not leak into crossdev/active/worker.

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
- **`-s --search`** — Search package names (each argument is a pattern)

  Deliberately separate from the `em search` applet: this is emerge's own `-s`, emerge-style output; `em search` is the equery-style applet (`--all`/`--desc`/`--name-only`/`--homepage`). Same split as real Portage's `emerge -s` vs `equery`, not accidental duplication.
- **`-S --searchdesc`** — Search package names and descriptions

  Same split as `-s`/`em search` above: emerge-style output here, `em search --desc` is the equery-style applet.
- **`-O --nodeps`** — Skip dependency resolution and only merge specified packages
- **`-C --unmerge`** — Remove the matching installed packages completely, without regard to dependencies

  Matches every installed slot/version of each atom. For removing unneeded dependencies too, use `depclean` instead.

  **Effect:** destructive — may delete or irreversibly overwrite
- **`-c --depclean`** — Remove installed packages that are not needed by @world (with no atoms, cleans everything unreachable; with atoms, only considers removing those, protecting everything else). Unlike `-C`, this walks the installed dependency graph first — matches real emerge's safe alternative to `-C`.

  Same behavior as the `em depclean [atoms]` applet — this flag exists for scripting convenience within a single `emerge`-style invocation.

  **Effect:** destructive — may delete or irreversibly overwrite
- **`-P --prune`** — Remove all but the highest installed version of each atom given, ignoring dependencies (real emerge's own historical caveat applies — prefer `--depclean` for a dependency-aware clean).

  **Effect:** destructive — may delete or irreversibly overwrite
- **`-W --deselect`** — Remove atoms and/or `@set`s from the world file, without unmerging anything.
- **`-r --resume`** — Resume the last saved merge (see `em maint cleanresume` to discard it instead)

  Atoms are not accepted together with this flag — the package list comes from the saved state. Combine with other flags (e.g. `-r -X stuck/atom`) to adjust the resumed run.
- **`--privilege <PRIVILEGE>`** — Privilege backend. Redeclared on merge applets; no `env` on the field.

  **Choices:** `auto`, `fakeroost`, `pseudoroot`, `hakoniwa`, `sudo`, `none`

  **Default:** `auto`

## Depgraph

How far to re-examine installed dependencies.
- **`-D --deep`** — Re-examine transitive dependencies

  With `--update` (`-uD`), upgrades installed packages in the depgraph to the newest accepted in-slot version (emerge `-uD`). Alone, still bumps `:*` any-slot deps to the newest slot rather than keeping a satisfying installed slot.
- **`-N --newuse`** — Reinstall installed packages when their planned USE or IUSE differs from the VDB (emerge `--newuse`)

  Applies to packages that appear in the depgraph; pairs with `--deep` for a full-tree USE recheck.
- **`-U --changed-use`** — Like `--newuse`, but only rebuild when an *enabled* USE flag changed among flags present in both installed and current IUSE (ignore pure IUSE add/drop). Emerge's `--changed-use` / `-U`.

## Activity

Where live progress is written.
- **`--activity-fd <N>`** — Write activity events as JSONL to file descriptor N (subprocess front-ends)

  Takes ownership of the FD.
- **`--activity-jsonl <PATH>`** — Append activity events as JSONL to PATH (not `-`; use `--activity-fd`)
- **`--emergelog`** — Dual-write Portage-compatible emerge.log lines (opt-in; qlop/genlop) Path defaults to `<merge-root>/var/log/emerge.log` (or `/var/log/emerge.log`).

## Output Formats

- **`json`** — Machine-parsable JSON (`--json` with `-p` or `--info`)

  **Framing:** `json`

## Subcommands

- [`em ebuild [-w --work-dir <DIR>] [--root <PATH>] <EBUILD_PATH> <PHASE>…`](ebuild.md)
- [`em maint [--root <PATH>] <SUBCOMMAND>`](maint.md)
- [`em maint binhost`](maint/binhost.md)
- [`em maint binpkg <SUBCOMMAND>`](maint/binpkg.md)
- [`em maint binpkg verify [--fix] [--require-signature]`](maint/binpkg/verify.md)
- [`em maint binpkg list`](maint/binpkg/list.md)
- [`em maint binpkg prune [--dry-run]`](maint/binpkg/prune.md)
- [`em maint binpkg fingerprint [--full] [--host]`](maint/binpkg/fingerprint.md)
- [`em maint binpkg gpg-import <KEYFILE>`](maint/binpkg/gpg-import.md)
- [`em maint cleanconfmem`](maint/cleanconfmem.md)
- [`em maint cleanresume [-f --fix]`](maint/cleanresume.md)
- [`em maint logs [--fix] [-t --older-than <AGE>]`](maint/logs.md)
- [`em maint merges`](maint/merges.md)
- [`em maint movebin`](maint/movebin.md)
- [`em maint moveinst`](maint/moveinst.md)
- [`em maint regen-use [-o --output <PATH>]`](maint/regen-use.md)
- [`em maint revisions [REPO]…`](maint/revisions.md)
- [`em maint sync [REPOS]…`](maint/sync.md)
- [`em maint world [-f --fix]`](maint/world.md)
- [`em portageq <COMMAND> [ARGS]…`](portageq.md)
- [`em sync [--root <PATH>] [REPOS]…`](sync.md)
- [`em depclean [FLAGS] [ATOMS]…`](depclean.md)
- [`em regen [FLAGS] [REPOS]…`](regen.md)
- [`em quickpkg [FLAGS] <ATOMS>…`](quickpkg.md)
- [`em mirrordist <FLAGS> [REPO]`](mirrordist.md)
- [`em query [--root <PATH>] <SUBCOMMAND>`](query.md)
- [`em query belongs <FILE>…`](query/belongs.md)
- [`em query check <ATOM>…`](query/check.md)
- [`em query depends <ATOM>…`](query/depends.md)
- [`em query depgraph [FLAGS] <ATOM>…`](query/depgraph.md)
- [`em query files <ATOM>…`](query/files.md)
- [`em query has <FIELD> [VALUE]`](query/has.md)
- [`em query hasuse <FLAG>…`](query/hasuse.md)
- [`em query keywords <ATOM>…`](query/keywords.md)
- [`em query list [-I --installed] [PATTERN]…`](query/list.md)
- [`em query meta <ATOM>…`](query/meta.md)
- [`em query size <ATOM>…`](query/size.md)
- [`em query uses <ATOM>…`](query/uses.md)
- [`em query which <ATOM>…`](query/which.md)
- [`em clean [--root <PATH>] <SUBCOMMAND>`](clean.md)
- [`em clean dist [FLAGS]`](clean/dist.md)
- [`em clean pkg [FLAGS]`](clean/pkg.md)
- [`em clean all [FLAGS]`](clean/all.md)
- [`em use [FLAGS]`](use.md)
- [`em pkg [--root <PATH>] <SUBCOMMAND>`](pkg.md)
- [`em pkg use [FLAGS] <ATOM>`](pkg/use.md)
- [`em pkg keyword [FLAGS] <ATOM>`](pkg/keyword.md)
- [`em pkg mask [FLAGS] <ATOM>`](pkg/mask.md)
- [`em pkg env [FLAGS] <ATOM>`](pkg/env.md)
- [`em revdep [FLAGS]`](revdep.md)
- [`em read [FLAGS] [PACKAGE]`](read.md)
- [`em log [--root <PATH>] <SUBCOMMAND>`](log.md)
- [`em log current`](log/current.md)
- [`em log list [LIMIT]`](log/list.md)
- [`em log time [ATOM]`](log/time.md)
- [`em log predict`](log/predict.md)
- [`em grep <PATTERN> [PATHS]…`](grep.md)
- [`em search [FLAGS] [PATTERN]`](search.md)
- [`em atom <ATOMS>…`](atom.md)
- [`em select [--root <PATH>] <SUBCOMMAND>`](select.md)
- [`em select profile <SUBCOMMAND>`](select/profile.md)
- [`em select profile list`](select/profile/list.md)
- [`em select profile show`](select/profile/show.md)
- [`em select profile set <TARGET>`](select/profile/set.md)
- [`em select repository <SUBCOMMAND>`](select/repository.md)
- [`em select repository list`](select/repository/list.md)
- [`em select repository add <NAME> <LOCATION>`](select/repository/add.md)
- [`em select repository remove <NAME>`](select/repository/remove.md)
- [`em select repository create <NAME> [LOCATION]`](select/repository/create.md)
- [`em select compiler <SUBCOMMAND>`](select/compiler.md)
- [`em select compiler list [-t --target <TARGET>]`](select/compiler/list.md)
- [`em select compiler show [-t --target <TARGET>]`](select/compiler/show.md)
- [`em select compiler set [-t --target <TARGET>] <PROFILE>`](select/compiler/set.md)
- [`em select binutils <SUBCOMMAND>`](select/binutils.md)
- [`em select binutils list [-t --target <TARGET>]`](select/binutils/list.md)
- [`em select binutils show [-t --target <TARGET>]`](select/binutils/show.md)
- [`em select binutils set [-t --target <TARGET>] <PROFILE>`](select/binutils/set.md)
- [`em select linker <SUBCOMMAND>`](select/linker.md)
- [`em select linker list [-t --target <TARGET>]`](select/linker/list.md)
- [`em select linker show [-t --target <TARGET>]`](select/linker/show.md)
- [`em select linker set [-t --target <TARGET>] <PROFILE>`](select/linker/set.md)
- [`em select clang <SUBCOMMAND>`](select/clang.md)
- [`em select clang list`](select/clang/list.md)
- [`em select clang show`](select/clang/show.md)
- [`em select clang set <SLOT>`](select/clang/set.md)
- [`em select pkgconf <SUBCOMMAND>`](select/pkgconf.md)
- [`em select pkgconf list [-t --target <TARGET>]`](select/pkgconf/list.md)
- [`em select pkgconf show [-t --target <TARGET>]`](select/pkgconf/show.md)
- [`em select pkgconf set [-t --target <TARGET>] <BACKEND>`](select/pkgconf/set.md)
- [`em select mirrors <SUBCOMMAND>`](select/mirrors.md)
- [`em select mirrors list [-c --country <COUNTRY>] [-r --region <REGION>]`](select/mirrors/list.md)
- [`em select mirrors show`](select/mirrors/show.md)
- [`em select mirrors set [-c --country <COUNTRY>] [-r --region <REGION>] [URL]…`](select/mirrors/set.md)
- [`em select news <SUBCOMMAND>`](select/news.md)
- [`em select news count`](select/news/count.md)
- [`em select news list`](select/news/list.md)
- [`em select news read [IDS]…`](select/news/read.md)
- [`em select news purge`](select/news/purge.md)
- [`em select glsa <SUBCOMMAND>`](select/glsa.md)
- [`em select glsa list`](select/glsa/list.md)
- [`em select glsa check [IDS]…`](select/glsa/check.md)
- [`em select glsa fix [IDS]…`](select/glsa/fix.md)
- [`em active <SUBCOMMAND>`](active.md)
- [`em active show`](active/show.md)
- [`em active set [REF]`](active/set.md)
- [`em active clear [--all]`](active/clear.md)
- [`em active env`](active/env.md)
- [`em active list`](active/list.md)
- [`em active add [NAME]`](active/add.md)
- [`em active remove <REF>`](active/remove.md)
- [`em setup [FLAGS]`](setup.md)
- [`em crossdev [FLAGS]`](crossdev.md)
- [`em toolchain [FLAGS]`](toolchain.md)
- [`em stages [FLAGS]`](stages.md)
- [`em etc [FLAGS] <SUBCOMMAND>`](etc.md)
- [`em etc diff [PATH]`](etc/diff.md)
- [`em etc merge`](etc/merge.md)
- [`em env [--root <PATH>]`](env.md)
- [`em completion <SHELL>`](completion.md)
- [`em emerge [FLAGS] [ATOM]…`](emerge.md)
