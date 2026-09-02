# cargo-ebuild — pycargoebuild replacement

Standalone workspace member `cargo-ebuild` (`bin:pycargoebuild-rs`) separate from `em` (`portage-cli/src/cli.rs:Applet`). MIT, no GPL copy (`AGENTS.md:166`).

## Goal

Parity with `pycargoebuild` (`pycargoebuild/cargo.py`/`__main__.py`/`ebuild.py`), built on top of this workspace's own machinery rather than as an isolated reimplementation. The first pass (`41a5379`) was a from-scratch clone with no ties to the rest of the workspace; it had real correctness bugs (a license AND/OR flattening bug, a fabricated `GIT_CRATES` subdir, no actual network fetch) that a second pass fixed by wiring in sibling crates that already solve the same problems, correctly and with tests. That pass also caught `templates/ebuild.j2`, which the first pass had copied near-verbatim from `pycargoebuild/ebuild.py`'s `EBUILD_TEMPLATE` Python string (same comments, same variable set, same structure line-for-line) — GPL-2 text in an MIT tree; it's now independently structured (different section order, comments, quoting) while still emitting the same `cargo.eclass`-required variables.

## Architecture

```
cargo-ebuild/
  Cargo.toml  # publish=false, deps: minijinja, cargo-lock, cargo_toml, spdx,
              # tar+xz2+flate2+sha2, portage-metadata, portage-repo,
              # portage-distfiles, tokio, chrono
  templates/ebuild.j2  # independently structured (see "GPL note" below), minijinja Environment{trim_blocks,keep_trailing_newline}
  src/
    lib.rs    # re-export cargo/ebuild/fetch/license/vendor
    cargo.rs  # Cargo.lock/toml, FileCrate/GitCrate, find_lock walk parents,
              # package_directory_in_archive (git subdir resolution),
              # license_from_crate (reads DISTDIR-fetched archives)
    ebuild.rs # minijinja render + update regex; GIT_CRATES via cargo.rs's
              # resolved subdir + Crate::git_crate_entry
    fetch.rs  # portage_distfiles::Fetcher-driven download + local sha256 verify
    vendor.rs # cargo vendor --offline (+ --sync for multi-dir) -> tarball
    license.rs# spdx::Expression AST -> Gentoo LICENSE= tokens (tree-preserving)
    main.rs   # clap derive, async (#[tokio::main]), MakeConf/ReposConf-based
              # DISTDIR/license-mapping defaults
```

## Reuse map — what comes from sibling crates and why

Three of the four hardest problems this tool has were already solved elsewhere
in the workspace:

- **License AST** (`portage_metadata::LicenseExpr`): used to validate and
  dedup the final combined `LICENSE=` expression (package license AND every
  crate's license group). The per-requirement SPDX→Gentoo mapping itself
  (`license.rs::spdx_to_ebuild`) stays bespoke — it walks `spdx::Expression`'s
  postfix token stream into a small local AND/OR tree and renders it
  preserving grouping, since `LicenseExpr`'s `Display` doesn't wrap a nested
  `All` inside an `AnyOf` in its own parens (needed for e.g.
  `(MIT AND Foo) OR Bar`).
- **Fetching** (`portage_distfiles::Fetcher`): async, resumable,
  checksum-aware-where-possible, atomic-write HTTP fetch — the same fetcher
  `em` itself uses for distfiles. `fetch.rs` builds one `Distfile` per
  `Crate` and drives them through it, then does a separate local sha256
  verification pass against each `FileCrate`'s `Cargo.lock` checksum
  (`portage_repo::ManifestEntry::verify_file` hard-checks file size before
  hashing, and `Cargo.lock` gives a checksum with no matching size — so a
  `ManifestEntry` can't stand in here; this two-phase fetch-then-verify shape
  matches pycargoebuild's own `fetch.py`/`__main__.py:315-323` split anyway).
- **Config resolution** (`portage_repo::MakeConf`, `portage_repo::ReposConf`):
  `DISTDIR` defaults to `make.conf`'s value (via `MakeConf::load_default()`),
  and the license-mapping path defaults to the main repo's
  `metadata/license-mapping.conf` (via `ReposConf::load().main_repo()`) —
  instead of hardcoded `/var/cache/distfiles` /
  `/var/db/repos/gentoo/...` guesses. Both still respect `-d`/`-l` CLI
  overrides.

What's *not* reused, deliberately:

- `cargo-lock` + `cargo_toml` — no workspace equivalent for `Cargo.lock`/
  `Cargo.toml` parsing.
- `spdx` — parses the *input* SPDX-2.0 expression from `Cargo.toml`; a
  different grammar from Gentoo `LICENSE=`, still needed alongside
  `LicenseExpr`.
- `cargo vendor --offline` (`vendor.rs`) — the only backend that actually
  understands Cargo's vendor-directory format for both registry and git
  sources; nothing in the workspace wraps it. Multiple project directories
  (`-c` with several `directories`) use cargo's own `-s/--sync <TOML>` to
  vendor them together, rather than a hand-rolled union of separately
  vendored trees. There's no manual-repack fallback if `cargo vendor` is
  missing — this tool already requires `cargo` on `PATH` to make sense of a
  `Cargo.lock`, so a second, harder-to-keep-correct vendoring implementation
  isn't worth it.

## GPL note

`pycargoebuild` is GPL-2; this crate is MIT (`AGENTS.md:166`: never copy or
closely paraphrase GPL source into this tree). Everything here is written
from independent understanding of the required *output* (cargo.eclass's
`CRATES`/`GIT_CRATES` format, PMS's `LICENSE=` grammar, Gentoo ebuild
structure) or from observed behavior, not transliterated from pycargoebuild's
Python — with one past exception: `templates/ebuild.j2` was originally copied
near-verbatim from `pycargoebuild/ebuild.py`'s `EBUILD_TEMPLATE` string (same
comments, same variable set, same line-for-line structure). It's now
independently structured — different section order, comments, and quoting —
while still emitting the same `cargo.eclass`-required variables, which are
themselves dictated by the eclass, not by pycargoebuild's particular
expression of them.

## The git-crate subdir fix

The `GIT_CRATES` value cargo.eclass needs includes the in-repo subdir
containing the crate's `Cargo.toml`, with the commit substituted for
`%commit%`. The first pass fabricated this as `{name}-%commit%`, which is
wrong whenever the archive's real top-level directory name differs (GitHub
archive tarballs generally do use `<repo>-<commit>`, not `<crate-name>-
<commit>` — they can differ, and always differ for a workspace member nested
below the repo root).

`cargo::package_directory_in_archive` fixes this by opening the crate's
already-fetched archive at `DISTDIR/<filename>` and scanning for the
`Cargo.toml` whose `package.name`/`package.version` match — ported
independently from `pycargoebuild/cargo.py:get_package_directory`
(algorithm understanding, not a transliteration of the GPL-2 Python, per
this repo's GPL-avoidance convention). Verified end-to-end against a real
GitHub archive (`dtolnay/unicode-ident`): the resolved subdir
(`unicode-ident-%commit%`) matched the archive's actual top-level directory,
correctly picked out from among four `Cargo.toml` files in that repo tree
(root crate, `diagram/`, `generate/`, `tests/crate/`) by name+version.

## CLI flags implemented

`-d/--distdir`, `-o/--output`, `-i/--input`, `-c/--crate-tarball`,
`--crate-tarball-path`, `--crate-tarball-prefix`, `--no-write-crate-tarball`,
`-L/--no-license`, `-l/--license-mapping`, `-e/--features`, `-f/--force`,
`-M/--no-manifest`, plus multiple `directories` (combined `AND` license,
pycargoebuild's `--input`/output precedence).

Template is fixed at `EAPI=8`, matching `cargo.eclass`'s
`@SUPPORTED_EAPIS: 8` (local Gentoo tree snapshot) — an `--eapi` flag to
target EAPI 9 was prototyped but reverted; it waits until the eclass
actually supports 9, since generated-and-broken output isn't a real feature.

`DESCRIPTION` (PMS 7.2: "must not be empty") falls back to `"{name} Rust
crate"` when `Cargo.toml` has no `description` field, rather than emitting
`DESCRIPTION=""` — pycargoebuild has the identical gap
(`pkg_meta.description or ""`) with no fallback; this is a deliberate
improvement over it, found while checking template output against the PMS
text directly.

## Explicitly deferred

`-F/--fetcher` choice (moot now that fetch is built-in, not shelled to
aria2/wget), `--no-config`/`pycargoebuild.toml` config file support
(`[license-overrides]`, `[paths]`), umask handling matching pycargoebuild's
exact file-mode semantics. None of these block correctness; they're
follow-up CLI-parity work.

## Verification

- `cargo check -p cargo-ebuild` / `cargo clippy -p cargo-ebuild --all-targets -- -D warnings` / `cargo fmt --check -p cargo-ebuild` clean.
- Unit tests: `license::tests` (plain AND/OR, and the AND-inside-OR /
  OR-inside-AND grouping regressions), `fetch::tests` (checksum mismatch +
  empty-checksum skip).
- Live smoke test (real network): a scratch crate with one registry
  dependency (`itoa`) and one git dependency pinned to a tag
  (`dtolnay/unicode-ident` `rev = "1.0.12"`) — both the plain (`CRATES=`/
  `GIT_CRATES=`) and `-c/--crate-tarball` (`cargo vendor`) code paths
  produce correct output; `LICENSE+="..."` for `unicode-ident`
  (`"(MIT OR Apache-2.0) AND Unicode-DFS-2016"` in its real `Cargo.toml`)
  rendered as `|| ( MIT Apache-2.0 ) Unicode-DFS-2016`, confirming the
  AND/OR grouping fix against live data, not just a synthetic test case.
