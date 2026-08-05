# portage-cli

[![LICENSE](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE-MIT)
[![Build Status](https://github.com/lu-zero/portage-cli/workflows/CI/badge.svg)](https://github.com/lu-zero/portage-cli/actions?query=workflow:CI)
[![dependency status](https://deps.rs/repo/github/lu-zero/portage-cli/status.svg)](https://deps.rs/repo/github/lu-zero/portage-cli)

A Rust reimplementation of the Gentoo Portage command-line tools, built on a
family of purpose-built crates for parsing atoms, metadata, repositories, and
the installed package database.

> **Note**: For a more mature Rust-based alternative, see
> [Pkgcraft](https://pkgcraft.github.io/).

> **Warning**: This codebase is currently mainly slop-coded and has not yet been
> thoroughly audited, some crates are already polished up to a degree and perform
> correctly. Use at your own risk.

> **Pre-release git checkout**: This is development source from `git` before the
> first release of `portage-cli` / the `em` binary on crates.io. The Applet status
> table below (and the per-crate "Published" vs "Local only" table) documents the
> current implementation state. See the warning above.

## The `em` binary

`em` is a unified front-end for the Portage tool suite. It dispatches to
subcommands corresponding to the traditional tools.

### Applet status

| Applet | Maps to | Status |
|--------|---------|--------|
| `atom` | — | Working |
| `query` | `equery` | Partial — see below |
| `use` | `euse` | Partial — see below |
| `pkg` | — | Working — edit `package.use` / `.accept_keywords` / `.mask` / `.env` |
| `maint` | `emaint` | Partial — see below |
| `sync` | `emaint sync` / `emerge --sync` | Working — `git` and `rsync` from `repos.conf` |
| `regen` | `emerge --regen` | Working |
| `search` | `emerge --search` | Working |
| *(default)* | `emerge` | Working — resolve → build loop; `-uD` in-slot upgrades; `--prefix` / multi-root |
| `ebuild` | `ebuild` | Working — fetch, unpack, phases, merge, VDB registration |
| `depclean` | `emerge --depclean` | Working — reverse-dep orphan clean (`-c`); world-aware |
| `quickpkg` | `quickpkg` | Working — GPKG from installed files / VDB `CONTENTS`; skips `CONFIG_PROTECT` by default |
| `mirrordist` | `emirrordist` | Working — flat-layout distfiles mirror builder, `--delete` with grace-period pruning; see below |
| `clean` | `eclean` | Stub |
| `revdep` | `revdep-rebuild` | Working — VDB-metadata-based, `-L`/`--library`; see below |
| `news` | `eselect news` | Stub |
| `glsa` | `glsa-check` | Stub |
| `log` | `genlop` | Working — `current`/`list`/`time`/`predict`; see [docs/activity.md](./docs/activity.md) |
| `grep` | `egreplite` | Stub |
| `portageq` | `portageq` | Stub |
| `read` | `elogv` / elog reader | Working — reads what the elog `save` module filed; see below |
| `select` | `eselect` | Partial — `profile`, `repository`, `compiler`, `binutils`, `linker`, `clang`, `pkgconf`, `mirrors`, … |
| `active` | — | Working — register default `--prefix`/`--local` for bare `em` |
| `setup` | — | Working — bootstrap a prefix layout (`--local` / `--prefix`) |
| `crossdev` | `crossdev` | Working — cross sysroot + staged toolchain bootstrap (`--target`) |
| `toolchain` | — | Working — native self-hosting toolchain bootstrap into `--root` |
| `stages` | catalyst stage1/3 | Partial — `--stage1` (`packages.build`), `--stage3` (emptytree `@system`); no stage4 yet |
| `dispatch` | `dispatch-conf` | Stub |
| `etc` | `etc-update` | Stub |
| `env` | `env-update` | Working — `profile.env` + `ld.so.conf` from `etc/env.d` |

---

### `em query` (equery)

| Subcommand | Alias | Status |
|---|---|---|
| `belongs` | `b` | Working — file → owning package via VDB CONTENTS |
| `check` | `k` | Working — MD5 checksum + mtime verification |
| `depends` | `d` | Working — reverse-dep search in metadata cache |
| `depgraph` | `g` | Working — full dep tree via PubGrub solver, portage-compatible output |
| `files` | `f` | Working — all files installed by a package |
| `has` | `a` | Working — VDB field search across installed packages |
| `hasuse` | `h` | Working — packages with a given USE flag in IUSE |
| `keywords` | `y` | Working — keyword status across architectures |
| `list` | `l` | Working — available packages; `-I` for installed only |
| `meta` | `m` | Working — maintainers, homepage, longdesc, installed info |
| `size` | `s` | Working — installed size + build timestamp |
| `uses` | `u` | Working — IUSE flags with descriptions + installed status |
| `which` | `w` | Working — path to best matching ebuild |

**`em query depgraph` / default resolve feature summary:**

- **VDB awareness** — installed packages use `InstalledPolicy::Favor` (keep satisfying versions); already-installed exact CPVs are filtered from the merge list; installed-and-kept packages expand runtime deps only
- **`-uD` / `--update --deep`** — transitive **in-slot upgrades** (`prefer_update`); host-satisfied build tools still enter the graph so they can upgrade (emerge deep-update). `-u` alone does not mass-upgrade deps; `-D` alone still bumps `:*` slots (`prefer_newest_slot`)
- **`-N` / `--newuse` and `-U` / `--changed-use`** — same-CPV rebuild when planned USE/IUSE differs from the VDB; with `-uD`, prefer newest when USE drift forces a rebuild
- **Profile USE flags** — `make.defaults` / `make.conf` through brush with portage-style incremental USE stacking (see `docs/architecture.md`)
- **USE_EXPAND** — `PYTHON_TARGETS`, `CPU_FLAGS_*`, `ABI_X86`, etc. expanded and grouped in output
- **OR-group branch selection** — prefer branches whose USE deps are already satisfied (avoids gratuitous rebuilds)
- **Post-solve USE-dep rebuilds** — violated USE deps on installed packages force rebuild / `upgrade_to` fixpoint (not full `--newuse`)
- **Action tags** — `N` new, `NS` new slot, `U` upgrade (`[old_ver]`), `D` downgrade, `R` reinstall
- **Preserve-libs** — NEEDED.ELF.2–driven orphan library keep/reclaim (parallel install-image ELF scan)
- **Cycle handling** — soft edges broken after SCC / Kahn order rather than silently dropping packages

**Performance** (AmpereOne aarch64, warm cache, hyperfine 2026-07-18; exit 1 ignored when the plan needs config changes):

| Target / mode | `emerge` | `em` |
|---------------|---------:|-----:|
| `www-client/firefox` `-p` | ~3.65 s | **~1.4 s** |
| `www-client/firefox` `-uDp` | ~6.3 s | **~1.45 s** (plan size ≈ emerge) |
| `app-office/libreoffice` `-p` | ~4.0 s | **~1.75 s** |

Older micro-tables (sub-second depgraph on lighter hosts) live in
[`benchmarks/BENCHMARKS.md`](./benchmarks/BENCHMARKS.md). Metadata cache regen
and install-image ELF scan benches are also there (`benchmarks/bench-elfscan.sh`).

**Gaps vs `emerge`:**
- Shallow `-p` package-set can still differ slightly from emerge on some hosts (emerge may list extra BDEPEND upgrades without `-u`); `-uDp` / `-uNDp` are near-parity on firefox-class targets
- Wrapper packages for old-slot BDEPEND (`autoconf-wrapper`, `gcc-config`, …) not fully modelled
- Flag ordering / `(-flag)` USE_EXPAND_IMPLICIT display polish
- Upgrade display shows full USE rather than only changed flags

**Gaps vs equery:**
- `uses` descriptions come from `profiles/use.desc` + `profiles/use.local.desc`.
  Overlay packages not yet regen'd fall back to empty description (metadata.xml
  per-package lookup is not yet wired as a fallback).
- No `stats` subcommand.

---

### `em use` (euse)

| Flag | Status |
|---|---|
| `-a FLAG` | Working — add USE flag to `make.conf` |
| `-r FLAG` | Working — remove USE flag from `make.conf` |
| *(no flags)* | Working — print current USE value |
| `--make-conf PATH` | Working — override make.conf path |

**Gaps vs euse:**
- No `-p pkg` for package-specific USE flags (`/etc/portage/package.use`).
- `MakeConf::get()` itself still returns the raw unexpanded value (e.g.
  `${COMMON_FLAGS}` stays literal); `em use`'s own display doesn't call the
  newer `MakeConf::apply_to()` evaluator (used by the binpkg build-env-key
  path — see [`docs/binhost.md`](./docs/binhost.md)) to expand it yet.

---

### `em maint` (emaint)

| Subcommand | Status | Notes |
|---|---|---|
| `world` | Working | Checks `world` + `world_sets`; validates `@set` refs against known sets from `/usr/share/portage/config/sets/`, `/etc/portage/sets.conf`, and `/etc/portage/sets/`; `--fix` rewrites both files |
| `revisions` | Working | Purges `repo_revisions` JSON (sync commit history); optional per-repo targeting |
| `moveinst` | Partial | Detects packages needing rename from `profiles/updates/`; does not apply moves or scan installed dependency metadata |
| `binhost` | Working | Regenerates the `Packages` index under `PKGDIR` |
| `binpkg` | Working | em-only: `verify` / `list` / `prune` / `fingerprint` / `gpg-import` (no real `emaint` equivalent) |
| `cleanresume` | Working | Reports / discards saved resume lists (`--fix`) |
| `sync` | Working | Same implementation as `em sync` |
| `regen-use` | Working | Regenerates `profiles/use.local.desc` from `metadata.xml` |
| `regen` | Working | Available as top-level `em regen` |

**Gaps vs emaint:**

- `moveinst` — missing the second pass that walks every installed package's
  `DEPEND`/`RDEPEND`/etc. fields for stale atom references, and the `--fix`
  mode that writes to the VDB.
- `world` — `@set` references are validated by name but not by content (e.g.
  `@preserved-rebuild` is accepted as long as the name is known).
- `all`, `cleanconfmem`, `logs`, `merges`, `movebin` — not implemented.

---

### `em revdep` (revdep-rebuild)

Detects installed packages whose own ELF objects require a shared-library
soname nothing installed currently provides, and rebuilds them
(`--oneshot --complete-graph`, matching gentoolkit's own always-on flags).
`-L`/`--library NAME` narrows the check to sonames containing `NAME`.

Deliberately **VDB-metadata-based**, not gentoolkit's live `scanelf` rescan of
`ld.so.conf`/`PATH` directories: every installed package's own
`NEEDED.ELF.2` VDB field already records what its own ELF objects require, so
a broken soname's owning package is already known while walking — no
`CONTENTS`-intersection ownership pass needed (unlike gentoolkit's
`assign.py`, whose scan is global and path-keyed).

**Gaps vs revdep-rebuild:**
- Only catches breakage in files already recorded as ELF-owning VDB entries;
  a hand-installed binary or a file modified out-of-band without a matching
  VDB update is invisible to it.
- No `.la` libtool-archive dependency checking.
- No `-i`/cache-file reuse (always a fresh scan).

`@preserved-rebuild` (real portage's special set for packages still linking
against a `FEATURES=preserve-libs`-preserved library) is also implemented —
usable directly, e.g. `em -p @preserved-rebuild`, anywhere a set name is
accepted, not only through `em revdep`.

---

### `em mirrordist` (`emirrordist`)

Not to be confused with `em select mirrors` (a `mirrorselect`/`eselect
mirror` workalike — picks which upstream `GENTOO_MIRRORS` *this machine*
fetches from). `em mirrordist` is the opposite-direction tool: it walks a
repository's every ebuild (every version currently in the tree, all USE
branches — a mirror must carry whatever any USE setting could ever need),
fetches every distfile its `SRC_URI` references, and verifies each against
the repo's `Manifest` — the server side of a Gentoo mirror. Requires an
up-to-date metadata cache (`em regen <repo>` first for any repo that doesn't
already ship one — this reads `metadata/md5-cache` directly rather than
falling back to live ebuild sourcing on a cache miss).

`--delete` prunes distfiles no longer referenced, behind two safety gates:
it refuses outright if the metadata scan was incomplete (missing cache
entries, unparseable `SRC_URI`/`RESTRICT`, missing digests or Manifests —
override with `--delete-allow-incomplete`) or if the scan found nothing
referenced at all (no override — a wrong `--repo` or an empty tree must
never look like "delete everything"). Orphans get a `--deletion-delay`
grace period (default `7d`) tracked in `em`'s own JSON state file, not
portage's `shelve`/dbm format — same convention as the preserve-libs
registry. `RESTRICT=fetch`/`RESTRICT=mirror` are evaluated with matchnone
semantics (every USE-conditional dropped, negated included) so a client's
particular USE selection can never change what gets mirrored.

**Gaps vs emirrordist:**
- Flat distfiles layout only — no GLEP 75 `filename-hash` (content-hash)
  layout support yet (what `distfiles.gentoo.org` itself uses); a
  `layout.conf` declaring one is refused rather than silently mishandled.
- No `--recycle-dir`, `--content-db`, `--distfiles-db`, `--mirror-overrides`,
  `--restrict-mirror-exemptions`, `--symlinks`, or `--tries` budget (an
  ordered URL list is tried until the first success instead).
- No GENTOO_MIRRORS peer fallback by default (`--gentoo-mirrors-fallback`
  opts in) — real emirrordist never uses the client's `GENTOO_MIRRORS` at
  all (that would make a mirror-of-a-peer-mirror). `mirror://gentoo/` in
  `SRC_URI` is still expanded via `profiles/thirdpartymirrors` (the
  official distfiles hosts), same as real emirrordist's
  `Config.mirrors = thirdpartymirrors()`.

---

### `em read` (elogv) and the elog system

An ebuild's `einfo`/`elog`/`ewarn`/`eerror`/`eqawarn` calls are not just
printed: each is also recorded against the phase that raised it, and replayed
once the package is merged — real portage's elog system, driven by the same
three settings.

- `PORTAGE_ELOG_CLASSES` (default `log warn error`) selects which classes are
  kept.
- `PORTAGE_ELOG_SYSTEM` (default `save_summary:log,warn,error,qa echo`)
  selects what happens to them. A module may override the class list with a
  `module:classes` suffix. Implemented: **`save`** (one
  `<category>:<pf>:<timestamp>.log` per package), **`save_summary`** (appended
  to `summary.log`), and **`echo`** (the *"Messages for package …"* block at
  the end of the run). `mail`, `mail_summary`, `syslog` and `custom` are
  accepted and ignored, as portage ignores a module it cannot import.
- `PORTAGE_LOGDIR` (default `<broot>/var/log/portage`) is where the file
  modules write, under an `elog/` subdirectory.

`em read` shows what `save` filed, newest first — the job `elogv` does
interactively:

```console
$ em read -l                    # index only
$ em read                       # the 10 most recent packages' messages
$ em read dev-libs/ -n0         # every package matching, no limit
$ em read --delete              # show, then remove what was shown
```

Files written by real portage are read too (both the flat layout and
`FEATURES=split-elog`'s per-category directories), and vice versa — the
on-disk format is portage's own, not an `em` invention.

A failed phase files its messages too — the `ewarn`/`eerror` explaining why a
build died is the most useful thing elog carries, and `${T}` is about to be
cleaned. So do the `pkg_prerm`/`pkg_postrm` of a package being removed or
replaced.

**Gaps vs portage:** no `mail`/`syslog` modules; the `summary.log` header
records UTC rather than local time; the log files are created with the
process umask rather than portage's `0o2770 portage:portage` (so under the
`sudo` privilege backend they come out root-owned, and an unprivileged
`em read --delete` on them will fail).

---

## Cross-compilation & toolchains

`em` understands the multi-root model (`docs/root-model.md`): a build reads its
config from one root (`--config-root`) and installs into another (`--root`),
with build tools resolved against the host (`BROOT`). On top of that it can
bootstrap toolchains and assemble stages.

- **`em --target <tuple> crossdev --init-target`** lays down a cross sysroot + overlay
  (a `crossdev` workalike); **`--setup`** then runs the staged
  `binutils → headers → gcc-stage1 → libc → gcc-stage2` bootstrap into
  `/usr/<tuple>`. Validated end-to-end for `riscv64-unknown-linux-gnu`.
- **`em toolchain --setup --root <dir>`** bootstraps a *native* self-hosting
  toolchain (`CHOST == CBUILD`) into an empty root —
  `baselayout → binutils → os-headers → glibc → gcc`. Unlike cross there is no
  two-stage gcc: the host (seed) compiler builds full glibc directly and a single
  full gcc links against it. Verified: a fully automated run produces a
  `gcc-16.1` in the root that compiles and links a working binary against the
  root's own libc.

The native toolchain and the cross bootstrap share one staged driver
(`crossdev::stages`), differing only in atom naming and how the `glibc ↔ gcc`
cycle is broken. Stage *production*: `em stages --stage1` installs the
profile's `packages.build` set (catalyst stage1 model); a full stage3-style
tree is still assembled with plain `em --emptytree @system` (no first-class
`--stage3` / `--stage4` flags yet).

---

## Binary packages & binhosts

`-b`/`-B` build GLEP 78 binary packages, `-k`/`-K` reuse them locally, `-g`/`-G`
fetch them from a configured binhost. Binpkgs are keyed by CPV + USE∩IUSE +
CHOST + an ISA/ABI-derived build-env key, so cross-compilation host tools and
same-CHOST, different-`-march` board variants never get mixed up. `em maint
binpkg {verify,list,prune,fingerprint}` covers maintenance (no real `emaint`
equivalent exists for this). See [`docs/binhost.md`](./docs/binhost.md) for
the identity model and PKGDIR/automation recipes.

---

## Architecture

See [`docs/architecture.md`](./docs/architecture.md) for the full crate
dependency graph, per-crate API catalog, and design reference.

### Crate family

| Crate | Purpose | Status |
|-------|---------|--------|
| `gentoo-interner` | String interning | Published |
| `gentoo-core` | Architecture and variant types | Published |
| `gentoo-stages` | Stage3 tarball fetch/cache | Published |
| `portage-atom` | PMS atom parser (`Cpn`, `Cpv`, `Dep`, `Version`) | Published |
| `portage-metadata` | md5-cache entry parser, EAPI, phases, keywords | Published |
| `portage-solver` | Solver-agnostic trait and shared vocabulary | Published |
| `portage-atom-pubgrub` | PubGrub solver bridge (default in `em`) | Published |
| `portage-atom-resolvo` | SAT dependency solver (resolvo bridge) | Published |
| `portage-vdb` | Installed package database (`/var/db/pkg`) | Published |
| `portage-binpkg` | GPKG binary package read/write | Published |
| `portage-resolve` | Resolution policy / plan layer (USE, roots, post-solve) | Local only |
| `portage-repo` | Repo layout, profiles, metadata cache, ebuild sourcing | Local only |
| `portage-distfiles` | Distfile fetch and mirror resolution | Local only |
| `portage-cli` | The `em` binary | Local only |
| `portage-bench` | Benchmark harness (`benchmarks/`) | Local only |

Further reading: [`docs/architecture.md`](./docs/architecture.md),
[`docs/build-roadmap.md`](./docs/build-roadmap.md),
[`docs/benchmarks.md`](./docs/benchmarks.md),
[`docs/binhost.md`](./docs/binhost.md) (binary packages, identity model, recipes).

### brush integration

`portage-repo` embeds [brush](https://github.com/lu-zero/brush) (the
`for-portage-repo` fork branch) — a Rust bash interpreter — for ebuild
sourcing and `make.conf` parsing. Additions to the fork:

- `Program.comments: Vec<SourceSpan>` — comment spans from the winnow parser,
  used by `MakeConf` for byte-precise round-trip editing.
- `ParseContext.comments` accumulator and comment-tracking whitespace parsers
  (`spaces_tracking`, `linebreak_tracking`, `newline_list_tracking`).

## Installation

```bash
cargo install --path portage-cli
```

## Local Development

The project expects sibling checkouts:

```
portage-cli/    # this workspace
brush/          # brush fork at for-portage-repo branch
```

Gitignored `.cargo/config.toml` path-patches the brush (and optionally
pkgcraft) crates to those checkouts — see `AGENTS.md`. Bumping the published
pin after brush work is a **routine** flow (test → push `mine/for-portage-repo`
→ bump `rev` in root `Cargo.toml`); full recipe in
[`docs/testing.md`](./docs/testing.md) § "Bumping brush".

```bash
cargo build
# CI parity — see AGENTS.md (cargo test includes doctests; nextest does not)
cargo test --workspace --exclude portage-bench
cargo clippy --workspace --exclude portage-bench -- -D warnings
cargo fmt --all -- --check
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --exclude portage-bench --no-deps
cargo msrv verify --rust-version 1.95 --path portage-cli
```

## License

[MIT](LICENSE-MIT)

## Contributing

See [AGENTS.md](./AGENTS.md) for project conventions (Conventional Commits,
style, checks).

## Author

Luca Barbato <lu_zero@gentoo.org>
