# `em` stage production (`em stages`) + binhosts

Separating two concepts that the early `em stage1` applet conflated:

- **toolchain** — produce a working compiler + libc in a root. `em toolchain
  --setup` (native, `CHOST==CBUILD`) and `em crossdev --setup` (cross) — they
  share `crossdev::stages` (`BootstrapKind`, `run_staged`). NOT a "stage".
- **stages** — assemble the actual release artifacts (stage1/2/3/4) *using* a
  toolchain (or a seed). This is what `em stages` will own. Not built yet.

`em toolchain --setup` landed (renamed from `em stage1`): builds
`baselayout → binutils → os-headers → glibc → gcc` into `--root`. See
`todo/em-root-characterization.md` for how each step was made to work.

## What catalyst does (native; seed = a stage3) — `~/Sources/catalyst/targets/`

Pipeline **seed → stage1 → stage2 → stage3** (→ stage4):

- **stage1** (`stage1/chroot.sh`): into empty ROOT with the **seed's compiler**:
  1. `emerge --oneshot --nodeps sys-apps/baselayout` (USE=build) — FS skeleton.
  2. `emerge --implicit-system-deps=n --oneshot <packages.build>` with
     `USE="-* build ${BINDIST} …"`, `FEATURES="nodoc noman noinfo"`.
  - `packages.build` is a **profile file** (`profiles/.../packages.build`);
    `build.py` intersects it with the profile `packages` to get versioned atoms.
    It is the *minimal bootstrap subset* of @system; the toolchain is just part
    of it, **built by the seed** — no staged gcc dance. Result is NOT
    self-hosting (links seed libs for anything not yet in ROOT).
- **stage2** (`stage2/chroot.sh`): treat stage1 as `/`; run the Gentoo
  `scripts/bootstrap.sh` — rebuild binutils/gcc/glibc/baselayout with stage1's
  own tools. This is what severs the seed and makes the toolchain
  self-consistent.
- **stage3** (`stage3/chroot.sh`): `emerge -e --update --deep --with-bdeps=y
  @system` — emptytree rebuild of all of @system with the stage2 toolchain. The
  canonical, self-hosting base. (stage4 = stage3 + extra packages/config.)

## What crossdev-stages does (cross; seed = the crossdev toolchain) — `~/Sources/crossdev-stages/cross-stage.sh`

Pipeline **toolchain → stage1 → stage3** — it **skips stage2**:

- **toolchain** (`setup_crossdev`): `crossdev <tuple> --init-target` then the
  real cross compiler build. Explicitly separate from the stages.
- **stage1** (`make`→`install_stage1`): into ROOT with the **cross compiler**:
  `ROOT=$1 USE=build <tuple>-emerge -k -b baselayout`; then `-k -b
  ${STAGE1_PACKAGES}` (the same `packages.build` list, from
  `profiles/default/linux/packages.build`); then `USE=build … -k -b portage`.
- **stage3** (`update`→`update_stage3`): refresh the cross toolchain, then
  `ROOT=$1 <tuple>-emerge -k -e @world` (emptytree rebuild into ROOT), then
  `ldconfig -r`.
- No stage2 — the cross toolchain is a freshly-built, independent compiler, so
  there is nothing to self-rebuild.

**em == crossdev, NOT catalyst (corrected).** stage2 (`bootstrap.sh`) is *not* a
native-vs-cross requirement — it is an artifact of catalyst's stage1 *reusing the
seed's existing compiler* without building a fresh one first (seed libs/CFLAGS
can leak in; stage2 converges it). `em toolchain --setup` builds a **fresh**
binutils/glibc/gcc into the ROOT (SYSROOT=ROOT; gcc links the ROOT's own glibc),
exactly like crossdev builds a fresh cross toolchain into its sysroot — and the
ROOT gets a host-usable `<ROOT>/usr/bin/<chost>-gcc`, the same shape as crossdev's
`<tuple>-gcc`. "Built by the host gcc" is true of the cross toolchain too and is
*not* seed leakage. So em follows the crossdev pipeline: **toolchain → stage1 →
stage3, no stage2.**

The only *real* native-vs-cross difference is sysroot isolation, and it is NOT
about stage2: cross gets it for free (host libs are the wrong arch, can't link by
accident); native is same-arch, so `em stages` must be **disciplined** — build the
stages with the ROOT's own `<chost>-gcc` and `SYSROOT=ROOT` (the crossdev
mechanism, same arch), never the host `/usr/bin/gcc`, so a host lib never silently
satisfies a stage build. With that discipline native is identical to crossdev.

## `em stages` — proposed design (NOT built)

Map the above onto em primitives we already have:

| stage | action | em today |
|-------|--------|----------|
| toolchain | `baselayout→binutils→os-headers→glibc→gcc` into ROOT | `em toolchain --setup` ✅ |
| stage1 | `baselayout` (USE=build, --nodeps) + `packages.build` (USE="-* build") into empty ROOT, built with the **ROOT's `<chost>-gcc` + SYSROOT=ROOT** (crossdev-style) | `em --root @system`/`packages.build` resolves exist; needs the `packages.build` set, USE wiring, and pointing CC at the ROOT toolchain |
| ~~stage2~~ | not needed — em builds a fresh toolchain first (crossdev model), so no `bootstrap.sh` self-rebuild | n/a |
| stage3 | `emerge -e @system`/@world — emptytree rebuild | `em --root <r> --emptytree @system` (engine exists) |
| stage4 | stage3 + extra packages/config | `em --root <r> <atoms>` after stage3 |

Open design questions:
1. **`packages.build` ingestion.** Read `profiles/<prof>/packages.build` stacked,
   intersect with `packages` for versions (catalyst's `build.py`). em already
   stacks profile `packages` for `@system`; add `packages.build` reading.
2. **`USE="-* build"` requires em to honour the `-*` wildcard** — it currently
   does NOT (see `todo/em-root-characterization.md` side finding). Either
   implement `-*` in env/profile USE, or drive the minimal USE another way.
   Blocks a faithful stage1.
3. **`--implicit-system-deps=n`** (catalyst) ≈ em's `--with-bdeps=n` +
   not auto-pulling the full @system closure. Confirm the mapping.
4. **CLI shape**: `em stages stage1|stage3|stage4 --root <dir>` (no stage2 —
   crossdev model), or `em stages --to stage3`. Probably a `stages` subcommand
   with per-stage actions mirroring crossdev-stages (toolchain → stage1 →
   stage3).
5. **Use the ROOT toolchain, not the host's.** The critical native discipline:
   stage builds must invoke the ROOT's `<chost>-gcc` with `SYSROOT=ROOT` (the
   crossdev mechanism), so no host lib leaks in. em already sets `CC=<bin>/<chost>-
   <tool>` + sysroot for `--cross`; the same wiring should drive native stages
   off the in-ROOT toolchain. This is what makes stage2 unnecessary.

## Binhosts — fast stage3/stage4 assembly (NEW, important)

The slow part of stage3 (`emerge -e @system`) and stage4 (extra packages) is
*compilation*. Catalyst/crossdev-stages already build with `-b`/`-k`
(`--buildpkg`/`--usepkg`): every package is saved as a binpkg and reused. With a
populated **binhost** (a binary package repository), stage3/stage4 become
*download + merge* instead of *compile* — minutes, not hours. This is the lever
for assembling stages quickly, especially across arches.

em status: `em -b/-k/-K/-g/-G` flags exist (buildpkg/usepkg/usepkgonly/
getbinpkg/getbinpkgonly), and `em maint binhost` ("Generate binary package
metadata index"). So the pieces are partly there. Work to do:

1. **✅ Producer: PKGDIR + `Packages` index — DONE.** `em -b` writes GPKGs to
   `PKGDIR` and `em maint binhost` generates the `Packages` index in portage's
   `binarytree` format. **Validated against host portage**: `binarytree.populate()`
   parses em's `Packages`, indexes the cpv, resolves SLOT/DESC/REPO/USE, zero
   invalids. GPKG (GLEP 78), not XPAK. The `read_metadata` reader (needed by the
   consumer + index) also landed. Producer commits `2f88678`…`359e65b`;
   reader+index `1b46a62` `413364f`.
2. **✅ Local reuse: `-k`/`--usepkg` — DONE & validated e2e.** The validity check
   (item 3) shipped alongside it — reuse only when version + USE (restricted
   to the package's own IUSE, `_match_use` bug-#453400 rule) + ABI + (sub)slot
   match, rebuilding a stale-USE binpkg like `emerge -k`.
3. **✅ Binpkg validity / rebuild triggers — DONE**, see item 2.
4. **✅ Consumer: `--getbinpkg` over a remote `PORTAGE_BINHOST` — DONE &
   validated e2e.** `-g`/`-G` fetch the remote `Packages` index (`.gz` then
   plain, `portage-distfiles::fetch_index`) + binpkgs over http(s), honour
   both legacy `PORTAGE_BINHOST` and modern `binrepos.conf` (combined in
   real portage's own priority order), with `URI`-header BASE_URI override,
   conditional-GET freshness caching (`If-Modified-Since`/304), and
   CHOST-gate on reuse. `-g` implies `-k` (local overrides remote); `-G` is
   binpkg-only (no source fallback).
5. **Per-arch binhost for stage assembly.** Still open — blocked on `em
   stages` itself gaining stage3/stage4 subcommands (today only stage1
   exists); `em stages --stage1` already defaults to `--buildpkg` so each
   run populates PKGDIR for reuse, but there's no stage3/stage4 driver yet
   to actually consume a binhost for near-instant re-rolls.
6. **Signing / trust.** Still open — no `BINPKG_GPG_*` signing/verify code
   anywhere. Lowest priority.

Sequence: ✅ producer (1), local reuse + validity (2/3), and remote consumer
(4) are all done. What's left: `em stages`' stage3/stage4 shape to actually
lean on a binhost (5), then signing (6) last.
