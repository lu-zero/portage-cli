# package.env — per-package build environment (RESOLVER-FREE slice)

STATUS: **build-env slice DONE (commit 7ce7c5b); USE part remains, resolver-side.**
On the 2026-07-18 next-pending queue as row 9 ([[PENDING]]).
The non-USE build environment is applied: `build_and_merge` (ebuild.rs) sources
matching `/etc/portage/env/<file>` entries on top of make.conf via the new
`EbuildShell::source_env_file` and the new `portage-cli/src/package_env.rs`
reader (atom→files, dir form, host + config overlay, slot-aware). FEATURES
composes, *FLAGS/MAKEOPTS replace; covered by unit + composition tests.

CONFIRMED 2026-06-20: landed in master alongside the `wip/solver-abstraction`
work (portage-solver crate + resolvo 0.11 bump, both additive — `em` depends on
neither). Full AGENTS.md checklist green (fmt/build/clippy/27 test suites), and a
re-benchmark vs baseline 8ce7a01 shows **no regression** — `em -pe` still
**1.23×** faster on firefox/gcc/multi (identical 619-line firefox plan), i.e. the
interning gains survived the fold+rebase+solver integration intact.

REMAINING (resolver-side, do NOT bundle with the above): `USE` set by a
package.env env file is not reflected in resolution — the resolved plan's USE
currently wins at build time. To honour it the resolver must read package.env
USE at resolution time (`query/depgraph/use_env.rs`) so the displayed plan and
the build agree. That is the only open piece; everything below was the original
brief for the now-completed build-env slice.

## What it is

Portage's `/etc/portage/package.env` maps atoms to env files under
`/etc/portage/env/`, and sources those files into a package's build environment
(overriding `make.conf` for matching packages). `em` can already *edit* the file
(`portage-cli/src/pkg.rs`, `em` subcommand) but **never applies it** to a build.

Reference: portage `config._grab_pkg_env` / `config.setcpv` in
`portage/package/ebuild/config.py`.

## Scope of THIS task (resolver-free)

Apply the **non-USE** build vars from the matched env files: `FEATURES`,
`CFLAGS`/`CXXFLAGS`/`LDFLAGS`/`FFLAGS`, `MAKEOPTS`, `CONFIG_*`, arbitrary build
vars — i.e. everything that affects the *build*, not the *plan*.

### EXPLICITLY OUT OF SCOPE (would desync plan vs build / touches resolver)

- **`USE` from package.env.** The depgraph has already resolved and *displayed*
  the plan's USE; setting different USE only in the build shell would build with
  flags the plan didn't show. Correct handling requires the resolver to see
  package.env USE at resolution time (`use_env.rs`) — that is the resolver
  owner's follow-up, NOT this slice. Skip `USE`/`USE_EXPAND` keys here (or warn
  + ignore), and leave a `// TODO(resolver): package.env USE` marker.

## Where it goes (the seam)

- **Reader** (new module, additive — do not edit existing readers): parse
  `/etc/portage/package.env` (`atom envfile1 envfile2 …`, dir form supported,
  `#` comments) and the referenced `/etc/portage/env/<name>` files. The env files
  are bash-style `VAR=value` assignments — reuse the existing make.conf parser if
  practical (`portage-repo/src/make_conf.rs` `MakeConf`), or a small sourced-vars
  reader. A new `portage-repo/src/package_env.rs` (or a cli-side reader) keeps it
  off shared code.
- **Apply** in `portage-cli/src/ebuild.rs::build_and_merge` (≈ line 135): right
  AFTER `apply_profile_env` (line ~235) establishes the make.conf baseline and
  BEFORE the build/FEATURES read (line ~304). For the package being merged, find
  matching package.env entries (atom vs the cpv/slot — reuse `Dep::matches_cpv`),
  and `shell.preset_var(...)` / source each env file's vars on top so they
  override make.conf for this build only.

## Semantics to get right

- Precedence: package.env overrides make.conf for the matched package; later env
  files in the line override earlier ones.
- Incremental vars: `FEATURES` is incremental (space-separated, `-feature`
  removes) — fold onto the configured FEATURES, don't replace blindly. Plain
  `*FLAGS`/`MAKEOPTS` are non-incremental (replace).
- Multiple matching atoms: apply in file order (later wins), like package.use.

## Validation

- Put `dev-foo/bar custom-flags` in package.env, `/etc/portage/env/custom-flags`
  with `CFLAGS="-O3 -march=native"` + `FEATURES="ccache"`, build `dev-foo/bar`,
  confirm the build shell sees the overridden CFLAGS/FEATURES (capture via the
  existing env dump in ebuild.rs ~line 620 `collect_env`).
- Unit-test the reader (atom→files, env-file var parse, FEATURES incremental).

## Coordination

Touch only: a new reader module + `portage-cli/src/ebuild.rs` (+ tests). Do NOT
edit `query/depgraph/**` or `portage-atom-pubgrub/**`. If `make_conf.rs` needs a
small shared helper, prefer adding a new fn over changing existing behavior.
