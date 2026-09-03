# Root model: `--root`, `--prefix`, and the location variables

`em` operates on **three independent roots** that portage also distinguishes
(PMS + `PORTAGE_CONFIGROOT`). Conflating them is a bug; keeping them orthogonal
is what lets one code path serve host management, offset/stage builds,
unprivileged `.local` overlays, and (eventually) crossdev.

| concept | PMS / portage var | governs | `${X}/…` paths |
|---|---|---|---|
| **config root** | `PORTAGE_CONFIGROOT` | profile, make.conf, USE, CFLAGS | `etc/portage/{make.profile,profile,make.conf}` |
| **base root** | (the planner's "installed" view) | what counts as already installed | `var/db/pkg` |
| **target root** | `ROOT` / `EROOT` | where new files install + the new VDB | `var/db/pkg`, install dest |
| **sysroot** | `SYSROOT` / `ESYSROOT` | where build-time `DEPEND` is found | headers / libs / `.pc` |
| (build tools) | `BROOT` | where `BDEPEND` tools run | host `/` for `--root`/`--prefix`/bare; the prefix itself for `--local` (see below) |

## The two user-facing flags

- **`--root R`** (default `/`, env `ROOT`): the **base** — the root whose VDB
  the planner reads as "already installed." Does **not** move the config
  source (matches real portage: `ROOT=` never touches `PORTAGE_CONFIGROOT`).
- **`--prefix P`** (default `= R`): the **install destination** for newly
  merged packages — the *delta* root. Also does not move the config source.

**`--config-root C`** is the only flag that moves the config source; without
it, config always comes from host `/` — the one exception is `--local`,
which prefers an already-bootstrapped prefix's own profile over the host
(still overridden by an explicit `--config-root`; see the Scenarios table
below). **`--vdb V`** overrides the base VDB path only.

Canonical form is `em [applet] [options] [args]`. Topology flags
(`--prefix`/`--local`/`--config-root`/`--vdb`/`--target`) and true globals
may also appear before a named applet (`em --prefix P toolchain` parses).
`--root` after `crossdev` is a parse error; prefix `em --root R crossdev`
is rejected. `em toolchain`/`em stages`/`em setup`/`em emerge` (and the
bare `em <atoms>` form) all still take `--root`. Prefix emerge-mixins
before a non-merge applet (`em -uD query …`) are rejected.

### Derived values

```
config_root   = --config-root || /                    # PORTAGE_CONFIGROOT — --root/--prefix never move this
base_root  R  = --root || /                            # planner "installed" base
target P      = --prefix || --root || /               # ROOT/EROOT: install dest + new VDB
base_vdb      = --vdb || R/var/db/pkg
planner installed = VDB(R) ∪ VDB(P)                   # P shadows R; equal ⇒ just one
merge into    = P
sysroot search = [P, R]                                # ordered, P wins; equal ⇒ [R]
broot         = / (--root/--prefix/bare)  | the prefix (--local)   # see below
EPREFIX       = --prefix || --local || ""              # empty only for --root/host
```

**`EPREFIX` is not cosmetic, and it is not empty under `--prefix`.** `--prefix P`
and `--local D` both set it (`Cli::base_roots`, asserted by `cli.rs`'s
`prefix_sets_eprefix`), and a build records it: a package merged into an overlay
prefix has `EPREFIX="<P>"` in its `environment.bz2`.

That matters because **a non-empty `EPREFIX` flips ebuilds into their
Gentoo-Prefix code paths**, which branch on prefix *flavor*. The distinction
they draw is `prefix-guest` (a prefix sharing the host's libc and loader) versus
a standalone/RAP prefix that owns its own. `llvm-core/clang` is the worked
example: without `prefix-guest` it builds with `-DDEFAULT_SYSROOT=${EPREFIX}`
and patches its ELF-interpreter path to `${EPREFIX}/` (`clang-22.1.8.ebuild:88`
and `:263`).

An **overlay `--prefix` is semantically a prefix-guest** — it has no libc of its
own by construction, since `R = /`. Presenting `EPREFIX` without that flavor
makes a toolchain bake standalone-prefix assumptions into a tree that will never
satisfy them: the resulting compiler looks for headers, `crt*.o` and its loader
under `P`, finds none, and cannot build a hello-world. A `--local` prefix, which
does own its libc, wants the standalone behaviour it already gets.

`USE=prefix-guest` itself is **not FreeBSD/Darwin-specific** — upstream Gentoo
Prefix force-sets it for the entire standard "rpath" profile family regardless
of host OS (`features/prefix/rpath/use.force`), so `em`'s
`native_prefix_guest` reads `true` for essentially every `--local`/`--prefix`
bootstrap, Linux included — live-confirmed on both a real Debian 12 and a real
FreeBSD 14.4 host.

Neither `--root` nor `--prefix` ever changes `config_root` — only
`--config-root` does (see above). `--prefix` additionally leaves `base_root`
alone too — only the destination moves. That is the whole difference between
"full offset" and "overlay":

- `--root foo/` alone ⇒ `R = P = foo/`, config still `/`: base *and*
  destination are `foo/`.
- `--prefix foo/` alone ⇒ `R = /`, `P = foo/`, config still `/`: base is the
  host, destination is the prefix.

## Scenarios

`R` below is `base_root` only — **not** config; config is always `/` unless
`--config-root` is given explicitly (or, for `--local`, an already-
bootstrapped prefix profile — see that row).

| invocation | R (base) | P (install) | planner installed | result |
|---|---|---|---|---|
| `em firefox` | / | / | host | normal host install |
| `em --root foo/ firefox` | foo/ | foo/ | VDB(foo/) (empty ⇒ **full closure**) | install *everything* up to firefox into `foo/` — stage / chroot-less full offset; config still read from host `/` |
| `em --root stage1/ @system` | stage1/ | stage1/ | empty | **build a stage from scratch**; config still read from host `/` |
| `em --prefix foo/ firefox` | / | foo/ | host ∪ VDB(foo/) | host is the base; install only the **new** packages into `foo/` — unprivileged `.local` **overlay** |
| `em --prefix a/ --root b/ firefox` | b/ | a/ | VDB(b/) ∪ VDB(a/) | general overlay: base `b/`, delta into `a/`, config still from host `/` (pass `--config-root` explicitly for anything else) |
| `em --local [DIR]` firefox | DIR (default `~/.gentoo`) | DIR | VDB(DIR) (empty ⇒ **full closure**) | standalone Gentoo-Prefix — like `--root`, but relocatable (`EPREFIX`), **its own BROOT** (see below), and config prefers `DIR`'s own profile once bootstrapped (else host `/`) — not the host's VDB either way |
| `{target}-emerge` (crossdev) | `/` (BROOT) | `/usr/<CHOST>/` or overridden `ROOT` | host VDB + target VDB | cross-compile; config *does* follow the target here (`PORTAGE_CONFIGROOT=${SYSROOT}`, set by the wrapper) — see [BDEPEND / crossdev](#bdepend-rdepend-and-with-bdeps) |
| `em` crossdev parity (future) | host | target | same model as portage | Stage 3 — dual-root plan entries |

`em --root foo/` and `em --prefix foo/` differ in exactly one thing: whether the
host counts as already installed. `--root` ignores it (full closure rebuilt into
`foo/`); `--prefix` keeps it (only the delta lands in `foo/`).

`--local` is a **third, distinct** mode, not "`--root` plus `EPREFIX`": its
BROOT is the prefix itself (`Cli::base_roots()`'s `--local` branch sets
`broot: Some(prefix.clone())`), unlike `--root` and `--prefix`, both of which
use the real host `/` as BROOT (`Cli::host_roots()`'s non-overlay branch
resolves `Local(prefix) => prefix, _ => "/"`). The design intent (per that
branch's own comment) is relocatability: build tools invoked during a `--local`
build are found via the shell's inherited `PATH`, not by depending on the
host's portage-tracked VDB state, so a finished `--local` prefix can be
moved/used standalone without carrying an implicit host dependency. **Known gap (found 2026-07-12)**: `preflight.rs`'s BDEPEND check has
no equivalent "found via `PATH`, no VDB entry needed" concept — it only
checks VDB entries at the BROOT it's given, which for `--local` is the
prefix's own (empty, for a from-scratch build) VDB. This makes a `--local`
`stages --stage1` bootstrap fail preflight for tools (`meson`, `xz-utils`,
`gcc`) that are genuinely present and would work fine at actual build time —
a real divergence between what preflight validates and what the "own BROOT,
PATH-based" design comment says should work. Not yet resolved; needs a
design decision (teach preflight about PATH-found tools for `--local`, or
reconsider whether `--local`'s BROOT should weave in the host after all)
before `--local` `stages --stage1` can complete end-to-end.

## Planner behaviour

1. "Installed" = `VDB(base_root) ∪ VDB(target)`, target entries shadowing base.
   - full mode (`P == R`): just `VDB(R)` (empty for a fresh stage ⇒ the plan is
     the entire `DEPEND` closure → a bootstrap).
   - overlay mode (`P ≠ R`): host (`R`) satisfies the base so **no full
     bootstrap**; the prefix (`P`) shadows and carries the delta + resume state.
2. Config (profile/make.conf/USE) is read from `config_root`.
3. The plan is the set of packages needed to satisfy the targets that the
   installed view does not already provide; each is merged into `target`.

## BDEPEND, RDEPEND, and `--with-bdeps`

PMS splits build-time needs across three dep classes, each with its own
**satisfaction root**:

| class | runs on / resolved against | typical role |
|---|---|---|
| `BDEPEND` | **BROOT** (host `/` for `--root`/`--prefix`/cross; the prefix itself for `--local`, see below) | build-host tools: `cmake`, `perl`, host `python` |
| `DEPEND` | **SYSROOT** / **ESYSROOT** | headers/libs `.pc` files for the compile |
| `RDEPEND` | **ROOT** (install destination) | binaries/libraries needed at runtime on the target |

A single atom can appear in more than one class for the same parent ebuild.
That does **not** mean “one plan entry is enough” in general — it means each
**edge** is checked against the VDB for its class's root. Whether the planner
must schedule a build depends on which edges remain unsatisfied after that
check.

### Native (`CBUILD == CHOST`, one architecture)

When host, base, and target all collapse to `/`, a package that is both
`BDEPEND` of `A` and `RDEPEND` of `C` usually needs **one** native build into
`ROOT`: the host copy satisfies the build edge, and the same binary serves
runtime once installed.

Portage's depgraph processes each class against the appropriate root; em
approximates this in `portage-atom-pubgrub` via `get_dependencies()`:

- **`--with-bdeps=n` (em default, `with_bdeps=false`)** — strip all `BDEPEND`
  edges for packages being built. Assumes BROOT already provides build tools.
  Matches `emerge -p --with-bdeps=n` on offset/stage plans.
- **`--with-bdeps` (`with_bdeps=true`)** — keep `BDEPEND`, but drop each edge
  individually when `host_installed` (host `/var/db/pkg`) already satisfies the
  version set. `RDEPEND` / `DEPEND` edges are unaffected. This is **per-edge**
  filtering, not per-package suppression: if `foo` is host-satisfied as `A`'s
  `BDEPEND` but still required as `C`'s `RDEPEND`, `foo` stays in the plan.

`preflight` mirrors the same split after the solve: `DEPEND` against
`VDB(base) ∪ VDB(target)`, `BDEPEND` against host `BROOT` (plus prefix target
for native `--prefix` within-run visibility).

**Within-run trim (2026-06):** after `install_order`, a post-solve pass drops
entries only needed for `BDEPEND` already satisfied on BROOT (host/prefix VDB)
or earlier plan entries — same growth model as [`preflight`](../../portage-cli/src/preflight.rs).
Runs only with `--with-bdeps`; see `depgraph/bdepend_trim.rs`.

### Cross (`CBUILD ≠ CHOST`) — why per-edge filtering is not enough

Crossdev does not implement a separate resolver. `crossdev -t <tuple>` lays down
`/usr/<CHOST>/` (profile, `make.conf`, overlay symlinks) and installs
`/usr/bin/<CHOST>-emerge` → `cross-emerge`. The wrapper sets:

```
CHOST     = <target triple>          # from argv[0] or env
SYSROOT   = /usr/${CHOST}            # unless overridden
PORTAGE_CONFIGROOT = ${SYSROOT}      # target profile/make.conf
CBUILD    = <host triple>            # portageq with CHOST unset
BROOT     = /                        # host (or the prefix under --local)
CROSS_CMD = emerge --root-deps=rdeps # legacy; EAPI 7+ uses BDEPEND instead
```

Target `make.conf` (written by crossdev) pins `CHOST`, `CBUILD`, and default
`ROOT=/usr/${CHOST}/`. Every `[ebuild N]` line in `riscv64-unknown-linux-gnu-emerge -p gcc`
shows `to /usr/riscv64-unknown-linux-gnu/`: the **whole closure** is a cross
build for the target. Host `cmake`/`perl` do not appear because `BDEPEND` edges
are satisfied from host `/var/db/pkg`.

Note the `gcc` in that plan output is `sys-devel/gcc` — the target's *own*
compiler, built with `CHOST == CTARGET`, ending up installed *in* the sysroot.
It is **not** the same package as `cross-<CTARGET>/gcc`, the host-side
cross-compiler crossdev's toolchain bootstrap already built and that
`riscv64-unknown-linux-gnu-gcc` on `PATH` resolves to. See
`portage-cli/src/crossdev/mod.rs`'s module doc for the full distinction —
conflating the two is exactly what caused a hard-to-read GCC self-bootstrap
failure.

Portage's depgraph (_emerge_, EAPI 7+) routes each dep string with an explicit
**`(dep_root, priority)`** pair:

| dep class | `dep_root` | priority |
|---|---|---|
| `RDEPEND` | target `ROOT` | runtime |
| `DEPEND` | `ESYSROOT` | buildtime |
| `BDEPEND` | **running root** (`/`, host) | buildtime |
| `IDEPEND` | running root | installtime + runtime |

Each scheduled task is a `Package` with a `.root` field. The **same CPV** can
appear twice: once merged to host (`/`), once to target (`ROOT`). Resolver tests
mark the target copy `{targetroot}`. Example with `--root-deps=y` (pre-BDEPEND
EAPI): `dev-libs/B` merged to **both** `/` and target; with EAPI 7+ and
`BDEPEND`, host-only build tools stay on `/` while runtime deps land on `ROOT`.

The hard case the user cares about:

```
foo  ∈  BDEPEND(A)   →  must exist on BROOT (native ${CBUILD}) to *run* during A's build
foo  ∈  RDEPEND(C)   →  must exist on ROOT   (cross  ${CHOST}) for C at runtime
```

If host already has native `foo`, only the cross `foo` is planned for `ROOT`. If
host lacks `foo` and only `BDEPEND` demands it, portage schedules a **native**
merge to `/`. If `RDEPEND` also needs it, portage schedules a **second** merge
to `ROOT` with target `CHOST` — two tasks, two images, two VDB locations.

**`em` today:** one `PortagePackage` node per `(CPN, slot)` and a single merge
into `target`. `host_installed` + `bdepend_filtered()` correctly implements the
host-satisfaction **check** for native builds, but there is no
`(cpv, slot, root)` plan entry, no per-task `CHOST`, and no “merge to BROOT”
path. Stage 3 must add portage's dual-root scheduling, not extend the current
filter.

### `ROOT=/tmp/place` + `{target}-emerge foo`

Crossdev's default target root is `/usr/<CHOST>/`, but nothing requires it.
`{target}-emerge` is `cross-emerge`: it fixes `SYSROOT` and `PORTAGE_CONFIGROOT`
to `/usr/<CHOST>` (unless you override those env vars), then execs plain `emerge`.

A custom merge destination is just a `ROOT` override on top:

```bash
# Profile/config still from /usr/riscv64-unknown-linux-gnu/
# Binaries land under /tmp/place instead of /usr/riscv64-unknown-linux-gnu/
ROOT=/tmp/place riscv64-unknown-linux-gnu-emerge -p foo
```

What each variable does in that invocation:

| variable | typical value | role |
|---|---|---|
| `PORTAGE_CONFIGROOT` | `/usr/riscv64-unknown-linux-gnu` | profile, `make.conf`, `package.*` — set by wrapper |
| `SYSROOT` / `ESYSROOT` | `/usr/riscv64-unknown-linux-gnu` | where `DEPEND` headers/libs are found |
| `ROOT` / `EROOT` | `/tmp/place` (**override**) | where `foo` and its **target** closure install |
| `BROOT` | `/` | where `BDEPEND` tools execute; unchanged |
| `CBUILD` | host triple | compiler that builds build-tools |
| `CHOST` | `riscv64-unknown-linux-gnu` | compiler that builds target packages |

Effects:

1. **Resolver** still distinguishes `_running_eroot` (host `/`) from
   `_target_eroot` (`/tmp/place`). `BDEPEND` checks host VDB; `RDEPEND` checks
   `/tmp/place/var/db/pkg`.
2. **Pretend output** shows `to /tmp/place/` on every target-scheduled package.
3. **Host merges** (when portage schedules a native copy for an unsatisfied
   `BDEPEND`) still go to `/` — they do not follow the `ROOT` override.
4. **Builder** on the target task sets `ROOT=/tmp/place`, `SYSROOT` to the
   cross sysroot, `BROOT=/`, `CHOST=riscv64-…`, `CBUILD=aarch64-…` (on this
   host).

So `ROOT=/tmp/place` is an alternate **install tree** for the cross target,
not a different config root. Common uses: disposable chroots, CI images, keeping
`/usr/<CHOST>` clean while iterating. `em` has no equivalent entry point yet;
mapping it would be `em -p --root /tmp/place --config-root /usr/<CHOST> …` once
the planner honours per-class roots and dual plan entries.

### `--with-bdeps` under cross

Same semantics as native, but the roots differ:

| flag | native offset | cross `{target}-emerge` |
|---|---|---|
| default (`with_bdeps=false`) | assume host tools; plan only target closure | same — host satisfies `BDEPEND`, plan goes to `ROOT` |
| `--with-bdeps` | include unsatisfied `BDEPEND` into plan (still merged to target on native) | same closure as default on `-p` when BROOT is full; unsatisfied `BDEPEND` schedule to **BROOT** `/` via Host-root nodes, not onto `ROOT` |

Cross `-p` with `--with-bdeps=y` does not expand host-satisfied build tools onto
the target merge list (verified: `riscv64-emerge -pv --with-bdeps=y gcc` still
lists 18 packages). `em` matches: target-root builds always use runtime deps;
`with_bdeps` only affects native offset filtering and future unsatisfied-BROOT
host scheduling.

## Builder behaviour

Per phase (`run_phase`) we set:

```
PORTAGE_CONFIGROOT = config_root                 # host unless --config-root (or crossdev's wrapper)
ROOT = EROOT       = target                       # install destination
SYSROOT = ESYSROOT = base                          # build-against system; SYSROOT
                                                   #   trailing slash stripped, "/"→""
BROOT              = /
EPREFIX            = ""
```

`SYSROOT`/`ESYSROOT` is the **base**, not the target: the build resolves
`DEPEND` against the base system — the real host `/` for both `--prefix` and
a same-arch `--root DIR` (matching `satisfaction_root(DepClass::Depend)` and
real `ROOT=` emerge's own `ESYSROOT`), with the target layered on top for
overlays. `SYSROOT` collapses to `ROOT` only for a topology with its own build
closure: `--local`'s own BROOT, or a cross-arch `--target` sysroot building
against itself. `Roots::build_sysroot()` returns `None` there, and `run_phase`
defaults `SYSROOT = ROOT` when no separate base is given.

The eclasses (already sourced from the host repo) translate these into
build-system specifics — chiefly `multilib_toolchain_setup` pointing
`PKG_CONFIG_{LIBDIR,PATH,SYSTEM_*}` at `ESYSROOT`, plus `econf --with-sysroot`,
`meson`/`cmake` cross-files, etc. **We never enumerate build systems.**

### Overlay support (`target ≠ base`, e.g. `--prefix`)

With `SYSROOT = base`, a package merged into the **target** is not visible to
later builds in the same run (the toolchain/eclasses point at the base). Making
a chain resolve earlier members needs the target layered on top of the base
sysroot. Two ways were considered; the choice is **config-driven now**, with a
zero-config option deferred.

**Rejected — env injection in our code.** Appending the target's `pkgconfig` to
`PKG_CONFIG_PATH` and its include/lib to `CPPFLAGS`/`LDFLAGS` covers pkg-config +
autotools/make (universal conventions), but some build systems locate deps
through their **own** search root — cmake `find_package` config-mode
(`CMAKE_PREFIX_PATH`/`CMAKE_FIND_ROOT_PATH`), some meson `dependency()`
providers. Covering those means our code enumerating per-build-system knobs,
which this design avoids (portage feeds them from eclasses keyed off a single
`ESYSROOT`). So we do **not** do this in code.

**Chosen — config-driven via `bashrc` (today).** We source portage's `bashrc`
hooks per phase (see "bashrc support" below) with the full env available
(`ROOT`, `SYSROOT`, `get_libdir`, …). The **user** wires the overlay there for
whatever build systems they use — `em` ships no build-system knowledge, and the
user completes it without touching our code. Verified: a `liba`→`usea` chain
into one `--prefix` resolves (pkg-config + compile/link) with this `bashrc`:

```bash
# /etc/portage/bashrc — layer an em --prefix target over the base sysroot
if [[ -n ${ROOT} && ${ROOT%/} != "" && ${ROOT%/} != "${SYSROOT%/}" ]]; then
    _ov=${ROOT%/}; _ld=$(get_libdir)
    export PKG_CONFIG_PATH="${_ov}/usr/${_ld}/pkgconfig:${_ov}/usr/share/pkgconfig${PKG_CONFIG_PATH:+:${PKG_CONFIG_PATH}}"
    export CPPFLAGS="-I${_ov}/usr/include ${CPPFLAGS}"
    export LDFLAGS="-L${_ov}/usr/${_ld} -Wl,-rpath,${_ov}/usr/${_ld} ${LDFLAGS}"
    # add e.g. CMAKE_PREFIX_PATH=${_ov}/usr for cmake find_package, etc.
fi
```

**Deferred — merged sysroot (zero-config).** Merge the target over the base into
one filesystem view (`fuse-overlayfs` / `overlayfs` under a user namespace) and
point **one `ESYSROOT`** at it; the existing eclass machinery then covers every
build system with **zero enumeration and no user bashrc**. This belongs with
**M3 (namespaces/sandbox)** and gives crossdev a real sysroot for free. (Shipping
our own two-root-aware **eclass overlays** is a complementary lever, but the
merged sysroot keeps the "one `ESYSROOT`, eclasses do the rest" invariant.)

**Status:** overlay works today via a user `bashrc`; out of the box (no bashrc)
`SYSROOT = base`, so single packages whose deps are all in the base build into a
target correctly, but a chain needs the bashrc recipe (or the future merged
sysroot). Full closure (`--root`, target == base) is unaffected.

### bashrc support

`em` sources portage's `bashrc` hooks per phase (not PMS; matches
`__source_all_bashrcs`): each profile's `profile.bashrc` in stack order, then
the user's `${PORTAGE_CONFIGROOT}/etc/portage/bashrc`, after the environment is
set up and before the phase function. They see the full env (`ROOT`, `EROOT`,
`SYSROOT`, `ESYSROOT`, `BROOT`, `PORTAGE_CONFIGROOT`, `get_libdir`, the flag
vars). The per-package `/etc/portage/env/` mapping is not yet sourced. This is
the general user hook for env tweaks; the overlay recipe above is one use.

### Known hard part

Native (`CHOST == CBUILD`) discovery of **non-`.pc` headers/libs** under a
`target ≠ /` is the genuine soft spot: a plain host `gcc` won't look in
`target/usr/include` without `--sysroot`/`-I` injection. It is mostly papered
over by pkg-config (`ESYSROOT`) + `econf --with-sysroot`, and the `bashrc`
recipe handles it for `--prefix`. The merged sysroot resolves it generally;
true isolation across arches is crossdev's job (CHOST-prefixed toolchain with a
baked sysroot).

## Orthogonal axis: package source (build vs binpkg)

How a package's *image* is produced — built from source, or unpacked from a
**binary package** (local or third-party) — is independent of the root model.
Once an image exists, the **merge** is identical: walk image → `target`,
register VDB, CONFIG_PROTECT, ecompress/estrip already applied at build/package
time. So binpkg support (`-k/-K`, gpkg/xpak, third-party `BINHOST`) is a
separate axis that reuses the same merge path; it changes only the *producer*,
never the root handling.

## Implementation staging

- **Stage 1 — the three-root split (planner side) [done]:** `Roots`
  (`config`/`base`/`target`); planner config from `config_root`, installed =
  `VDB(R) ∪ VDB(P)`, merge into `target`; applets (`env`, `world`, `query`, …)
  read the right root; `SYSROOT` trailing slash.
- **Stage 1b — the builder side [done]:** thread the roots through
  `build_and_merge`/`run`/`run_inner` to `EbuildShell::set_build_roots`;
  `run_phase` sets `PORTAGE_CONFIGROOT = config`, `ROOT/EROOT = target`,
  `SYSROOT/ESYSROOT = base` (collapsing to `ROOT` when base == target), `BROOT
  = /`; `apply_profile_env` reads config from `config_root`. Makes host, full
  offset / stage, and single-package `--prefix` (deps in base) builds correct.
- **Stage 2 — overlay (`--prefix`, target ≠ base) [config-driven, done]:** `em`
  sources portage `bashrc` hooks per phase exposing the roots + `get_libdir`;
  the user wires overlay search paths there (recipe in "Overlay support"). No
  build-system knowledge in code. The zero-config **merged sysroot** (single
  `ESYSROOT` over a `fuse-overlayfs`/`overlayfs` union) is deferred to M3.
- **Stage 3a — BDEPEND native parity [partial]:** `--with-bdeps` flag,
  `host_installed` filtering, default-off matching `emerge --with-bdeps=n` on
  offset builds. Per-edge filter only; see [BDEPEND / crossdev](#bdepend-rdepend-and-with-bdeps).
- **Stage 3b — dual-root planner [partial, 2026-06]:** `MergeRoot` on
  `PortagePackage`; auto-activation for crossdev, `config_root ≠ merge_root`,
  or `merge_root ≠ /` (native stage/offset). Dep classes routed per PMS table
  8.2; host-satisfied `BDEPEND`/`IDEPEND` dropped on BROOT. Cross `gcc -p`
  matches emerge (18 packages) with or without `--with-bdeps`. Post-solve
  within-run `BDEPEND` trim for `--with-bdeps` prefix chains (stage 3e).
  Profile `ACCEPT_LICENSE` `@GROUP` tokens expanded via `profiles/license_groups`
  (`portage-repo::AcceptLicense`).
- **Orthogonal — binpkg:** producer-only; plugs into the existing merge.

## Sequencing: `--root` → crossdev → `--local`/`--prefix`

Stages 1–2 above are the shared three-root plumbing (done). **Stage 3 is the
multi-root *dep routing*, and it is staged by how much machinery each target
needs — each tier reuses what the previous built, so they are not independent
features and must land in this order.**

### Tier 1 — `--root` for a Gentoo host (native offset, active)

`em -p --root <empty> --config-root / <atoms>`. The simplest multi-root case
and the foundation. Same arch throughout (`CBUILD == CHOST`), `BROOT = /`,
`SYSROOT` collapses to `ROOT`. This is where per-class edge routing must become
correct: each of `BDEPEND`/`DEPEND`/`RDEPEND`/`IDEPEND` checked against its own
satisfaction root, including the rule the current open gap exposes — an
**unsatisfied `BDEPEND`/`DEPEND` edge on `BROOT` must schedule a native merge
to `/`** (a `MergeRoot::Host` entry), not be silently broot-filtered. The
offset `@system` gap (em 177 vs emerge 180; the `nghttp2/nghttp3/ngtcp2`
host-side build copies) is exactly this. Once the solver emits
`(cpn, slot, MergeRoot::Host)` entries for native offsets, Tier 1 reaches
parity.

### Tier 2 — crossdev (`{target}-emerge`, `CBUILD ≠ CHOST`)

Crossdev is Tier 1's dual-root model with one addition: the `Host` and
`Target` entries are built by *different compilers* (native `gcc` vs the
`<CHOST>-gcc` cross-toolchain). The **routing is identical** to what Tier 1
made correct — `crossdev` already auto-activates dual-root scheduling and
matches `riscv64-emerge -p gcc` (18 packages). Tier 1's Host-merge scheduling
is the missing piece for the cross equivalent of the offset gap; once it
exists, cross is "turn on the foreign `CHOST`" over the same
`(cpn, slot, root)` plan entries. **Depends on Tier 1**: cross cannot schedule
host-side build deps correctly before native offset can.

### Tier 3 — `--local` / `--prefix` (non-Gentoo host; BROOT becomes the prefix)

The most divergent, and last by design. On a Debian/Arch/Fedora host there is
no portage VDB and no recorded toolchain, so **`BROOT ≠ /`**: it is a **stage1
build-tool subset installed *into* the prefix** (compiler + core build tools,
sharing the host **libc** — the Gentoo-Prefix "coexist with a foreign
userland" property). It reuses Tier 1/2's dep-class routing and
`(cpn, slot, root)` scheduling verbatim, but adds a *mutable BROOT* and the
`--setup` ceremony that bootstraps the subset. Deferred because the "host is
Gentoo and provides `BROOT=/`" assumption that makes Tiers 1–2 tractable does
not hold here, so it is the most work and the least general. Today `--setup`
*borrows* host tools via symlinks (`portage-cli/src/setup.rs`) rather than
building a real subset; making the prefix self-hosting is this tier.
