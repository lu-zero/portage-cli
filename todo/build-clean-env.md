# Build-dir clean + environment handling — portage parity

STATUS: **✅ done 2026-07-30** — `noclean`/`keeptemp`/`keepwork` FEATURES
parity in `ebuild.rs` (`filter_clean_subs`); triggering USE-reinstall symptom
closed 2026-07-29 (see [[reinstall-default]]). Originally triggered by the
staged cross glibc apparently still building headers-only on a reinstall
([[reinstall-default]], [[crossdev-target]]) — comparison of em's build-dir
lifecycle to portage `bin/phase-functions.sh` / `bin/misc-functions.sh`
(portage-3.0.79) below stands on its own regardless.

## What portage does

- **`__dyn_clean`** (`phase-functions.sh:329-350`): always removes `image/`,
  `homedir/`, `empty/`, `.installed`. Removes `${T}` (temp — which holds the
  saved `environment`) unless `keeptemp`/`keepwork`. Removes `${WORKDIR}`, the
  phase **stamp files** (`.unpacked/.configured/.compiled/.tested/…`), and
  `build-info/` unless `keepwork`. A merge starts from a clean builddir; a build
  is *resumed* only via the stamps when keepwork keeps them.
- **Phase resume via stamps** (`.unpacked` at `:305`, `.configured` at `:391`,
  …): a phase is skipped when its stamp exists. Lets an interrupted build resume
  without recompiling.
- **Per-phase environment save/source** (`phase-functions.sh:200-237`, `:212`):
  after each phase the (filtered) bash env is written to `${T}/environment`; the
  next phase sources it, carrying global-scope state (`S`, USE-derived vars,
  functions). `${T}/environment` therefore records `USE=…` — **the lingering-USE
  vector** if a builddir/`${T}` is reused across builds with different USE.
- **Post-merge clean**: gated on `keepwork`/`noclean` (and `merge-wait` for the
  early WORKDIR drop, `misc-functions.sh:256-262`).
- FEATURES that gate cleaning: **`keepwork`**, **`noclean`**, **`keeptemp`**.

## What em does (and the gaps)

- **Single carried build shell** across phases (`shell.rs:786`) instead of
  portage's per-phase `${T}/environment` save/source — so there is no
  `${T}/environment` file to leak, and no phase stamps. em **re-runs every phase**
  each `build_and_merge` (safer for a rebuild; no resume).
- **Pre-build clean** (`ebuild.rs:411`): removes `work/ image/ temp/ homedir`
  when `merge_mode && !keepwork`. **Post-merge clean** (`:452`): same gating.
- Gaps vs portage:
  1. ✅ **`keepwork` / `keeptemp` / `noclean`** — `filter_clean_subs` (2026-07-30):
     keepwork skips pre+post; keeptemp keeps `temp/`; noclean keeps
     `work/`+`temp/` on post only (make.conf(5); still pre-cleans).
  2. No phase **stamp files** ⇒ no interrupted-build resume (em rebuilds from
     scratch). Acceptable for now; revisit if rebuild cost matters.
  3. ✅ USE-carry / fresh shell — live sandbox 2026-07-29: no USE-application
     bug (see [[reinstall-default]]).

## Closed

Live sandbox run (2026-07-29) settled the USE symptom: a full `em crossdev
--setup` for riscv64 produced a correctly-recorded `-headers-only` glibc with a
real `libc.so`, and gcc-stage2 with a real `+cxx`/`libstdc++.so`. FEATURES
clean-flag parity landed 2026-07-30.
