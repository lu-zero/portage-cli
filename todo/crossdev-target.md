# Crossdev `{target}` — build a cross sysroot + compiler(s)

STATUS: **GCC toolchain bootstrap WORKS — produces a functional RISC-V cross
compiler** (2026-06-25). `riscv64-unknown-linux-gnu-{gcc,g++}` compile C and C++
(dynamic + static) to `ELF … UCB RISC-V` executables. Three bugs were fixed to
get here: the brush `export +=` append bug (cross libc `-O3` loss), em's wrong
`ESYSROOT` for `cross-*` builds (gcc-stage2 `--with-build-sysroot`), and the
missing `lib64/<abi> -> .` osdir compat symlink. See the 2026-06-25 sections
below.

**2026-06-27 — clean end-to-end run + clang/rust validated.** `em --privilege none
--local crossdev -t riscv64-unknown-linux-gnu --setup` ran the full 6-step
bootstrap (binutils→headers→libc-headers→gcc-stage1→libc→gcc-stage2) into
`~/.gentoo` in **9:56, unprivileged, exit 0** — confirms the in-flow
overlay/sysroot/symlink creation. `--privilege none` is the right backend for
`--local` (files stay user-owned; a cross toolchain `fowners` little so the degrade
path is fine — fakeroost is far too slow, sudo would leave root-owned files in the
home prefix). On the em-built sysroot, **all three cross-compile to riscv64**:
gcc-16 (C/C++, static+dynamic); clang 22 (`--target=riscv64-… --sysroot
~/.gentoo/usr/riscv64-… --gcc-toolchain ~/.gentoo/usr`); rustup rust
(`--target riscv64gc-unknown-linux-gnu -C linker=riscv64-…-gcc`). The gentoo system
`rustc` lacks the riscv64gc std — use the rustup toolchain, which has it. Remaining:
activation/wrapper polish. Goal: make `em` act as a `{target}-emerge` that actually
*builds* a cross toolchain and sysroot for a foreign `CHOST` (`CBUILD ≠ CHOST`),
covering **both** the GCC and the **LLVM/Clang** toolchain models. Target libc is
the standard choice (glibc / musl, and LLVM libc only as a generic option).

**Update 2026-08-03 — three corrections before trusting anything below:**
1. **CLI syntax throughout this doc is stale.** Every `crossdev -t <tuple>` /
   `em --cross <tuple>` / `-s0..s4` / `--ov-*` shown below is what the CLI
   looked like *before* 2026-07-09's rename (`todo/PENDING.md`): the tuple
   flag is now the single **global `--target`/`-T`**, with no separate `-t`
   on `crossdev` itself. Historical command lines are left as-is (they're
   what was literally run at the time) — read them as `-t <tuple>` →
   `--target <tuple>` / `-T <tuple>` mentally. `docs/crossdev.md` has the
   current, accurate flag reference and examples.
2. **The doc's "LLVM leads"/"LLVM first"/"simpler, preferred path" framing
   throughout (e.g. the "Stage B/C" ordering, "## LLVM/Clang cross specifics"
   below) is planning language from before either path was attempted — it
   is not a report of relative maturity.** What actually happened is the
   opposite: the **GCC** two-stage path is the one with a repeated,
   validated end-to-end bootstrap (through 2026-06-27, below). LLVM/Clang
   only ever got the setup-layer scaffolding (`CrossTarget::parse`,
   `cross_llvm-*` category/package-set derivation, `-L` flag) — no LLVM
   toolchain bootstrap has actually been run.
3. **No `em`-driven cross-*build* of a CMake/TableGen-based package (e.g.
   `llvm-core/clang` itself) has ever been tested.** The 2026-06-27 "clang
   validated" entry below is a **raw host `clang` binary** manually invoked
   with `--target=riscv64-… --sysroot=…` against the em-built sysroot — a
   sysroot-usability smoke test, not `em --target riscv64-unknown-linux-gnu
   llvm-core/clang` going through the normal cross-merge path. Every
   target package actually built through `em` so far is a small
   autotools/econf leaf (zlib, ncurses, readline, bzip2, libpcre2, less).
   This is a genuine, unverified gap, not a known-blocked one: "Stage D —
   true dual-root scheduling" (below) is about a single CPV needing both a
   host-native and a target-cross build simultaneously, which doesn't apply
   here (clang's BDEPEND tools — cmake, python — are ordinary host-native
   BDEPENDs, already routed by `host_copies.rs`, "the common case" per that
   section). Worth an empirical test, not a re-plan.

**2026-07-08 — derive-on-the-fly landed; `--prefix` setup now unprivileged.**
The on-disk symlink overlay (`write_overlay`) is gone. `cross-<tuple>/<pkg>`
is now derived from `::gentoo` at resolve time via a `Location::Alias`
repos.conf entry written by `write_alias_repo_conf` (`d7ac770`). The
merge-path CPV landmine (`Ebuild::from_path` re-deriving `CATEGORY` from the
real path, silently building native under a cross category) was closed by
`Ebuild::with_cpv` + threading a real `Cpv` through the merge path
(`b3df565`+`363e9aa`). `write_cross_env` now writes into the config overlay
(`<prefix>/etc/portage`), so `em --prefix <dir> crossdev -t <tuple> --setup`
is fully unprivileged. Live-validated: `cross-riscv64-…-gnu/binutils` builds,
registers in the VDB under the virtual cross category, `binutils-config`
activates. See `todo/cross-derive-on-the-fly.md` (incl. "How to test").

Authoritative design context: `docs/root-model.md` (§ Cross, § Sequencing),
`todo/em-root-characterization.md`, `todo/nonemptytree-bdeps-gap.md`.

## Host facts (this dev box, arm64)

- Overlays (`/etc/portage/repos.conf`): `gentoo`, `crossdev`
  (`/var/db/repos/crossdev`).
- Cross sysroots laid down: `/usr/riscv64-unknown-linux-gnu`,
  `/usr/aarch64-unknown-linux-gnu`. Installed GCC cross toolchains:
  `cross-riscv64-unknown-{elf,linux-gnu}` (VDB).
- LLVM: `llvm-core/clang` 20/21/22 + `clang-common`, `clang-toolchain-symlinks`,
  `clang-linker-config` installed.

## Already built (the foundation — do not redo)

- **Multi-repo**: the crossdev overlay loads; `cross-*` categories resolve.
- **force/mask for `cross-*`**: `package.use.force/mask/cross-*`
  (multilib/cet/nopie) apply per package.
- **ROOT-offset BDEPEND + Tier-1 host scheduling**: `host_copies.rs` post-solve
  walk emits `MergeRoot::Host` build-copies; offset `@system` = 180 == emerge
  180, `curl` = 15 == 15.
- **Cross `-p` parity**: `riscv64-…-emerge -p gcc` = 18 pkgs already matches.

So **resolution/pretend for cross is essentially done**; everything below is past
`-p`.

## Prerequisites — CONFIRMED LIVE (2026-06-20, release `em`)

Verified against the actual binary/code, not docs (which understated it):

| # | Prerequisite | Check | Result |
|---|---|---|---|
| P1 | crossdev overlay + `cross-*` resolve (symlink-follow) | `em -p cross-riscv64-unknown-linux-gnu/binutils` | `U 2.46.1 [2.46.0]` ✓ |
| P2 | `--root` target-root offset | `em -p --root /tmp/e --config-root / zlib` | `N … to /tmp/e/` ✓ |
| P3 | `--prefix` offset | `em -p --prefix /tmp/p zlib` | `R … to /tmp/p/` ✓ |
| P4 | `--local` `EPREFIX=~/.gentoo` (the **retarget** primitive) | `em -p --local zlib` | `R … to ~/.gentoo/` ✓ |
| P5 | `MergeRoot::Host` build-closure walk wired | `host_copies::compute(...,&cross)` spliced into plan (mod.rs:636) | ✓ |
| P6 | `--setup` prefix bootstrap | `setup.rs::bootstrap` (needs `--local`/`--prefix`) | ✓ |
| P7 | root derivation composes config/base/target/eprefix | `Cli::roots()` (cli.rs) | ✓ |
| P8 | **cross-context detection + dual-root routing already exist** | `root_aware::CrossContext{sysroot,target,chost,cbuild,active}`, `detect()` reads `CHOST`/`CBUILD` from the sysroot make.conf, `is_cross_arch()`; `MergeRoot` nodes in the solver | ✓ |

**Reconciliation:** the earlier "crossdev not started beyond `-p`" framing was
stale. Cross-context **detection** and **dual-root `MergeRoot` routing** are
implemented (`root_aware.rs`, `host_copies.rs`, pubgrub `MergeRoot`).
**(a) entry-point ergonomics is now DONE** — superseded by the global
`--target`/`-T` flag (2026-07-09, see the 2026-08-03 update note above);
`docs/crossdev.md` is the current reference. (b) the actual cross **build**
also landed and is validated end-to-end for the GCC model (below). What
remains genuinely open: (c) the **`--local` sub-path retarget** (below), and
whether a CMake/TableGen-scale package cross-builds cleanly (2026-08-03
update note, point 3).

### Gap for the retarget requirement (point 1)
`--local` currently hardcodes `target = EPREFIX = ~/.gentoo`. A cross sysroot at
`~/.gentoo/usr/<CTARGET>` needs either `--prefix ~/.gentoo/usr/<CTARGET>` (works
today, but config comes from the host, not the sysroot) or — better — the cross
entry point (concern 2) setting `EPREFIX=~/.gentoo`, `sysroot = target =
$EPREFIX/usr/<CTARGET>`, and `config_root` at that sysroot. The primitives (P3/P4/
P7) compose; only the cross-specific wiring is missing.

## Decomposition: three orthogonal concerns (the planning frame)

crossdev conflates three things into one stage loop. em should treat them as
**separate concerns split by install root**, each mapping to an existing em
primitive:

### 1. Populate the `<EPREFIX>/usr/<CTARGET>` sysroot — *Target root, ≈ `--local --setup`*
The target-arch artifacts: kernel-headers, libc (glibc/musl), and the runtimes
(compiler-rt, libunwind, libc++/libc++abi). They install **into the sysroot**
(cross-built). This is em's ROOT-offset build of target packages — the
`--root`/`--local` machinery — plus a `--setup`-style bootstrap to lay down the
empty sysroot's base config (crossdev writes
`<sysroot>/etc/portage/{make.conf,package.*}`; em's `--local --setup` already
bootstraps a prefix — reuse that path, see `setup.rs`).

**REQUIREMENT — the sysroot must be RELOCATABLE, not hardcoded to `/usr/<CTARGET>`.**
The axis is **self-contained (own libc/kernel) vs host-shared**, and it is the
`--root` vs `--prefix` distinction (NOT `--local`, which is merely a prefix):

| mode | sysroot location | libc + kernel | which em primitive |
|---|---|---|---|
| **default** `em crossdev <t>` | `/usr/<CTARGET>` (`EPREFIX=/`) | system | (root install, crossdev parity) |
| **`--root DIR`** | `DIR` (own VDB) | **built from scratch** (self-contained) → the "stage1 from scratch" | `--root` offset, empty VDB ⇒ full closure `N` |
| **`--prefix DIR`** (and `--local` = `--prefix ~/.gentoo`) | `DIR` | **host's libc+kernel SHARED** (base = host; only the delta builds) | prefix overlay, host VDB shared ⇒ delta only |

- **`--local` is shorthand for `--prefix ~/.gentoo`** — a host-sharing prefix,
  *not* self-contained. (Smoke test confirmed: `em -p --local cross-…/gcc` →
  `U gcc`, no closure, because the host's cross binutils/glibc/headers are
  shared. Correct for a prefix.)
- **`--root <empty>`** is the self-contained path: an isolated VDB ⇒ em plans the
  whole toolchain closure from scratch (all `N`), own libc/kernel. This is "stage1
  from scratch".
- **`--prefix`** = root-model `target ≠ base` (base = host): share host
  libc+kernel-headers, build only what's missing — much lighter.

Both must work; the driver (concern 2) takes the sysroot/prefix/root as input and
nothing may assume `/usr/<CTARGET>`.

This reuses the `--local`/`--prefix` EPREFIX machinery (`root-model.md`,
[[local-eprefix-mode]]): every cross location var gets the prefix —
`SYSROOT=ESYSROOT=<EPREFIX>/usr/<CTARGET>`, `ROOT` likewise, and
`PORTAGE_CONFIGROOT=<EPREFIX>/usr/<CTARGET>`. NB for LLVM:
the generated `clang` cross cfg (`--sysroot=…`) and `/etc/clang/cross/*.cfg`
location must also follow `<EPREFIX>`, not the hardcoded `/usr/<CTARGET>` crossdev
writes.

### 2. The driver — *a dedicated `em crossdev` subcommand (+ a `<CTARGET>-emerge` wrapper)*
**Make it a separate subcommand**, not just a flag, so users coming from the
original `crossdev` get a seamless/familiar interface. Two entry forms:

- **`em crossdev -t <tuple> [-s0..s4] [-L] [--ex-pkg X] [--ov-output DIR] …`** —
  the orchestrator, **mirroring the original `crossdev` option surface** (same
  flags: `-t/--target`, the stage flags, `-L/--llvm`, `--ex-*`, the overlay/`--ov-*`
  and package-override `--[bdgkl]pkg/cat/env` options, `-S/--stable`, `-C/--clean`,
  `--init-target`, `--show-target-cfg`). It does concern-1 init/setup (lay down
  the sysroot base + overlay, reusing `--setup`/`setup.rs`) and drives the stage
  sequence (concerns 1+3) over em's own resolve+build — replacing the crossdev
  bash, not shelling out to it.
  **Default install target: `/usr/<CTARGET>`** (`EPREFIX=/`), exactly like the
  original crossdev — bare `em crossdev <tuple>` is the privileged system install
  to `/usr/<CTARGET>`. The retarget (concern 1, `<EPREFIX>/usr/<CTARGET>`, e.g.
  `~/.gentoo/usr/<CTARGET>`) is **opt-in** via `--local`/`--prefix`; the default
  is unchanged so existing crossdev users get identical behaviour.
- **`<CTARGET>-emerge <pkg>`** — the per-target emerge for ongoing builds /
  `--ex-pkg` (concern 3) and target packages (concern 1). Generated by
  `em crossdev` (like crossdev installs `/usr/bin/<CTARGET>-emerge`); it's just
  `em` with the cross context auto-detected (P8 `root_aware::detect` already does
  this from the sysroot make.conf) — sets
  `CHOST/CBUILD, SYSROOT=ESYSROOT=<EPREFIX>/usr/<CTARGET>, BROOT=/, ROOT=…`.

So the subcommand owns the orchestration + UX; the wrapper is the thin
per-package path. Map crossdev flags 1:1 where sensible; document any deltas.

### 3. Host-installed cross tooling — *Host root, the "cross compilers" / `--ex-pkg`*
Things that install on the **host** (`ROOT=/`) but provide target capability:
`cross-<CTARGET>/{binutils,gcc}` (the `<CTARGET>-gcc` binaries), the rust target
std (`rust-std` / `RUST_TARGETS`), `clang-crossdev-wrapper`. Host builds of
`cross-*` packages → `MergeRoot::Host` (the `host_copies.rs` machinery); the
eclass builds them as cross-compilers targeting `<CTARGET>`. `--ex-pkg` just adds
more of these on demand.

**The stage loop is the dependency ordering that interleaves 1 and 3**
(binutils→gcc1 [host] → headers→libc [sysroot] → gcc2 [host] → runtimes
[sysroot]). em's resolver orders by deps; a thin stage driver supplies the
per-stage `USE`.

**LLVM (`-L`) collapses concern 3**: clang already cross-targets, so there is no
per-target compiler to build host-side — concern 3 shrinks to
`clang-crossdev-wrapper` (+ `/etc/clang/cross/<CTARGET>.cfg`), and the bulk is
concern 1 (sysroot) + concern 2 (driver). This is why LLVM leads.

The Stages A–D below are the *implementation* increments; concerns 1/2/3 are the
*architecture* they serve (2 = Stage A; 1 = a Target-root build reusing
`--local`; 3 = Host-root builds via the existing `MergeRoot::Host` walk).

## crossdev-bash behaviour, characterized (2026-06-20, `--show-target-cfg`)

Tuple = `ARCH-VENDOR-OS-LIBC`; libc ∈ `gnu`(glibc)/`musl`/`newlib`(bare metal)/
`uclibc`/`klibc`. `--show-target-cfg -t <tuple>` (safe, no writes) gives the
package set; combined with the `doemerge` stage loop:

| target | overlay **category** | libc | kernel | toolchain pkgs |
|---|---|---|---|---|
| `…-linux-gnu` (GCC) | `cross-<CTARGET>` | `sys-libs/glibc` | `sys-kernel/linux-headers` | binutils, gcc, linux-headers, glibc |
| `…-linux-musl` **`-L`** | **`cross_llvm-<CTARGET>`** | `sys-libs/musl` | linux-headers | clang-crossdev-wrapper, compiler-rt, libunwind, libcxxabi, libcxx (+ musl) |
| `…-elf` (bare metal) | `cross-<CTARGET>` | `sys-libs/newlib` | **none** (`kernel_category=`) | binutils, gcc, newlib |

KEY behaviours to match:
- **LLVM uses a different category prefix `cross_llvm-<CTARGET>`** (not
  `cross-<CTARGET>`). em must resolve/route both.
- **`-L` rejects glibc** — `crossdev -L … -linux-gnu` errors "LLVM/Clang cannot
  currently compile glibc". LLVM ⇒ musl / newlib / llvm-libc only.
- **bare-metal (`-elf`)** has **no kernel-headers** stage; libc = newlib.
- This box has GCC targets installed (`cross-riscv64-unknown-linux-gnu`,
  `cross-riscv64-unknown-elf`); `cross_llvm-*` (LLVM) is not yet set up here.

### Host stage1 vs target sysroot — which config (clarified + validated)
- **Host stage1 (concern 3, the cross compiler/tools)**: just use the **HOST
  config**. `cross-<CTARGET>/{binutils,gcc}` are host-arch tools targeting
  `<CTARGET>`; the eclass does cross via the *category*. Validated:
  `em -p --root /tmp/e --config-root / cross-…/binutils` → from-scratch closure
  (`virtual/libintl`, `libiconv`, `zlib`, … all `N`). No special config needed.
- **Target sysroot (concern 1, libc/headers/runtimes)**: uses crossdev's
  **special make.conf** (`CHOST/CBUILD`, `ROOT=/usr/${CHOST}/`) + a target
  **profile link** (next item).
- The earlier `--root` NoSolution was self-inflicted: I fed the *special cross*
  make.conf (`--config-root /usr/riscv64…`, whose `ROOT=/usr/${CHOST}/` fought
  `--root`) to a *host stage1* build. Host stage1 wants host config; the special
  config belongs to the target-sysroot build.

## Profile linking for the target sysroot — ITEM TO ADDRESS

How crossdev-stages does it (the reference; `lib/sysroot.sh:84-93`,
`crossdev-stages/src/target.rs:123-160`, `cross-stage.sh:45-99`):
- **`eselect profile` CANNOT be used cross-arch** (host ARCH ≠ target ARCH) —
  explicit comment in `lib/sysroot.sh:90`. So the profile is linked by a **direct
  absolute symlink**:
  ```
  ln -s /var/db/repos/gentoo/profiles/<target-profile> <sysroot>/etc/portage/make.profile
  ```
- the target profile path is arch-specific, mapped from the tuple, e.g. riscv64 →
  `default/linux/riscv/23.0/rv64/lp64d` (`common.sh:258` `gentoo_profile`,
  `cross-stage.sh:45`).
- the Rust `target.rs` copies both the `make.profile` symlink and the `profile/`
  dir from the crossdev prefix (`/usr/<CHOST>/etc/portage`) into the target
  sysroot's portage config.

**em requirement:** concern-1 init/`--setup` for a cross target must (a) write the
special make.conf (`CHOST/CBUILD`), and (b) **link `make.profile` directly to the
target-arch profile in the repo — NOT via `eselect profile`** (which validates
against the host arch and fails). Need a tuple→profile mapping (reuse
crossdev-stages' `gentoo_profile`, or crossdev's `--show-target-cfg` arch). This
is also the missing piece behind concern 1's self-contained `--root` path.

### crossdev's hardcoded `embedded` profile is a SHORTCOMING — em follows the crossdev-stages fix
Read from the canonical `crossdev` sources (`emerge-wrapper` `cross_wrap_etc`):
crossdev links **`embedded` for *every* sysroot** —
`ln -snf ${MAIN_REPO_PATH}/profiles/embedded ${SYSROOT}/etc/portage/make.profile`
— regardless of target arch. Because `embedded` is arch-neutral, crossdev then
has to **inject what the profile would have provided** via a local `profile/`
subdir it ships in `/usr/share/crossdev/etc/portage/`:
- `profile/make.defaults`: `ARCH=<arch>`, `KERNEL="-linux <kernel>"`, `ELIBC=<libc>`
- `profile/use.force`: `-kernel_linux` + `kernel_<KERNEL>`
- (LLVM) appends `CC=<CHOST>-clang`, `LD=ld.lld`, `AR=llvm-ar`, … to make.defaults

That whole `profile/` dance exists **only to paper over the arch-neutral
`embedded` base** — it loses the arch profile's multilib/ABI/USE-default chain
(e.g. riscv `rv64/lp64d`), which crossdev then has to reconstruct per-package in
the multilib env files (`load_multilib_env`). **crossdev-stages fixes this**
(`lib/sysroot.sh:84-93`): it links the proper arch-specific `gentoo_profile`
(`default/linux/riscv/23.0/rv64/lp64d`) directly, so ARCH/ELIBC/KERNEL/ABI all
come from the profile and no `profile/` override is needed.

**em adopts the crossdev-stages fix** (`crossdev/target.rs::profile_path`): link
the arch-specific profile for OS targets; fall back to `embedded` **only** for
bare-metal (`-elf`/newlib, no kernel), where no `default/linux/<arch>` profile
applies. Consequently em does **not** need crossdev's `profile/make.defaults` +
`use.force` shim — the arch profile supplies ARCH/ELIBC/KERNEL. (LLVM's
`CC=<CHOST>-clang` toolchain vars from make.defaults are still relevant and
belong with the Stage-B build-env wiring, not the profile shim.)

### crossdev helper wrappers (read for Stage A/B build-env parity)
- **`cross-emerge`/`cross-ebuild`**: `CHOST` from argv0, `SYSROOT=/usr/$CHOST`,
  `PORTAGE_CONFIGROOT=$SYSROOT`, `CBUILD`+`BUILD_*FLAGS` from host `portageq`,
  `exec emerge --root-deps=rdeps`. (ROOT comes from the sysroot make.conf.)
- **`cross-pkg-config`** (`<CHOST>-pkg-config`): sets `PKG_CONFIG_SYSROOT_DIR=
  $SYSROOT`, `PKG_CONFIG_LIBDIR`/`PKG_CONFIG_SYSTEM_*` into `$ESYSROOT/usr/<libdir>`,
  libdir from `LIBDIR_${ABI}` (else probe `-print-file-name=pkgconfig`), and
  **rejects host `-I`/`-L`** in the output as a guard. em's build env must point
  pkg-config at the sysroot for cross target-package builds.
- **`cross-fix-root`**: post-install fixup — chmod sysroot libs, rewrite `.la`
  `libdir=`/`dependency_libs` and `*-config` `prefix=` to `$SYSROOT/usr`, and add
  `<CHOST>-`prefixed `*-config` symlinks. Relevant to the merge/postinst path.

## The two toolchain models (KEY: they are very different)

### GCC cross (`cross-<triple>/*`, crossdev's classic model)
Per-target *compiler binaries* (`<triple>-gcc`, `<triple>-as`/`ld`). crossdev
builds them as `cross-<triple>/{binutils,gcc,glibc|musl,linux-headers}` with a
**staged bootstrap** because gcc needs a libc that needs headers that need
binutils:
1. `cross-<triple>/binutils`
2. `cross-<triple>/linux-headers` (kernel UAPI)
3. `cross-<triple>/gcc` **stage1** (`USE=-* nostdlib`, C only, no libc)
4. `cross-<triple>/glibc` (or `musl`) — built with stage1 gcc
5. `cross-<triple>/gcc` **stage2** (full, links against the new libc)
The resolver orders these by deps, but the **stage1/stage2 gcc USE split** is
crossdev policy, not in the ebuild graph — em must drive it.

### LLVM/Clang cross (the simpler, preferred path — what "better llvm/clang" means)
`clang`/`lld` are **already cross-compilers**: one host binary targets any triple
via `--target=<triple> --sysroot=<sysroot>`. **No per-target compiler build.**
The cross toolchain is just the *target* runtime bits built into the sysroot with
the host clang cross-targeting:
1. `cross-<triple>/linux-headers` (or none for `-elf`/baremetal)
2. **libc for the target**: glibc / musl (or LLVM libc as a generic option),
   cross-built with host clang.
3. `compiler-rt` (builtins), `libunwind`, `libc++`/`libc++abi` for the target.
No stage1/stage2 dance: clang+lld already exist; we only produce the sysroot
contents. This makes LLVM cross dramatically less staged than GCC — lead with it.

## How crossdev actually works (read from `/usr/bin/crossdev`, 2057-line bash)

crossdev does **not** resolve or build anything itself — it (1) lays down the
overlay + config for `<CTARGET>`, then (2) drives a fixed sequence of
`emerge cross-<CTARGET>/<pkg>` calls with per-stage `USE`. The unit is:

```
doemerge <pn> [logsuffix]:
    set_use <pn> <USE>            # writes the per-pkg package.use into the overlay
    emerge cross-<CTARGET>/<pn> ${EOPTS}     # EOPTS = UOPTS + "-u"  (no --nodeps/--oneshot by default)
```
Global env it sets: `EMERGE_DEFAULT_OPTS=--quiet-build=n`,
`FEATURES="$FEATURES -stricter"`, `USE="$USE -selinux"`.

### Package set (chosen by tuple + `--llvm`)
GCC: `BPKG`=binutils, `GPKG`=gcc, `KPKG`=linux-headers, `LPKG`=libc
(glibc/musl/… from the tuple's LIBC field). LLVM: `CPKG`=clang-crossdev-wrapper,
`RPKG`=compiler-rt, `UPKG`=libunwind, `APKG`=libcxxabi, `PPKG`=libcxx. Each has a
matching `?USE` var (`BUSE`/`GUSE`/…) and stage-disable masks
(`GUSE_DISABLE_STAGE_1/2`).

### Stage sequence (the emerge calls)
GCC:
- **s0** binutils — `USE=$BUSE doemerge $BPKG`
- **s1** bare C compiler — (if `--with-headers`: kernel `headers-only`, then libc
  `headers-only` with `--nodeps`) then `USE="$GUSE $GUSE_DISABLE_STAGE_1" doemerge $GPKG-stage1`
- **s2** kernel headers — `USE="$KUSE headers-only" doemerge $KPKG`
- **s3** full libc — `USE="$LUSE $LUSE_DISABLE" doemerge $LPKG`
- **s4** full gcc — `EOPTS+=--newuse USE="$GUSE $GUSE_DISABLE_STAGE_2" doemerge $GPKG-stage2`

LLVM (`-L`): preflight asserts `llvm-core/llvm` installed AND the target arch is
in its `llvm_targets_*` USE; writes `/etc/clang/cross/<CTARGET>.cfg`
(`--sysroot=/usr/<CTARGET> --target=<CTARGET> @../gentoo-runtimes.cfg`;
`-static -fno-stack-protector` for llvm-libc). Then: s0 `$CPKG`
(clang-crossdev-wrapper), s1 `$RPKG` (compiler-rt), s4 `$UPKG`(libunwind
static-libs)/`$APKG`(libcxxabi)/`$PPKG`(libcxx). No stage1/stage2 gcc split —
clang is the cross compiler.

Extra (after stages): `--ex-gcc`→`$GPKG-extra`, `--ex-gdb`→`$DPKG`,
`--ex-pkg X`→`doemerge X`.

### KEY ARCHITECTURAL INSIGHT: em does NOT reimplement cross-compilation
`set_links` (l.1416) shows `cross-<CTARGET>/<pkg>` is a **symlink** to the real
`<cat>/<pkg>` ebuild dir (e.g. `cross-riscv64…/gcc` → `sys-devel/gcc`). The cross
magic (CHOST mangling, installing libc/headers into `/usr/<CTARGET>` while the
compiler lands on the host, stage gating) lives in the Gentoo **eclasses**
(`toolchain.eclass`, `toolchain-funcs`, cross handling), triggered by the
`cross-<CTARGET>` **CATEGORY**. em already resolves these symlinked ebuilds
(multi-repo + `follow_links(true)`, per [[project-dep-resolver]]). So em's builder
does **not** cross-compile by hand — it runs the ebuild phases for
`cross-<CTARGET>/<pkg>` (brush already sources the eclasses) and the eclass does
cross. em's real additions are: (A) the cross config/env entry, (B) running those
phases ROOT-correctly, (C) the per-stage `USE` (`headers-only`, stage1/stage2
disables) that crossdev injects via `set_use`.

### KEY DESIGN DECISION for em
crossdev's `doemerge` calls literal `emerge`. So there are two ways em fits:
1. **em as the emerge backend** crossdev drives (lightest): em just needs to
   correctly cross-build ONE `cross-<CTARGET>/<pkg>` (Stage A+B). Then real
   `crossdev -t <tuple>` + `<CTARGET>-emerge <pkg>` (and `--ex-pkg`) work by
   pointing at em. Immediately useful; em owns the *build*, crossdev owns the
   *orchestration*.
2. **em replicates the orchestration** (`em --cross -t <tuple>`): em owns the
   overlay/config setup + the stage loop too. More work; only needed if we want
   to drop the crossdev bash dependency.
Recommend **(1) first** — Stage A+B unlocks `<CTARGET>-emerge`/`--ex-pkg` and is
the foundation for (2). Stage C (the stage loop) is the (2) increment.

### Still to read in the script (next)
`parse_target` (l.142, tuple→vars), `setup_portage_vars` (l.658),
`set_links`/`set_use_force`/`set_use_mask`/`set_metadata` (l.1416–1547, the
overlay/symlink/config writers), `load_multilib_env` (l.1212).

## First prerequisites — three Stage-0 setup tools (= `--init-target`, no build)

STATUS: **DONE (2026-06-20).** Shipped as the `em crossdev` subcommand
(`src/crossdev/{mod,target}.rs`): `--show-target-cfg` (preview, no writes) and
`--init-target` (lay down everything). `CrossTarget::parse` does the tuple →
category / package-set / `gentoo_arch` / profile / `CFLAGS` derivation (glibc
`gnu`, `musl`, bare-metal `-elf`/`-eabi` newlib; `-L` ⇒ `cross_llvm-*` and
rejects glibc). Install root follows the em root model: sysroot =
`<EROOT>/usr/<CTARGET>` (`/usr/<CTARGET>` default; `--local`/`--prefix`/`--root`
retarget). All three tools below implemented + idempotent; verified against a
sandboxed `--config-root`/`--root` (overlay symlinks/metadata/categories/
repos.conf match the live crossdev overlay byte-for-byte; sysroot make.conf has
`CBUILD`/`CHOST`/`CTARGET`/`ARCH`/keywords/`CFLAGS`; make.profile is a direct
absolute symlink). 4 unit tests in `target.rs`. **Next: Stage B** (cross-build
one target package end-to-end).

The foundation to build FIRST (pure filesystem setup, independent of the
resolver). They compose into a no-build `em crossdev --init-target` /
concern-1+2 init:

1. **Repo-management tool — create the crossdev overlay.** Lay down
   `cross-<CTARGET>` (or `cross_llvm-<CTARGET>` for `-L`): per-package symlinks to
   the real ebuild dirs (crossdev `set_links`), `metadata/layout.conf` +
   `profiles/{repo_name,categories}` (`set_metadata`), and a `repos.conf` entry.
   *em today:* `ReposConf` (repos_conf.rs) **reads** only — overlay **creation is
   NEW** (symlinks + metadata + repos.conf write).
2. **Confdir-creation tool — write the cross make.conf.** Write the special
   `<sysroot>/etc/portage/make.conf` (`CHOST`, `CBUILD`, `ROOT=/usr/${CHOST}/`,
   `CFLAGS`, …) — crossdev `set_metadata` / crossdev-stages `target.rs`.
   *em today:* **mostly EXISTS** — `MakeConf::{set,save}` (make_conf.rs) +
   `setup.rs::bootstrap` already write a prefix make.conf. Reuse for cross values.
3. **Profile-management tool — link the profile in the confdir.** Symlink
   `<sysroot>/etc/portage/make.profile` → `…/gentoo/profiles/<target-profile>`
   **directly** (NOT `eselect profile` — fails cross-arch), plus the
   tuple→profile mapping.
   *em today:* reads `make.profile` everywhere; has symlink helpers
   (`setup.rs::link_host_*`, `std::os::unix::fs::symlink`) + an `eselect` wrapper
   (`select`) — but the **cross-arch direct `make.profile` symlink + tuple→profile
   map are NEW**.

Net: tool 2 ≈ done (reuse `MakeConf`+`setup.rs`); **tools 1 and 3 are the new
build**, both pure FS setup ⇒ a clean, testable first slice with no resolver
dependency. Sequencing: 1 → 2 → 3, then they wire into `em crossdev --init-target`.

## Stage A/B findings — the real `<CTARGET>-emerge` wrapper (2026-06-21)

Read from the installed `/usr/bin/<CTARGET>-emerge` (the authoritative driver):

```sh
CHOST=<tuple>                       # from argv0
SYSROOT=${BROOT}/usr/${CHOST}       # = /usr/<CHOST>
PORTAGE_CONFIGROOT=${SYSROOT}       # config (profile/make.conf) FROM THE SYSROOT
# CBUILD + BUILD_CFLAGS/CXXFLAGS/CPPFLAGS/LDFLAGS: queried from the HOST
#   (portageq envvar with CHOST/SYSROOT/CONFIGROOT unset)
exec emerge --root-deps=rdeps "$@"
```

- **`ROOT` is NOT set by the wrapper** — it comes from the sysroot `make.conf`
  (`ROOT=/usr/${CHOST}/`). So the sysroot make.conf's `ROOT` matters (our
  EPREFIX-aware `ROOT` write feeds this).
- **`--root-deps=rdeps` is the crux**: only **RDEPEND** is installed into the
  target ROOT (the sysroot); **DEPEND/BDEPEND resolve against the build host
  (`/`)**. This is the inverse of the toolchain case (`host_copies.rs`, which
  pushes host build-copies): here the *bulk* is host build-deps and only runtime
  deps land in the sysroot.
- `BUILD_*FLAGS`/`CBUILD` come from the host config; `C*FLAGS` from the sysroot.

### em gaps blocking target-package cross builds
Probed live (`em -p --config-root /usr/<CHOST> --root /usr/<CHOST> sys-libs/zlib`
→ `NoSolution`/`NoVersions` over `(Unbounded,Unbounded)` on `merge_root: Target`):

1. **FIXED (2026-06-21) — keyword acceptance used the host arch, not the
   target.** The real blocker: `AcceptKeywords::new` was keyed to the global
   `--arch` (host, e.g. arm64), so a target package keyworded `~riscv`/`riscv`
   was filtered out for a riscv sysroot ⇒ every target package = NoVersions.
   Decoded by instrumenting: cpn was `sys-libs/zlib`, `in_data=true`, yet served
   no Target versions; `--arch riscv` made it resolve. Fix (`depgraph/mod.rs`):
   when `cross.active`, derive the acceptance arch from the sysroot `CHOST`
   (`Arch::from_chost`) instead of `--arch`. Now `em -p --config-root <sysroot>
   --root <sysroot> <pkg>` resolves the full target closure into the sysroot with
   no manual `--arch`. (The sysroot `repos.conf` from `--init-target` is still
   wanted so `PORTAGE_CONFIGROOT=<sysroot>` sees the tree, but repo discovery
   currently falls back to the host `ReposConf::load()`, so it was not the gating
   bug.)
2. **FIXED (2026-06-21) — `--root-deps=rdeps` semantics.** A genuine cross-*arch*
   target build now discards the target package's DEPEND (build-only) from the
   sysroot graph, matching crossdev's `<CTARGET>-emerge --root-deps=rdeps`: only
   RDEPEND/PDEPEND install into the sysroot; build deps resolve on the host
   toolchain (BDEPEND was already host-routed). Plumbing: new provider flag
   `root_deps_rdeps` (`set_root_deps_rdeps`), consumed in
   `cross_target_runtime_deps` (drops `by_class[0]`). Gated in `depgraph/mod.rs`
   to `cross_arch != host arch` so same-arch offset/stage builds (`--root
   stage1/`, also `cross.active`) keep DEPEND → target ROOT. **Subtlety:** the
   synthetic solver root's seed targets live in `by_class[0]` and the root reports
   `MergeRoot::Target`, so rdeps must be suppressed for `package.is_virtual()` —
   otherwise it drops the user's requested targets and the whole solve collapses
   to empty (hit this; fixed with the `&& !package.is_virtual()` guard at both
   call sites). Unit test `root_deps_rdeps_drops_target_depend` covers on/off.

   **Also found — empty cross plan = missing target VDB.** If `<sysroot>/var/db/pkg`
   does not exist, the installed loader falls back to the **host** VDB, so
   host-installed packages (zlib, libpcre2…) wrongly satisfy target requests and
   the plan comes up empty. `--init-target` now `mkdir -p`s the empty target VDB
   (`write_sysroot_config`); a fresh sysroot then resolves `[ebuild N ... to
   <sysroot>/]` correctly. (Manual sysroots need the dir too.)

The cross compiler (`<CHOST>-gcc`/`-ld`) is already on `PATH`, so once
resolution is fixed the eclass-driven build should follow. NB: cross
**toolchain** packages (`cross-*/binutils|gcc`) are HOST builds — resolve them
with **host** config (config-root=`/`), NOT the sysroot (that NoSolution is
self-inflicted, see "Host stage1 vs target sysroot" above). Only **target**
packages use the sysroot config + `--root-deps=rdeps`.

**Recommended Stage-B order:** (1) sysroot `repos.conf` in `--init-target`
[DONE, commit 9407925], (2) `--root-deps=rdeps` root routing [DONE — see gap #2;
keyword-arch fix + rdeps drop + target-VDB dir], then **(3) NEXT: the
`<CTARGET>-emerge` wrapper / `em --cross <tuple>` entry point** [Stage A
ergonomics] + verify one leaf target lib actually builds and `file` reports the
target arch [Stage B build shell].

## Implementation stages

### Stage A — cross entry point (`{target}-emerge` equivalent) — DONE (2026-06-21)
Implemented as a global `--cross <tuple>` flag (chose the flag over argv0
`<tuple>-emerge`: friendlier, and em has no per-target symlinks). It is **sugar**
over the existing root model: `Cli::roots()` layers the cross sysroot
`<EROOT>/usr/<tuple>` on top of `base_roots()` as `config == base == target`
(crossdev's `PORTAGE_CONFIGROOT == ROOT == SYSROOT`). `<EROOT>` still comes from
`--local`/`--prefix`/`--root`, so `em --local --cross <t>` targets
`~/.gentoo/usr/<t>`. CHOST/CBUILD + `--root-deps=rdeps` then fall out of the
existing `root_aware::detect` (reads the sysroot make.conf) — no extra plumbing.
`run_emerge` pre-flights the sysroot (`<sysroot>/etc/portage/make.conf` exists)
and otherwise bails with `run: em crossdev -t <tuple> --init-target`. Tests:
`cli::tests::cross_*`. Verified live: `em --root <eroot> --cross <t> -p zlib`
→ `[ebuild N] ... to <eroot>/usr/<t>/`, header shows
`CHOST=riscv64-… CBUILD=aarch64-…`.

**Correction (the build shell already sources our confdir):** the build-time env
is NOT a separate Stage-B task — it falls out of the existing merge path once
`--cross` points config/root at the sysroot:
- `ebuild.rs` `apply_profile_env(config_root=…/usr/<tuple>)` sources the sysroot
  make.defaults chain + make.conf, so phases see **CHOST, CBUILD, target
  CFLAGS/LDFLAGS, ABI, USE_EXPAND** — straight from the make.conf `--init-target`
  wrote. The cross `package.env`/`env/*.conf` is sourced too.
- `set_build_roots(config_root, build_sysroot=None, eprefix=None)`: with
  `config == base == target` (build_sysroot None), `shell.rs` sets
  `SYSROOT = ESYSROOT = ROOT = <sysroot>` and `BROOT = "/"` — exactly crossdev's
  `SYSROOT=ESYSROOT=ROOT=/usr/<CHOST>`, `BROOT=/`. `econf` then passes
  `--host=$CHOST --build=$CBUILD`.
- `${CHOST}-gcc` (`tc-getCC`) is already on the host PATH from the installed
  `cross-<tuple>/gcc`.

So Stage B is not env wiring — it is just *running a real build and verifying*:
the toolchain is installed, the compiler is actually invoked cross, and the
artifact is the target arch (`file`). Watch for the genuine gaps: BDEPEND build
tools resolving on BROOT (already handled by the dual-root solver + `--root-deps
=rdeps`), and the cross toolchain PATH when it is NOT in `/usr/bin` (LLVM
`--target`/`--sysroot` model).

### Stage B — cross builder (one leaf target package) — DONE (2026-06-21)
Verified end-to-end: `em --cross riscv64-unknown-linux-gnu sys-libs/zlib`
produces a real **RISC-V** `libz.so` (`file` → `ELF … UCB RISC-V, double-float
ABI`) merged into the sysroot, on an aarch64 host with the crossdev toolchain
installed.

The one real gap the build flushed out (everything else was already wired):
- **Toolchain selection.** The env wiring (CHOST/CBUILD/CFLAGS from make.conf,
  SYSROOT=ESYSROOT=ROOT, BROOT=/) was all correct, but the build still used the
  **host `gcc`** → a host-arch artifact. Root cause: `tc-getCC` only exports
  `CC=${CHOST}-gcc` when something calls it, and for a *single-ABI* target the
  multilib `DEFAULT_ABI` path skips `multilib_toolchain_setup`'s CC export, so an
  ebuild that builds with a raw `./configure` (zlib) never gets the cross CC.
  Diagnosed with a probe ebuild: CHOST **did** propagate (`CHOST=riscv64-…`), so
  it was purely CC selection. Fix (`portage-repo` `shell.rs::init_build_env`): when
  cross (CHOST≠CBUILD, both set) and `${CHOST}-gcc` is on PATH, proactively export
  `CC/CXX/AR/NM/RANLIB/STRIP/OBJCOPY/OBJDUMP/READELF/LD = ${CHOST}-<tool>` unless
  already set — em's standing-in for `tc-getCC`. Native builds (CBUILD unset or
  ==CHOST) untouched.
- Header note: the cross gcc's baked-in `--sysroot` (`/usr/<tuple>`) already has
  the headers/libc, so a clean `--cross` build (SYSROOT==ROOT==populated sysroot)
  resolves `sys/types.h` etc. A split `--config-root X --root Y(empty)` leaves
  SYSROOT empty and fails — not a bug, just don't point ROOT at an unpopulated
  tree for a from-scratch build.

Still TODO for Stage B breadth:
- **LLVM**: host `clang --target=<triple> --sysroot=$ESYSROOT`, `lld` — needs
  wiring (no `<triple>-clang` unless `clang-crossdev-wrappers` is installed); the
  GCC `${CHOST}-<tool>` export above does not cover the clang model.
- A leaf with real deps (confirm RDEPEND libs resolve from the sysroot and
  BDEPEND tools from BROOT during an actual build, not just `-p`).

## `em crossdev --setup` — full cross environment (design, 2026-06-21)

The full cross `--setup` is **two phases** (the user's two items), which crossdev
+ crossdev-stages keep terminologically distinct (`docs/design.md`):

1. **Toolchain creation** → the **crossdev prefix** `/usr/<chost>` on the **host**
   (`ROOT=/`, config from host). The cross compiler + headers + stage1 libs. This
   is "Stage C" below (the staged binutils→headers→gcc1→glibc→gcc2 bootstrap).
   *Not* a sysroot — "sysroot" is reserved for the `--sysroot` compiler flag.
2. **Sysroot/target-stage creation** → the **target stage** rootfs at `ROOT=/target`,
   built with `em --cross <tuple>` (= `<chost>-emerge`). Two sub-modes mirroring
   catalyst: **stage1** (bootstrap: `baselayout` `USE=build` → `packages.build` →
   `portage`) and **stage3** (`@system`/`@world`). Optionally **seeded** from a
   target-arch stage3 tarball (the unused `gentoo-stages` downloader).

**Why "both share the stage1/stage3 problems":** each is an *ordered build into a
root the solver can't fully resolve against* (nothing installed yet), so neither
is pure solver output. The shared problems, from `cross-stage.sh`:
- **Ordered, curated build lists** with explicit bootstrap order (not a plain atom
  set) — the chicken-and-egg (headers-only, `--nodeps`, two-stage gcc/glibc).
- **`USE=build`** minimal pass for `baselayout`/`portage` (and stage1 gcc USE).
- **Binpkg** (`-b -k`) to cache+reuse across the staged passes.
- **Per-root profile selection** (`eselect profile set` on each root).
- **`merge-usr --root`** (crossdev prefix starts split-usr).
- **The `--sysroot=$EROOT` LDFLAGS workaround** for hosttools like `perl`.

**The shared abstraction em needs — an ordered `StagePlan` driver.** A `StagePlan`
is `Vec<StageStep>` where `StageStep { atoms, use_override, env_tag, nodeps,
binpkg, root }`; the driver runs each step through the *existing* build/merge path
(the one Stage B verified) against the step's root. Both phases are `StagePlan`
templates:
- **toolchain plan** → host config, `ROOT=/`, installs into `/usr/<chost>` via the
  `cross-*` overlay category; the GCC/LLVM stage list with per-stage `USE` + the
  `*-stage1`/`*-headers` env tags (`write_cross_env` already lays some down).
- **sysroot stage1 plan** → `--cross` (config/root = target), `USE=build`
  baselayout→packages.build→portage; **stage3 plan** → `@system`/`@world`.

**Reuse map (already in em):** the build shell + cross toolchain selection (Stage
B), `--cross` entry point, `--root-deps=rdeps`, `--init-target` FS setup, the
completed `Solver` trait, and `gentoo-stages` for the seed tarball. **New:** the
`StagePlan` type + driver, the two templates, and `-b/-k` binpkg + `USE=build`
plumbing in the driver.

**Proposed CLI surface** (em owns the build engine + standalone path; crossdev-
stages stays the rootless-sandbox/image orchestrator that calls em):
- `em crossdev <tuple> --setup` (alias `--stage4`): toolchain bootstrap into the
  prefix. `--stage0..--stage3` stop earlier (binutils-only … libc).
- `em crossdev <tuple> --sysroot[=stage1|stage3] [--seed <stage3.tar>] --root DIR`:
  build the target stage. (Or fold under `em --cross <tuple> --stage1`.)

**Implementation sequence:** (1) `StagePlan`/`StageStep` + driver over the existing
merge path; (2) `--nodeps` + per-step `USE` override + `-b/-k` plumbing; (3) the
toolchain template (Stage C list) → `--setup`; (4) the stage1/stage3 templates →
`--sysroot`; (5) seed-from-stage3 via `gentoo-stages`. Stage 1+2 are the shared
core both phases depend on — build it first.

**Progress (2026-06-21):** `crossdev/stages.rs` landed the `StagePlan`/`StageStep`
types + `toolchain_plan()` (the GCC two-stage + LLVM-runtimes templates, with the
`headers-only`/`--nodeps`/per-stage gcc USE), unit-tested. `em crossdev <t> --setup`
runs `--init-target` then prints the ordered plan. **Remaining:** the *driver* —
execute each step through the resolve+merge path (per-step `USE` override,
`--nodeps`, `-b/-k`), which is sequence item (1)+(2). `--setup` is plan-only until
then. Decision recap (user): `--setup` is one intertwined bootstrap (compiler
needs the cross libc to work); only *after* a valid compiler exists are toolchain
update vs target-stage build nearly independent. Toolchain template was built
first.

**Smoke test (2026-06-22) — `em --local crossdev -t riscv64-unknown-linux-gnu
--setup` now runs END TO END (`EXIT=0`, all 6 steps: binutils → kernel-headers →
libc-headers → gcc-stage1 → libc → gcc-stage2).** Three bugs found and fixed to
get gcc-stage1 through build+install:

1. **Per-step `USE=-flag` did not override a `+flag` IUSE default** — commit
   `fix(use): let an explicit USE=-flag override a +flag IUSE default`. The driver
   injects stage USE (`-cxx -fortran …`) via the process `USE` env var. The global
   USE resolution flattened to an *enabled-only* set, so a `-cxx` whose flag was
   never globally enabled (cxx is a per-package default, not a profile flag) was
   dropped — "merely absent" — and `fold_iuse_defaults` re-enabled the `+cxx`
   default (`-openmp` worked only because openmp *is* a profile flag). gcc-stage1
   thus configured `--enable-languages=c,c++,fortran` and tried to build the
   **target** `libstdc++-v3`/`libbacktrace` against the *headers-only* glibc →
   `C compiler cannot create executables`. Fixed by tracking explicit disables
   through resolution (`merge_flag_lists_signed` → `ResolvedUse.disabled` →
   `UseFlagState::Disabled`). `em -pv` now shows `USE="-cxx -fortran -openmp"`,
   configure → `--enable-languages=c`. (Rejected: writing a scoped `package.use`
   per step — brittle file churn; the env var with correct precedence is the
   portage-faithful mechanism.)
2. **`emake -f -` lost piped stdin** — commit `fix(emake): forward the pipeline's
   stdin …`. toolchain.eclass `get_make_var`/`XGCC`
   (`echo -e "…include $makefile" | emake -s -f -`, in `gcc_movelibs`) saw no
   makefile → `make: No targets. Stop.` → `$(XGCC)` empty → `command not found:
   -print-multi-lib` / `-mabi=lp64d`. The `emake` builtin wired stdout/stderr but
   not stdin; added `context_stdin`.
3. **Unprivileged root chown** — commit `fix(build): tolerate root chown/chgrp …`.
   `toolchain.eclass: chown -R 0:0 "${LIBPATH}" || die` fails EPERM as uid 1000
   (no fakeroot) → bare `die`. Added fakeroot-style `chown`/`chgrp` shims that
   tolerate failure only when not root. (Only 2 eclasses do raw root chown:
   toolchain + kernel-2; everything else uses `fowners`/`fperms`.)

Also fixed: `-p`/`-a`/`-D` were not `global = true` in clap, so they were rejected
*after* a subcommand (`em … crossdev … --setup -p`). Now global.

**REMAINING — toolchain not yet *functional* (2 gaps, found 2026-06-22 via a
clean-slate reinstall).** A clean reinstall (wiped the cross VDB + install trees,
re-ran `--setup`) rebuilt **all** packages from scratch (steps 1–4 each `merged 1`),
but the toolchain still can't compile (`riscv64-…-gcc /tmp/t.c` → host `as:
invalid option -- 'p'`; no `libc.so`/`libc.a` in the sysroot). Two driver bugs:

1. **Two-stage same-version steps are SKIPPED.** Step 3 installs
   `glibc-2.43-r2[headers-only]`, step 4 `gcc-15.3.1[stage1]`. Steps 5 (full glibc)
   and 6 (gcc-stage2) are the **same CPV**, so `run_merge_plan`'s resume logic
   (main.rs ~335: "recorded at the planned version → skip") skips them — `>>> …
   already installed — skipping`. The two-stage rebuild (headers-only→full,
   stage1→stage2) is the *whole point* and must force. The skip ignores USE; the
   right fix is USE-aware (`--newuse`-style: reinstall when the installed USE
   differs from the planned USE), which also benefits normal merges. Driver-local
   alternative: a `StageStep.force`/emptytree-for-atom flag, but USE-aware skip is
   cleaner and not crossdev-specific. **This is the top blocker.**
2. **binutils not activated — WRAPPER CREATION + `--setup` WIRING DONE
   (2026-06-24).** Two pieces were missing under one heading:
   - `em select {binutils,compiler} set` only wrote `config-<target>` + the env.d
     global file; the **`/usr/bin/<CTARGET>-*` wrapper symlinks were never created**
     (the doc claimed them; the code didn't). Now `EnvDProfile::install_wrappers`
     replicates `binutils-config` (two-level `usr/libexec/gcc/<T>/<tool>` →
     `binutils-bin/<VER>`, then `usr/bin/<T>-<tool>`) and `gcc-config`
     (`usr/bin/<T>-<tool>` → `<GCC_PATH>/<T>-<tool>` + `<T>-cc`), all rooted at
     `<EPREFIX>` so `--local`/`--prefix` link their own binaries. See
     [[select-toolchain]]. Unit-tested.
   - `crossdev --setup` now calls `select::activate_{binutils,compiler}` after the
     binutils/gcc steps (`activate_toolchain`, EPREFIX-aware), so the prefix gets
     activated instead of the host `/` that the eclass `pkg_postinst`'s
     `binutils-config` targets. Activates the newest profile built into this root;
     no-op under `-p`.
   **Build-PATH exposure — TRACED + FIXED (2026-06-25).** `init_build_env`
   (`shell.rs:1062-1076`) rebuilds PATH from the process PATH but **strips every
   `$HOME` path** — and `~/.gentoo/usr/bin` is under `$HOME`, so the freshly
   installed wrappers are removed. The only thing that re-adds it is the `--local`
   prefix `bashrc` hook (`setup.rs::BASHRC_LOCAL`: `export
   PATH="${_ov}/usr/bin:${PATH}"`), loaded into the build shell by
   `ebuild.rs` (config-overlay bashrc). But `crossdev::init_target` never wrote
   that bashrc — only `em setup` did. Fix: `init_target` now calls
   `setup::bootstrap(&roots)` when `EROOT != /` (idempotent), so `em --local
   crossdev --setup` lays down the prefix `bashrc`+skeleton and the wrappers in
   `~/.gentoo/usr/bin` are on the gcc-stage build PATH. Verified `--prefix`
   bootstrap fires (writes BASHRC_PREFIX, no PATH hook — by design, ROOT-offset
   binaries shouldn't shadow host tools).
   **STILL OPEN:** (a) live `em --local crossdev --setup` end-to-end →
   `<CTARGET>-gcc hello.c` → `file` = RISC-V; (b) two latent edges — `--prefix`
   cross has no PATH hook for its cross gcc, and the cross-CC auto-export gate
   (`shell.rs:1124` `program_on_path`) reads the **process** PATH not the
   bashrc-extended shell PATH, so on a host with no system crossdev toolchain the
   prefix's `<CTARGET>-gcc` wouldn't trip the gate. Both fine on this dev box
   (system crossdev present); revisit if testing on a clean host.

Completion criterion (unchanged): `<CTARGET>-gcc hello.c` → `file a.out` reports
`ELF … RISC-V`. The *build* pipeline is done; these two are merge/activation.

### libc step (5) now BUILDS — root cause was a brush `export VAR+=` bug (2026-06-25)

The full cross glibc (step 5) had been dying with `glibc cannot be compiled
without optimization`: `setup_flags` emptied CFLAGS, so the final value was just
`-Wno-unused-command-line-argument` (no `-O`). Earlier suspicion fell on the
multilib CFLAGS weave and on flag-o-matic's `strip-unsupported-flags` stripping
`-O3` — both were **wrong leads**. The multilib `package.env` is correct
(`ABI=lp64d`, `CFLAGS_lp64d='-mabi=lp64d -march=rv64gc'`, committed in 09f03d2),
and a logging-CC probe inside the real build proved `strip-unsupported-flags`
**keeps** `-O3` (the `-Werror -O3 -xc -c` probe returns rc=0).

Root cause: **brush mis-parsed `export NAME+=value` as a plain assignment**,
discarding the prior value. flag-o-matic's `append-cflags` is literally
`export CFLAGS+=" $*"` (flag-o-matic.eclass:310), so glibc's
`append-flags -Wno-unused-command-line-argument` (the *last* line of
`setup_flags`) wiped `-O3 -pipe` and left only the appended flag. A bare
`NAME+=value` appended fine; only the `export`/declaration form was broken —
which is why it stayed invisible until a cross libc exercised it. Minimal repro:

    X="-O3 -pipe"; export X+=" -Wno-foo"   # brush: X=[ -Wno-foo]   bash: X=[-O3 -pipe -Wno-foo]

Fix in the brush fork (`brush-builtins/src/export.rs`): the parsed-`Assignment`
branch ignored `assignment.append`; the runtime-split `String` branch already
honored it. Now, when `assignment.append` and the variable exists, it does
`base_var_mut().assign(value, true)` (mirrors `declare`/`local`); a missing
variable falls through (append-to-nothing == plain assign). Added three
`export.yaml` compat cases (append to existing / already-exported / unset); full
`brush-compat-tests` = 2097 passed, 0 failed. em picks up the fork via
`.cargo/config.toml` `[patch]`.

Result: `em --local --nodeps cross-riscv64-unknown-linux-gnu/glibc` → EXIT=0,
CFLAGS now `-O3 -pipe -Wno-unused-command-line-argument`, and
`usr/riscv64-unknown-linux-gnu/lib64/libc.so.6` =
`ELF 64-bit LSB shared object, UCB RISC-V, RVC, double-float ABI`. **The libc
step is no longer a blocker.** (Brush fork fix not yet pushed / rev not yet
bumped in `portage-repo/Cargo.toml` — do that before CI relies on it.)

Two non-fatal merge-time errors surfaced after a successful build (cosmetic,
track separately): the OLD glibc's `pkg_prerm` failed —
`environment.old: syntax error at end of input` then `command not found:
pkg_prerm` (the saved env of the replaced package doesn't reload cleanly in the
carried shell), and a postinst `failed to redirect to ~/.gentoo//etc/hosts`
(getent/nscd touching a nonexistent prefix `/etc/hosts`). Neither stopped the
merge.

### NEXT blocker: missing `lib64/lp64d -> .` osdir compat symlink — ROOT-CAUSED + FIX VERIFIED (2026-06-25)

With libc installed, `riscv64-...-gcc hello.c` fails at *link*: `ld: cannot find
Scrt1.o`. Root-caused by comparing against the working **system** crossdev
toolchain (identical sysroot layout, gcc-16 links fine):

- gcc (both ours and the system's) searches `<sysroot>/usr/lib64/lp64d/` for the
  CRT/libc (it's in the gcc-config `MULTIOSDIRS`; `gcc-abi-map` maps riscv
  `lp64d → lp64d`).
- glibc installs the CRTs/libc to **bare** `<sysroot>/usr/lib64/` — because
  `multilib_env` gives the **DEFAULT_ABI** the un-suffixed libdir
  (`LIBDIR_lp64d=lib64`, while non-default `lp64 → lib64/lp64`). glibc's
  `src_install` uses `get_abi_LIBDIR ${DEFAULT_ABI}` and never makes the
  suffixed dir; with `SYMLINK_LIB=no` no `lib`-style symlink is made either.
- The working system sysroot bridges the gap with two **untracked** compat
  symlinks (not in any VDB CONTENTS, not made by the crossdev script, glibc, or
  an obvious gcc-ebuild line): `usr/lib64/lp64d -> .` and `lib64/lp64d -> .`.

**Verified fix:** creating those two symlinks makes the *existing* gcc-stage1
compile **and link** `hello.c` → `ELF 64-bit … UCB RISC-V` PIE executable — the
completion criterion, met. (And it's what unblocks the gcc-stage2 self-build,
whose `xgcc` hit `configure: error: Link tests are not allowed after
GCC_NO_EXECUTABLES` for the same reason — it couldn't link a target executable.)

The symlink was necessary but **not sufficient**: with it present, the *installed*
gcc links, but a clean `gcc-stage2` self-build still died at the target
`libbacktrace` configure (`GCC_NO_EXECUTABLES`). An `ld`-shim trace cracked the
*real* second bug — see below.

**Second root cause: em set `ESYSROOT` wrong for `cross-*` builds — FIXED
(2026-06-25).** The build-tree `xgcc` was passing `--sysroot=~/.gentoo` (the
EPREFIX) to `ld`, so the target CRT/`libc` were searched under the *host*
`~/.gentoo/usr/lib`. Source: `toolchain.eclass:1537` passes
`--with-build-sysroot="${ESYSROOT}"`, and em computed `ESYSROOT = SYSROOT+EPREFIX
= ~/.gentoo` for the cross gcc build (SYSROOT stays host `/`). For a
`cross-<tuple>/*` package the target deps live in the **cross sysroot**, so
`ESYSROOT` must be `${EPREFIX}/usr/<tuple>`. Fixed in `shell.rs` (the `ESYSROOT`
computation special-cases a `cross-` `CATEGORY`); `SYSROOT` stays host so the gcc
host parts still build natively.

**Both fixes landed; toolchain BUILDS END-TO-END (2026-06-25):**
- `shell.rs`: `ESYSROOT = ${EPREFIX}/usr/<tuple>` for `cross-*` packages.
- `crossdev/mod.rs`: `link_abi_osdirs()` creates `<sysroot>/{,usr/}<LIBDIR_default>/<DEFAULT_ABI> -> .`
  after the libc step (the default ABI's bare-libdir → suffixed-osdir bridge),
  using `multilib::query`.

Result: `em --local --nodeps cross-…/gcc` (gcc-stage2) → EXIT=0, installs, and
`riscv64-unknown-linux-gnu-{gcc,g++}` compile **C, C++ (dynamic + static)** to
`ELF 64-bit … UCB RISC-V` executables. **Completion criterion met.**

**ESYSROOT scope fix (regression caught in the first full `--setup`):** the cross
sysroot ESYSROOT must apply to **host cross-tools only** (`binutils`/`gcc`/`gdb`/
`clang-crossdev-wrappers`). A blanket `cross-*` rule broke the **glibc** step —
glibc builds `--with-headers=${ESYSROOT}$(alt_prefix)/usr/include` with
`alt_prefix=/usr/<tuple>`, so ESYSROOT=cross-sysroot **doubled** the offset
(`…/usr/<tuple>//usr/<tuple>/usr/include`) and the kernel-header check failed.
Target packages keep the standard `ESYSROOT=SYSROOT+EPREFIX`. (Same split as
[`is_target_package`].)

**END-TO-END VALIDATED (2026-06-25):** a full clean `em --local crossdev -t
riscv64-unknown-linux-gnu --setup` (osdir symlinks removed first) → `SETUP_EXIT=0`,
all 6 steps from scratch (binutils → kernel headers → libc headers → gcc-stage1 →
libc → gcc-stage2), em creates the osdir symlinks in-flow after the libc step
(`osdir compat: …/lib64/lp64d -> .`), `>>> cross toolchain … ready`. The
freshly-bootstrapped `riscv64-unknown-linux-gnu-{gcc,g++}` compile C and C++ to
`ELF … UCB RISC-V` executables. **The crossdev GCC bootstrap is complete.**

### NEXT: cross-emerging *target* packages (`--cross`) — two issues found (2026-06-25)

First real test of the `<CTARGET>-emerge` path: `em --local --cross
riscv64-unknown-linux-gnu sys-apps/less -p` resolves the full chain correctly
(zlib, ncurses, readline, bzip2, libpcre2, less → all into the sysroot,
`CHOST=riscv64 CBUILD=aarch64`).

**ROOT CAUSE + FIX (2026-06-25, `3907b91`): the prefix cross toolchain wasn't on
the `--cross` build PATH.** The `<chost>-*` wrappers (`crossdev --setup`) live in
`<EROOT>/usr/bin` = `~/.gentoo/usr/bin`, which is under `$HOME` and so stripped by
em's build-PATH sanitiser; and `--cross` sets `eprefix=None` (cli.rs:295,326) so
the prefix bashrc PATH hook never runs. A debug dump confirmed the cross build's
PATH was `.em-helpers:/usr/bin:/opt/bin:/usr/lib/llvm/*` with **`CC=[]`** and
`riscv64-…-gcc` not found. zlib then either fell back to host gcc or its
configure test compile SIGSEGV'd (red herring — the gcc is fine; the prior
`.default`+`.lp64d` "double pass" was also a misread of an **accumulated**
build.log across my reruns: a clean run does a single `.lp64d` pass, and
`MULTILIB_ABIS=lp64d` *is* set correctly from the sysroot's `lp64d` profile).

Fix in `shell.rs`'s cross-toolchain block: when `CHOST≠CBUILD` and the
`<chost>-gcc` wrapper exists at `build_config_root` (the sysroot
`<EROOT>/usr/<tuple>`) grandparent `bin`, prepend that dir to the build PATH and
set `CC/CXX/AR/…/STRIP` to the **absolute** `<bin>/<chost>-<tool>`. Absolute
because em's own post-`src_install` estrip runs the tool from em's process (host
PATH, no `$HOME` prefix bin) — a bare name failed with
`riscv64-…-strip: No such file or directory`. Host crossdev (toolchain in
`/usr/bin`) keeps the bare `${CHOST}-<tool>`.

Result: `em --local --cross riscv64-unknown-linux-gnu sys-libs/zlib` → EXIT=0,
1 object stripped, `libz.so.1.3.2 = ELF … UCB RISC-V, RVC, double-float ABI`,
merged into the sysroot VDB.

**Chain verified + a second bug found (2026-06-25, `13c7599`).** With the PATH
fix the `less` chain cross-builds `ncurses readline editor-wrapper bzip2`
(libpcre2 only stopped on a transient **distfile fetch** failure, not cross). The
one real blocker was `ncurses[cxx]`'s `libncurses++.so`:
`relocation R_RISCV_TPREL_HI20 … recompile with -fPIC` — it pulled the **static**
`libstdc++.a` because the cross gcc had **only** static libstdc++. Root cause: an
em `has_version` bug — `best_match` matched cpn/version/slot and **ignored the
`[...]` USE-dependency**, so toolchain.eclass's
`has_version "${needed_libc}[headers-only(-)]"` matched the *full* glibc as
headers-only and configured gcc `--disable-shared`. Fixed: `has_version`/
`best_version` now evaluate USE-deps against the installed VDB USE/IUSE
(PMS 8.3.4, with `(+)/(-)` defaults), unit-tested. After the fix the cross gcc
builds `--enable-shared`; `g++ -print-file-name=libstdc++.so` resolves the shared
lib, and C++ shared libraries cross-compile.

Remaining cross-emerge polish (lower priority): the cosmetic merge-time
`pkg_prerm`/`environment.old` errors on replaced packages; `--prefix` (non-local)
cross validation; and surfacing a proper `<CTARGET>-emerge` wrapper.

**Progress (2026-06-22) — toolchain-package build shell VERIFIED; eclass
resolution corrected.** Two findings while bringing up the Stage-C driver:

- **The cross *toolchain* package build shell works** (not just Stage-B leaf
  libs). `cross-riscv64-unknown-linux-gnu/binutils` cross-*configures* correctly:
  `CATEGORY=cross-riscv64-…`, `CTARGET=riscv64-unknown-linux-gnu`, `configure`
  reports the riscv64 target triple and finds every `riscv64-unknown-linux-gnu-*`
  tool, with **no `tc-arch: command not found`**. This confirms the KEY
  ARCHITECTURAL INSIGHT for toolchain packages: the original `tc-arch` failure
  was the `cross-*` category-collapse bug, fixed by the canonical-path change
  (commit 33cddfc) — `Ebuild::from_path` now derives the CPV from the *given*
  (overlay) path so `CATEGORY` stays `cross-*`, while storing the *canonical*
  path so `repo_root` resolves to gentoo and eclasses are found. So the staged
  bootstrap is blocked only on the **driver**, not the build shell.
- **`em ebuild` now resolves master-repo eclasses** (was: only the repo's own
  `eclass/`). A standalone overlay with `masters = gentoo` (not a `cross-*`
  symlink into gentoo) previously failed `inherit flag-o-matic` with "eclass not
  found". `ebuild.rs::run_inner` now resolves masters by name via repos.conf
  (sibling-dir fallback; unresolvable masters skipped with a warning) and uses
  `shell_with_masters`. The `cross-*` path didn't need this (symlinks ⇒
  `repo_root` is already gentoo) but it makes plain overlays correct and the
  cross overlay robust regardless of layout.
- **Eclass search-path precedence fixed to match portage.** `shell_with_masters`
  used to *prepend* master eclass dirs, so a master's eclass won over the repo's
  own on a name conflict — the inverse of portage. Portage builds
  `eclass_locations = [master1, …, masterN, own]` with last-writer-wins
  (`repository/config.py` + `eclass_cache.py`), i.e. **own repo > masterN > … >
  master1**. em now *appends* masters in reverse order so its first-hit-wins list
  is `[own, masterN, …, master1]` — same precedence. Regression test
  `eclass_search_path_prefers_own_repo_over_masters`. (Affects `em regen` too,
  which shares `shell_with_masters_and_cache`.)

### Stage C — toolchain bootstrap (the real crossdev workflow) — LARGE
NB: Stage B built a **leaf lib against an already-bootstrapped toolchain**. The
toolchain itself needs crossdev's staged bootstrap, which em does NOT yet do —
it is the chicken-and-egg part (a compiler needs a libc, a libc needs a
compiler). em's plain dependency solver can't express it; it needs an explicit
**ordered bootstrap driver** with per-stage USE/env, mirroring crossdev.

**Canonical crossdev GCC sequence** (`/usr/bin/crossdev` `doemerge` loop,
lines ~1939-2049), staged `is_s0..is_s4`:
1. **binutils** (`USE=${BUSE}`) — the assembler/linker, no compiler yet.
2. **stage1 (bare C compiler)**: first **kernel-headers** (`headers-only`) then
   **libc headers** — glibc with `USE="headers-only"` **and `--nodeps`** to break
   the glibc→newer-gcc dep cycle (`${LPKG}-headers`). Then **gcc-stage1**
   (`${GPKG}-stage1`) with `GUSE_DISABLE_STAGE_1 = -fortran -d -go -jit -cxx
   -openmp -sanitize -zstd -zlib …`: a freestanding C compiler, no libc.
3. **stage2 (kernel headers)**: full `linux-headers` (`${KPKG}-stage1/stage2`).
4. **stage3 (libc)**: full **glibc/musl**, compiled by gcc-stage1.
5. **stage4 (full compiler)**: **gcc-stage2** (`${GPKG}-stage2`,
   `GUSE_DISABLE_STAGE_2 = … -sanitize`) linked against the just-built libc; the
   final `<triple>-gcc`. (LLVM model: `compiler-rt` at stage1, then
   `libunwind`/`libcxxabi`/`libcxx` at stage4 — no two-stage gcc.)

Two bootstrap tricks em must reproduce: **two-stage glibc** (headers-only →
full) and **two-stage gcc** (stage1 no-libc → stage2 full). crossdev expresses
the per-stage build via `USE="…" doemerge ${PKG} ${PKG}-stageN`, where the
`-stageN` tag selects a `/etc/portage/env/<cat>/<pkg>` file crossdev wrote
(`*-stage1`/`*-headers`/`*-quick`). em equivalent: drive the same ordered list
with per-step USE overrides + the cross env files (`write_cross_env` already lays
some down), gated `--root-deps=rdeps` + `--nodeps` where crossdev uses them.

**LLVM first** (simpler, no two-stage gcc): headers → target libc (glibc/musl) +
`compiler-rt`/`libunwind`/`libc++` into the sysroot, all with host clang.

Decide how em expresses "build me a `<triple>` toolchain" — a `--toolchain`/
`--stageN` mode on `em crossdev` (closest to crossdev) vs a set/meta target
(`@cross-toolchain`). Leaning `em crossdev <t> --stage4` driving the ordered
list, since the per-stage USE/env doesn't fit a plain atom set.

### Stage D — true dual-root scheduling — LARGE, deferred
Independent `PackageData` per root so a CPV needing both host-native and
target-cross builds is two plan entries (`root-model.md` § Cross). The post-solve
host walk (`host_copies.rs`) covers the common case; only revisit if a real cross
build needs the same CPV on both roots.

## LLVM/Clang cross specifics (the "better llvm/clang" ask)

- Treat the **LLVM path as first-class**, arguably the default for new targets:
  no per-target compiler build, just sysroot population with host clang.
- Clang multi-version (20/21/22) is installed; pick the active one via the
  existing `llvm_slot`/`LLVM_SLOT` machinery (already handled in USE_EXPAND /
  Level-C work).
- `clang-toolchain-symlinks` / `clang-linker-config` already provide the
  `<triple>-clang` symlink + linker wiring — reuse rather than reinvent.
- baremetal/`-elf` targets (e.g. `riscv64-unknown-elf`): no kernel headers / no
  full libc — LLVM (compiler-rt + picolibc) is the natural fit.

## "ex-pkg" — resolved: it's crossdev's `--ex-pkg`

`--ex-pkg <pkg>` (crossdev `--help`, "Extra Fun" section, with `--ex-gcc`/
`--ex-gdb`) builds **extra packages onto an already-established cross target**,
after the toolchain stages. In the script it is just:
```
for pkg in "${XPKGS[@]}" ; do doemerge "${pkg#*/}" ; done   # = emerge cross-<CTARGET>/<pkg>
```
i.e. nothing special — the same per-package cross build as everything else, run
after stage4. So "support ex-pkg" == "em can build an arbitrary package into an
existing `<CTARGET>` sysroot", which is exactly Stage A+B below (the
`<CTARGET>-emerge <pkg>` path). `--ex-gcc`/`--ex-gdb` are the same with the
gcc/gdb atoms (`GPKG`-extra / `DPKG`).

## Sequencing / first steps

1. Validate current cross `-p` on this host (`riscv64` gcc = 18 pkgs) — baseline.
2. **Stage A** — cross entry point (cheap unlock).
3. **Stage B** — cross-build ONE leaf target package (LLVM path first), verify
   the artifact arch.
4. **Stage C (LLVM)** — populate a `<triple>` sysroot (target libc + runtimes).
5. **Stage C (GCC)** — the staged binutils/gcc/glibc bootstrap.
6. Stage D only if a concrete build needs it.

## Coordination

Mostly cli/build path (`ebuild.rs`, `root_aware.rs`, `overlay.rs`, cli entry) +
config. Touches the resolver only lightly (per-class root routing is already in
place via Tier-1). Keep Stage A/B independent of the resolver-abstraction work.
