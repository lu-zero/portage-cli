# The Compile/Install worker split and the per-package build tree

`em` splits an unprivileged source build into two processes
(`portage-cli/src/ebuild.rs`'s `PhaseGroup`), to keep pseudoroot's
per-syscall tax (and sudo's real root) off the compile:

- **`PhaseGroup::Compile`** (the un-wrapped parent): `pretend`, `setup`,
  `fetch`, `unpack`, `prepare`, `configure`, `compile`, `test`. Runs
  unprivileged, no fake/real root involved.
- **`PhaseGroup::Install`** (a spawned, hidden `em __worker` child): `install`,
  `qmerge`. Runs under whichever privilege backend is active (pseudoroot,
  sudo, hakoniwa, …) — the only phases that actually need root-like
  behaviour (`chown`, device nodes, `setuid`, …).

Real root and the hakoniwa umbrella backend instead run everything as one
process (`PhaseGroup::Full`) — the split only exists to keep the expensive
backends off the compile.

Cross-phase shell *variables* survive the process boundary via a
`worker-env` dump/restore (`declare -p`, functions excluded — see the doc
comment on `should_dump_env`/`should_restore_env`). This file is about the
other kind of state that has to survive the same boundary: **the on-disk
build tree**.

## The build tree's directories, and which ones PMS says persist

Per PMS's ebuild-defined variables (the same predefined, read-only path
variables every ebuild phase sees — `WORKDIR`, `S`, `T`, `D`, …; see
<https://projects.gentoo.org/pms/9/pms.html>), a single package build has
several on-disk directories with **different** lifetimes:

| dir | PMS var | who writes it | who reads it | lifetime |
|---|---|---|---|---|
| `work/` | `WORKDIR`/`S` | `src_unpack`/`src_prepare`/`src_compile` | `src_compile`/`src_install` | spans the *entire* build |
| `temp/` | `T` | any phase (PMS: "temporary directory, for arbitrary use by the ebuild and eclasses") | any *later* phase | spans the *entire* build |
| `image/` | `D` | `src_install` only | `pkg_preinst`/qmerge | **fresh per `src_install` run** |
| `homedir/` | `HOME` | any phase (some build tools cache under `$HOME`) | usually same-phase only | mostly per-phase, occasionally longer |

The key distinction: `WORKDIR`/`T` are **cross-phase scratch space that
persists for the whole build**, exactly like the source tree itself — PMS
never scopes them to a single phase. `D` is different by construction: PMS
defines `src_install`'s job as populating `D` fresh each time from scratch,
so leftover files from a *previous* `src_install` attempt (e.g. an earlier,
separate re-emerge of the same package) must never survive into a new one.

## The bug this file documents (fixed)

`PhaseGroup::clean_subs()` decides which of these directories get wiped
before a phase-group's own phase loop starts. `Full`/`Compile`/`BinpkgMerge`
correctly wipe everything (`work`, `image`, `temp`, `homedir`) — they're
always starting a build from scratch. `Install` correctly does **not** wipe
`work` (its doc comment already called this out: "`work/` holds the compile
artifacts").

But until this was found (chasing a real stage1 failure), `Install`'s clean step *also* wiped `temp`
— on the unstated assumption that only `work/` needed cross-phase
persistence. That assumption is wrong for the same PMS reason `work/`
itself needs to persist: `T` is defined the same way, "for arbitrary use...
during the build process," not "reset between phases." Concretely,
`app-crypt/gnupg`'s `src_prepare` copies its systemd unit templates into
`${T}` (`GNUPG_SYSTEMD_UNITS`), and `src_install_all` later does
`systemd_douserunit "${GNUPG_SYSTEMD_UNITS[@]/#/${T}/}"` (a `doins` call) —
a completely ordinary, PMS-legal use of `T` as scratch space spanning
`prepare` → `install`.

Because `Install`'s own phase list is just `["install", "qmerge"]` — it
never re-runs `prepare` — wiping `temp` at the start of the Install worker
destroyed those staged files with nothing left to repopulate them. The
build looked completely healthy right up to the `doins` call, which then
died on a file that had existed moments earlier in the very same build.

**Fix**: `Install`'s `clean_subs()` now wipes only `image`/`homedir`, not
`temp` — matching the same "must survive the Compile→Install boundary"
treatment already correctly given to `work/`.

## Why `image` still needs wiping even inside one build

`Install`'s phase list ensures `src_install` is only ever entered once per
build attempt in the normal path, so `image` wiping there is mostly
belt-and-braces for the *retry* case (`--keep-going` re-attempting a package
that partially installed before a later failure, or a completely separate
re-emerge reusing the same work directory). Wiping `temp` for the same
reason was the actual bug — the intent behind wiping `image` ("stale `${D}`
must never leak into the current merge," `portage-cli/src/ebuild.rs`
`build_and_merge`'s comment) doesn't apply to `T`, which the ebuild is
*supposed* to be able to write once and read later.
