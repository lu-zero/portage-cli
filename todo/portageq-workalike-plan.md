# `em portageq` — implementation plan

STATUS: **partially implemented 2026-09-17** — the VDB group (step 2 of the
implementation order below) is done: `has_version`, `best_version`, `match`,
`mass_best_version`, in `portage-cli/src/portageq.rs`, wired as a real
`#[usage(subcommand)]` enum on `PortageqArgs` (`cli.rs`). Unit-tested
(`portageq::tests`, a throwaway `TestVdb` fixture) and live-verified against
the real installed `portageq` (portage 3.0.82.2) for every documented case —
hit, miss, invalid atom, missing `<EROOT>` — all byte-exact, including the
`"Not a directory: '<x>'" / "Run portageq with --help for info"` error text.

**Not yet done:** steps 3–7 below (settings/`envvar` group, repos group,
metadata/contents/owners, `best_visible`, protect/eclass/license/
`repos_config` tail) — still bare, not stubbed. `match`'s glob-style extended
syntax (`cat/*`, `*/*`) is also not yet supported (falls through to
"invalid atom" today); only plain dependency atoms and the empty-atom
"match everything" case work so far. Comparison-test parity (§5's
`#[ignore]` live-parity tests) has not been added yet either — only the
manual live checks above.

Original plan below, produced by reading real portage's `portageq` source
(host portage 3.0.82.2) and verifying every format live, not from memory —
still the design reference for the remaining tiers. See also
[`unimplemented-surface.md`](./unimplemented-surface.md)'s "Standalone
applets" section for the original "no known consumer" framing this
supersedes.

## 1. Survey: what real `portageq` actually is (host portage 3.0.82.2)

Source read at `/usr/lib/python-exec/python3.13/portageq` (`/usr/bin/portageq`
is a python-exec shim); every format below was also verified by running it
live.

Structural facts that shape the design:

- **No `help` subcommand, and no usage-on-no-args.** Any unrecognized word
  (including `help`, `config_root`, `mass_reset` — neither of the latter two
  exists) falls through to `pquery`, so bare `portageq` prints *every
  visible package in every repo* (19,648 lines here) and exits 0.
  `portageq --help` is the real listing.
- **Two argument conventions:** `uses_eroot` commands take `<eroot>` as the
  first positional (validated: not a directory → stderr `Not a directory:
  '<x>'` + `Run portageq with --help for info`, **exit 64**); config-only
  commands (`envvar`, `vdb_path`, `pkgdir`, `distdir`, `portdir`, …) take no
  root at all and read `portage.settings` from the ambient
  `PORTAGE_CONFIGROOT`/`ROOT`.
- **Exit codes are the API** for `has_version`/`is_protected`/`owners`/
  `best_visible`/`get_repo_path`. Error text goes to **stdout** for some
  commands (`has_version`, `best_version`, `match`, `envvar`, `contents`,
  `mass_*`) and **stderr** for others (`metadata`, `owners`,
  `get_repo_path`, `master_repos`, `eclass_path`) — inconsistent in the
  original, so it must be copied per-command, not normalized.
- `-v` is a *root-level* flag re-appended to the subcommand's argv, so
  `portageq -v envvar X` == `portageq envvar -v X`.
- Three entries in `--help` (`debug_signal`, `signal_interrupt`,
  `uses_configroot`) are introspection artifacts, not commands.

## 2. Scope decision: tiered, "everything backed by an existing `em`
   internal", explicitly excluding `pquery`

The old doc's guess (`envvar`/`has_version`/`match`/`best_version`) is right
about *demand* but wrong about *cost*. The survey shows ~20 of the 30
commands are one-to-five-line adapters over internals `em` already has
(`Roots`, `ReposConf`, `Vdb`, `ConfigProtect`, the `build_use_env`+
`ResolvedPolicy` pair). The expensive ones are expensive for a reason and
should be dropped, not stubbed:

- **Tier 1 (do first, 16 commands):** `envvar`, `has_version`,
  `best_version`, `match`, `get_repos`, `get_repo_path`, `vdb_path`,
  `metadata`, plus the settings one-liners `pkgdir` `distdir`
  `gentoo_mirrors` `config_protect` `config_protect_mask` `portdir`
  `portdir_overlay`. Each maps to an existing internal.
- **Tier 2 (cheap, do next, 12):** `contents`, `owners`, `is_protected`,
  `filter_protected`, `best_visible`, `mass_best_version`,
  `mass_best_visible`, `master_repositories`+`master_repos`,
  `available_eclasses`, `eclass_path`, `license_path`,
  `repositories_configuration`+`repos_config`.
- **Not implemented (justified refusals):** `pquery` and the bare
  fall-through (that is `em grep`'s reserved shape — see
  [`grep-qgrep-workalike-plan.md`](./grep-qgrep-workalike-plan.md) — and
  silently dumping 19k lines on a typo is a footgun; `em portageq` should
  hard-error on an unknown subcommand instead), `all_best_visible` (a full-
  tree visible resolve per `cp`; minutes of work for no consumer),
  `expand_virtual` (GLEP 37 virtual expansion has no `em` equivalent),
  `jobs` (portage's `FEATURES=observability` snapshots — `em` has its own
  activity bus; answering would be a lie), `colormap` (portage's
  `color.map`, not `em`'s palette), `list_preserved_libs` (`em`'s registry
  is its own JSON at `var/lib/portage/preserved_libs_registry`, not
  portage's pickle — implementable but answers about `em`'s state under a
  portage-shaped name).

## 3. Subcommand → output → `em` internal

`E` = eroot positional. Exit codes: `∅` = no stdout. All paths absolute. All
output plain (`std::io::stdout()`, **never** `anstream`/`style::*` —
consumers parse this).

| portageq | args | exact stdout / exit | em internal |
|---|---|---|---|
| `has_version` | `E atom` | `∅`; 0 hit / 1 miss; invalid atom → stderr `ERROR: Invalid atom: 'X'` +2; <2 args → **stdout** `ERROR: insufficient parameters!` +3 | `Vdb::open(E/var/db/pkg)` + `vdb::find_packages` (`Dep::matches_cpv`) |
| `best_version` | `E atom` | best cpv + `\n`; **no match → empty line, exit 0**; <2 args → stdout ERROR +3 | same, then max by `cpv().version` |
| `match` | `E atom` | one cpv per line, ascending; empty atom ⇒ all installed; `cat/*`,`*/*` extended syntax sorted; wrong argc → stdout `ERROR: expected 2 parameters, got N!` +2 | `find_packages`; extended syntax via the `glob_match` helper in `query/list.rs` (make it `pub(crate)`) applied to the cpn |
| `best_visible` | `E [pkgtype] atom` | cpv + `\n`, exit 0; **none → empty line, exit 1**; bad pkgtype → stderr `Unrecognized package type: 'x'` +2; <2 args → stderr ERROR +2 | `pkg.rs:resolve_active_use`'s pattern: `build_use_env` → `ResolvedPolicy::from_use_env(env, cli.arch())` → `accept_keywords.accepts` + `repo::is_masked` + `license_accepted` |
| `metadata` | `E type cpv key…` | one raw value per line in key order; `installed` needs a **full cpv**; not found → stderr `Package not found: 'X'` +1; bad type → stderr +1; <4 args → stderr ERROR +2 | installed: `InstalledPackage::field(key)`; ebuild: `Repository::cache_entry(cpv)` / `RawCacheEntry::field`; binary: `binpkg` index |
| `contents` | `E cpv` | `sorted(getcontents())`, one per line, **directories included**, bare paths; not found → stderr +1; wrong argc → stdout `ERROR: expected 2 parameters, got N!` +2 | `InstalledPackage::contents()` directly — **not** `vdb::query_files` (that drops dirs and prefixes the pkg name) |
| `owners` | `E file…` | per owner: `cpv\n` then `\t<abs path>\n` sorted; unowned → stderr `None of the installed packages claim these files:` + `\t` lines; exit 0 if any owner else 1; path outside eroot → stderr `ERROR: file paths must begin with <eroot>!` +2; bare basename allowed (matches any path with that basename) | `Vdb::owner` / `InstalledPackage::owns`; new grouping + basename mode (`vdb::query_belongs` prints only the package) |
| `mass_best_version` | `E atom…` | `atom:cpv` per line, `atom:` when none | loop over `best_version` |
| `mass_best_visible` | `E [type] atom…` | `atom:` then `best_visible`'s own line | loop over `best_visible` |
| `envvar` | `[-v] NAME…` | value per name (`""` + overall exit 1 if unset); `-v` → `NAME='shlex-quoted'`; trailing `*` = prefix match (portage's order is dict order — **sort**, document it); `PORTDIR`/`PORTDIR_OVERLAY`/`SYNC` print a deprecation line on stderr; no args → stdout ERROR +2 | `crossdev::main_repo(cli)` + `repo.shell()` + `ebuild::apply_profile_env(&mut shell, roots.config(), roots.config_overlay())` + `shell.get_var` / `ebuild::elog_setting` (exactly `info::resolve_info_var`) |
| `vdb_path` | — | `<EROOT>/var/db/pkg` | `cli.roots().merge_root()` |
| `pkgdir` `distdir` `gentoo_mirrors` `config_protect` `config_protect_mask` | — | single line | `envvar` plumbing (`elog_setting` for the first four) |
| `portdir` `portdir_overlay` | — | single line + a deprecation line on **stderr** | `Cli::repo_path()` / `Roots::portdir_overlay()` |
| `get_repos` | `E` | one line, space-separated, **reverse** resolution order (overlays first, main repo last) | `Roots::repos_conf().repos().iter().rev()` — `ReposConf::repos()` is already portage's `prepos_order` |
| `get_repo_path` | `E repo…` | path per repo; unknown → empty line + **exit 1 immediately**; invalid repo name → stderr `ERROR: invalid repository: X` +2; <2 args → stderr +3 | `ReposConf::find(name).location.as_path()` |
| `master_repositories` / `master_repos` | `E repo…` | space-joined master names; unknown repo → empty line +1 | `RepoEntry.masters`, falling back to `Repository::layout().masters` (same precedence `info.rs` uses) |
| `available_eclasses` | `E repo` | space-joined **sorted** eclass names | `Repository::eclasses()` |
| `eclass_path` | `E repo name…` | path per name; missing → empty line, exit 1 at end | `repo.path()/eclass/<n>.eclass` searched repo-then-masters (highest priority wins) |
| `license_path` | `E repo lic…` | path per license; missing → empty line, exit 1 | search `[masters…, repo]` **reversed** ⇒ repo first, then masters in reverse |
| `is_protected` | `E file` | `∅`; 0 protected / 1 not; outside eroot → stderr +2 | `ebuild::ConfigProtect::from_roots(&roots).is_protected(rel)` |
| `filter_protected` | `E` | echoes protected stdin lines verbatim; exit 2 if any line errored | same |
| `repos_config` / `repositories_configuration` | `E` | portage's INI dump: `[DEFAULT]` then a section per repo, keys sorted | new formatter over `ReposConf` (only genuinely new output format in Tier 2) |

**Root resolution** falls out of portage's own split, no new flags needed:
eroot-taking commands build their own `Roots` from the positional
(`Roots::default().with_config(Some(E)).with_base(Some(E)).with_target(Some(E)).with_broot(Some("/"))`);
config-only commands use `cli.roots()`, which already honors `em
--prefix/--local/--root` in prefix position via `Cli::root_topology` (so `em
--local L portageq envvar CFLAGS` does the right thing). Keep
`PortageqArgs` free of `RootedTopology` — `cli/topology.rs`'s module doc
already documents `Portageq` as getting none of it.

## 4. New Rust types and files

`portage-cli/src/cli.rs` — replace the current `PortageqArgs { command:
String, args: Vec<String> }`:

```rust
/// `em portageq` — Portage's own query interface, byte-compatible
#[derive(usage::Args, Debug, Clone)]
#[usage(effect = "read",
        exit_code(0, "query succeeded"), exit_code(1, "no match / unset variable"),
        exit_code(2, "invalid atom, repository or argument"),
        exit_code(3, "insufficient parameters"),
        exit_code(64, "<eroot> is not a directory"),
        example = "em portageq has_version / sys-apps/portage")]
pub struct PortageqArgs {
    #[usage(subcommand)]
    pub command: PortageqCommand,
}

#[derive(usage::Subcommands, Debug, Clone)]
#[usage(rename_all = "snake")]           // HasVersion -> has_version (verified supported)
pub enum PortageqCommand { /* one variant per command in §3 */ }
```

Aliases: `#[usage(alias_hidden = "has-version")]` etc. for `em`-idiomatic
kebab spellings; `#[usage(name = "repos_config")]` for the two real aliases
(`master_repos`, `repos_config`) — the derive accepts explicit
`name`/`alias` on variants.

New module `portage-cli/src/portageq/` (mirrors `query/`):

- `mod.rs` — `pub(crate) struct Status(pub u8)`, the `run(&PortageqCommand,
  &Cli) -> Result<Status>` match, and `fn eroot_roots(e: &str) ->
  Result<(Roots, Status)>` (the exit-64 validation).
- `settings.rs` — `envvar` + the settings one-liners; owns the single
  profile-shell build so `envvar A B C` sources once.
- `vdb.rs` — `has_version`, `best_version`, `match`, `contents`, `owners`,
  `metadata`, `mass_*`.
- `repos.rs` — `get_repos`, `get_repo_path`, `master_*`,
  `available_eclasses`, `eclass_path`, `license_path`, `repos_config`.
- `visible.rs` — `best_visible` (the policy build).
- `protect.rs` — `is_protected`, `filter_protected`.

**Every command function takes `out: &mut dyn Write, err: &mut dyn Write`
and returns `u8`** — that is what makes exact-byte assertions possible in
unit tests, and it is the one design decision worth being strict about.

`dispatch.rs`:

```rust
impl RunAsyncWith<&cli::Cli> for PortageqArgs {
    type Output = Result<()>;
    async fn run_async_with(self, cli: &cli::Cli) -> Self::Output {
        let status = crate::portageq::run(&self.command, cli).await?;
        if status.0 != 0 {
            std::io::stdout().flush().ok();
            std::process::exit(status.0 as i32);   // precedent: dispatch.rs:600, HelperArgs
        }
        Ok(())
    }
}
```

No change to `main.rs`/`error.rs` — exiting from the applet after an
explicit flush is the established pattern here, and it keeps
`has_version`'s silent exit 1 out of `print_fatal_error`.

One genuinely new library API: `envvar NAME*` needs the list of variables
the profile shell defined. `EbuildShell` iterates `self.shell.env()`
internally in `export_sourced_env` but exposes nothing; add `pub fn
var_names(&self) -> Vec<String>` to `portage-repo/src/build/shell.rs` next
to `get_var`.

## 5. Tests

- **Unit, per module** (`#[cfg(test)] mod tests` in the same file, repo
  convention): build a temp VDB with the `write_pkg` helper from
  `src/vdb.rs`'s tests (or `query/mod.rs`'s `make_vdb`), call the command
  function with `Vec<u8>` sinks, assert the **exact** bytes and returned
  code. Must-have cases: `best_version` miss → `"\n"` + code 0;
  `best_visible` miss → `"\n"` + code 1; `has_version` miss → `""` + 1;
  invalid atom → 2; missing args → 3 on stdout; `owners` tab-indentation
  and the unowned-to-stderr split; `get_repos` reversal; `get_repo_path`
  unknown repo → `"\n"` + 1; `envvar -v` quoting (quoted only when needed).
- **Live parity, `#[ignore]`, in `portage-cli/tests/comparison.rs`** (it
  already has the `CARGO_BIN_EXE_em` + external-tool harness): run `em
  portageq X` and `/usr/bin/portageq X` and compare stdout bytes *and* exit
  status for `has_version` (hit and miss), `best_version` (hit and miss),
  `match`, `get_repos`, `get_repo_path / gentoo`, `contents`, `owners`,
  `envvar CFLAGS`, `metadata / ebuild <cpv> SLOT`. This is the test that
  actually protects the formats; skip cleanly when `/usr/bin/portageq` is
  absent.
- **Docs:** the spec change is structural, so `UPDATE_CLI_DOCS=1 cargo test
  -p portage-cli --test cli_docs` is mandatory — it will replace
  `docs/user/cli/portageq.md` and add a `docs/user/cli/portageq/` page per
  subcommand (`committed_cli_docs_match_spec` fails on a missing page
  otherwise).
- Update `todo/unimplemented-surface.md` §4 and its "suggested order" item
  4 (they currently say "measured, not worth it").

## 6. Implementation order

1. **Skeleton + exit codes.** `PortageqCommand` enum with all Tier 1+2
   variants, `portageq/mod.rs` with `Status`, the eroot validator (exit
   64), an "unknown/unsupported command" error for the refused set, and the
   `dispatch.rs` wiring. Regenerate docs. Every variant returns
   `3`/unimplemented at this point — the CLI shape lands and stops being a
   lie first.
2. **VDB group** (`has_version`, `best_version`, `match`,
   `mass_best_version`) — the four with real-world consumers, all on
   `find_packages`. Unit tests + comparison tests here.
3. **Settings group** (`envvar` + the one-line aliases + `vdb_path`),
   including the `EbuildShell::var_names` addition for `NAME*`.
4. **Repos group** (`get_repos`, `get_repo_path`, `master_*`) — pure
   `ReposConf` reads.
5. **Metadata + contents + owners** — `metadata`'s three package types and
   `owners`' basename mode are the fiddly parts.
6. **`best_visible` / `mass_best_visible`** — needs the policy build; do
   after 2–5 so the cheap surface is already correct.
7. **`is_protected` / `filter_protected` / eclass+license paths /
   `repos_config`** — the tail.
8. Prune the refused commands' rows from the doc table with the one-line
   reason each (so the next reader doesn't re-litigate `pquery`).

### Critical files
- `portage-cli/src/cli.rs` (`PortageqArgs` at 2371, `QueryArgs`/
  `QueryCommand` at 2529/3528 as the pattern to mirror)
- `portage-cli/src/dispatch.rs` (`PortageqArgs` impl at 149, `run_query` at
  499, `process::exit` precedent at 600)
- `portage-cli/src/vdb.rs` (`find_packages`, `open_cli_vdb`, the temp-VDB
  test helpers)
- `portage-cli/src/info.rs` (`resolve_info_var` + the profile-shell build
  that `envvar` must reuse)
- `portage-cli/src/pkg.rs` (`resolve_active_use` at 429 — the light
  `build_use_env` + `ResolvedPolicy` path `best_visible` needs)
- `portage-cli/tests/comparison.rs` (the `#[ignore]` live-parity harness for
  byte-exact format tests)
