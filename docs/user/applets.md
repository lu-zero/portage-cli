# Applet reference

Detail behind the summary table in the [README](../../README.md#applet-status):
per-subcommand status, gaps against the real Portage tool, and notes on
deliberate design differences.

**Covered here:** `query`, `use`, `maint`, `clean`, `etc`, `revdep`,
`mirrordist`, `read`, plus [what is still planned](#em-portageq-and-em-grep-planned-user-facing).

**Covered by their own document:**

| Applet | Document |
|---|---|
| `crossdev` | [`crossdev.md`](./crossdev.md) |
| `toolchain` | [`prefix-toolchain.md`](./prefix-toolchain.md) |
| `stages` | [`stages-and-testing.md`](./stages-and-testing.md) |
| `log` | [`activity.md`](./activity.md) |
| `maint binhost`, `maint binpkg` | [`binhost.md`](./binhost.md) |
| the `--root`/`--prefix`/`--local` model the topology flags select | [`root-model.md`](./root-model.md) |

**Everything else** — `active`, `atom`, `depclean`, `ebuild`, `emerge`,
`env`, `pkg`, `quickpkg`, `regen`, `search`, `select`, `setup`, `sync` — has
no long-form how-to. Flag-by-flag help is in [`cli/`](./cli/index.md);
`em <applet> --help` is the same in the terminal. `em --help` lists every
applet, and each one's `--help` lists its subcommands.

## `em query` (equery)

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
[`../benchmarks/BENCHMARKS.md`](../../benchmarks/BENCHMARKS.md). Metadata cache regen
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

## `em use` (euse)

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
  path — see [`binhost.md`](./binhost.md)) to expand it yet.

## `em maint` (emaint)

| Subcommand | Status | Notes |
|---|---|---|
| `world` | Working | Checks `world` + `world_sets`; validates `@set` refs against known sets from `/usr/share/portage/config/sets/`, `/etc/portage/sets.conf`, and `/etc/portage/sets/`; `--fix` rewrites both files |
| `revisions` | Working | Purges `repo_revisions` JSON (sync commit history); optional per-repo targeting |
| `moveinst` | Partial | Detects installed packages needing rename from `profiles/updates/`; report-only — does not apply moves or scan installed dependency metadata |
| `movebin` | Partial | The binpkg twin of `moveinst`, sharing its `profiles/updates/` reader. Report-only for a stronger reason: renaming a GPKG container would leave the archive metadata *and* the `Packages` index still naming the old cpv |
| `logs` | Working | Prunes the `build.log` files a finished merge leaves under `<work_base>/<root-key>/<category>/<PF>/`. **Not portage's target** — real `emaint logs` cleans `PORTAGE_LOGDIR`, which for `em` holds only elog output that `em read --delete` owns. `--fix` removes, `-t 30d` bounds by age |
| `cleanconfmem` | N/A | Reports a no-op. It discards stale entries from portage's config tracker (`/var/lib/portage/config`), and `em` never writes one, so there is nothing to go stale |
| `merges` | Unavailable | `em` keeps no failed-merge registry; a failure is reported at the end of the run and in its build log |
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
- `merges` — needs a failed-merge registry `em` does not keep.
- `all` — deliberately absent. An aggregate over `maint` would mostly wrap
  no-ops, since these subcommands are overwhelmingly checks and reports; the
  thing worth batching is the cleaning, which is [`em clean
  all`](#em-clean-eclean).

**A bare `em maint`** prints this list and exits 2, like `em query` and
`em select` — it has no default action.

## `em clean` (eclean)

| Subcommand | Status | Notes |
|---|---|---|
| `dist` | Working | Removes distfiles no ebuild references. The reference set is every `DIST` line in every `Manifest` across the configured repos |
| `pkg` | Working | Removes binary packages whose cpv no longer has an ebuild |
| `all` | Working | Both of the above plus the retained `build.log` files (`em maint logs`), announcing each step |

Shared options: `--deep` narrows the reference set to what the *installed*
packages alone still name; `--size-limit 100M` and `--time-limit 2weeks`
filter the candidates. The global `-p` reports without removing.

**Why this exists when `eclean` does.** `eclean` answers about the host's
`DISTDIR`/`PKGDIR`. Under `em --root`/`--prefix`/`--local` those live inside
the offset, where a host tool cannot reach them — `em clean` follows the same
root resolution as the rest of `em`.

**Safety.** Both targets refuse to act on an empty reference set: finding no
`Manifest` entries at all means the repos were unreadable, not that every
file is stale. `clean all` keeps going when one step fails — an unreadable
`PKGDIR` should not cost you the distfile sweep — and returns the first error
at the end so a script still sees a non-zero exit.

Portage bookkeeping in a local `DISTDIR` is never a candidate: `<file>.lock`
is a live fetch lock, and `.layout.conf.<mirror>` a cached mirror layout.
The DISTDIR walk itself is shared with `em mirrordist`, so the two cannot
drift on how a distfiles directory is read; only the reference-set policy
differs.

**Gaps vs eclean:** no interactive mode, and no `--destructive` ("keep only
the newest version") flavour.

## `em revdep` (revdep-rebuild)

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

## `em mirrordist` (`emirrordist`)

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
  `Config.mirrors = thirdpartymirrors()`. When `--gentoo-mirrors-fallback`
  *is* passed, `mirror://gentoo/` candidates get the GLEP 75 hash-layout
  treatment against the configured `GENTOO_MIRRORS` too, same as every
  other URI under the flag — it is not a separate, narrower gate for this
  one URI scheme.
- A regular `em` fetch/build (not mirrordist) is unaffected by that flag —
  it always reads `GENTOO_MIRRORS` (env/shell/`make.conf`) and, since GLEP
  75, tries it for `mirror://gentoo/` URIs too (hash-layout, alongside the
  official thirdpartymirrors bases), not only for direct/non-gentoo mirror
  URLs as before.

## `em read` (elogv) and the elog system

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
records UTC rather than local time. A log directory `em` creates is mode
`2770` like portage's, but group-owned by whoever ran `em` (`SUDO_GID`)
rather than by `portage` — under `--privilege sudo` the portage group would
leave the logs unmanageable by the user who asked for them, which is the
opposite of the point. An existing directory is never re-permissioned, so a
system where portage already owns `/var/log/portage` keeps its own scheme
and `em`'s files inherit the portage group from it.

## `em etc` (etc-update / dispatch-conf)

One command for the job real Gentoo splits between two tools: `etc-update`
and `dispatch-conf` differ in UX, not in what they do. Aliased as
`em config` and `em dispatch`.

| Invocation | Does |
|---|---|
| `em etc` | List pending `._cfgNNNN_` files, grouped by target, each classified |
| `em etc diff [PATH]` | `diff -u` of target vs pending; `PATH` filters by substring |
| `em etc merge` | Interactive per file: `[n]ew [o]ld [d]iff [e]dit [m]erge [s]kip [q]uit` |
| `em etc --auto` | Resolve only what needs no decision |
| `em etc --use-new` / `--use-old` | Batch-resolve everything |

Each pending file is classified, which is what makes `--auto` safe:

| Class | Meaning | `--auto` |
|---|---|---|
| identical | byte-identical to the installed file | discards it |
| comments/whitespace only | differs only in comments and blank lines — `dispatch-conf`'s auto case | installs it |
| modified | a real content change | left for you |
| new file | the target does not exist yet | left for you |

**Why this exists when `etc-update` does.** `em` writes those sidecars under
whatever root it merged into. A host tool only ever looks at `/`, so under
`--root`/`--prefix`/`--local` there is otherwise no way to review them at
all. `CONFIG_PROTECT`/`CONFIG_PROTECT_MASK` are the stacked profile,
make.conf, and merge-root `env.d` lists — the same configuration the merge
itself used to decide what to protect.

**Notes.**

- Multiple sidecars for one target are offered oldest-first: accepting the
  newest would silently discard the versions behind it.
- Installing a pending file keeps the *sidecar's* mode (the package image),
  matching `dispatch-conf`'s use-new rename. Global `-p` lists without writing.
- `diff` and `sdiff` are shelled out to, as the real tools do — your own
  `diff` options and merge habits keep working, and `$EDITOR` is honoured by
  `[e]dit`.
- `merge` refuses outside a terminal rather than reading EOF as an answer for
  every file; use `--auto`/`--use-new`/`--use-old` non-interactively.
- After accepting a pending file, `em query check` will report an md5
  mismatch for it: `CONTENTS` holds the digest of what the *package*
  installed. Real portage behaves the same way — config files legitimately
  drift — so the VDB is deliberately not rewritten.

**Gaps vs dispatch-conf:** no RCS/archival history of superseded versions.

## `em portageq` and `em grep` — planned, user-facing

Both are CLI stubs today.

`portageq` is a **user** tool: it answers "what does this configuration
actually resolve to" — `envvar DISTDIR`, `get_repo_path`, `match`,
`expand_virtual` — which is exactly the question `em`'s own
`--root`/`--prefix`/`--local` offsets make hard to answer by hand. That
makes it *more* useful here than on a stock host, not less, since the
system's own `portageq` reports about `/` regardless of which root you are
operating on.

`grep` (a `pquery`-shaped search through ebuilds and eclasses) is likewise
for a person looking something up.

No inheritable eclass or live ebuild calls `portageq`, so `em` does not need
it as a phase builtin. The user-facing command is still wanted.

Whenever it is built, `envvar`, `has_version`, `match`, `best_version` and
`get_repo_path` cover every shape found in the wild.
