# Applet reference

Detail behind the summary table in the [README](../../README.md#applet-status):
per-subcommand status, gaps against the real Portage tool, and notes on
deliberate design differences. Applets not covered here (`active`, `setup`,
`crossdev`, `toolchain`, `stages`, `log`, `env`) have their own docs — see
the README's documentation index.

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
  `Config.mirrors = thirdpartymirrors()`.

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
