# Cross-sysroot spec: what it says and how it becomes Portage config

A cross target's description is data: `SysrootSpec` (`portage-cli/src/crossdev/spec.rs`), serialised as
TOML. A tuple has a built-in spec (`em crossdev --target T --print-spec`); `--target` also takes a spec file.
This page states how a spec turns into files Portage and `em` read, and what is still missing.

> **Slop warning.** Verify against the code before relying on this page; update it when it drifts.

## The two conversions

A spec cannot become one kind of file. Portage reads some settings from a profile and others only from the
user configuration directory, so a spec converts in two places:

| Spec says | Lands in | Why there |
|---|---|---|
| `profile` (one or several) | the derived profile's `parent`, one `gentoo:<profile>` line each | a profile is how Gentoo describes a system |
| a version or slot on a toolchain package (a pin) | the derived profile's `package.mask` + `package.unmask` | both are profile files |
| which toolchain builds the system (`system.built-by`) | that toolchain's `env` in the derived profile's `make.defaults` | any variable may be set there |
| packages another toolchain builds (`system.exceptions`) | `<sysroot>/etc/portage/package.env/cross-sysroot` + `env/<toolchain>-toolchain.conf` | Portage reads `package.env` only from user config |
| `package-use` | the derived profile's `package.use` | a profile file |
| `cflags` | `<sysroot>/etc/portage/make.conf` today; the profile's `make.defaults` is the natural place | |
| `[make-conf]` | appended to `<sysroot>/etc/portage/make.conf` | user-level settings, above the profile |
| `extra` | the cross alias, a final `--setup` step each, and pins as for a toolchain package | built on the host like crossdev's `--ex-pkg` |

Built-in tuples get neither: their `make.profile` links the `::gentoo` profile directly, as before specs.

## Conversion 1: the derived profile (implemented)

`--init-target` with a spec file writes an overlay inside the sysroot and links `make.profile` to its profile:

```
<sysroot>/var/db/repos/cross-sysroot/
  metadata/layout.conf          masters = gentoo, profile-formats = portage-2
  profiles/repo_name            cross-sysroot
  profiles/sysroot/eapi         8
  profiles/sysroot/parent       gentoo:<profile>, one line per profile
  profiles/sysroot/make.defaults     the system toolchain's env
  profiles/sysroot/package.use       the spec's package-use
  profiles/sysroot/package.mask      one bare package per pin
  profiles/sysroot/package.unmask    the pins as written
  profiles/sysroot/cross-sysroot.toml   the spec itself
<sysroot>/etc/portage/repos.conf/cross-sysroot.conf
<sysroot>/etc/portage/make.profile -> .../profiles/sysroot
```

- **`gentoo:` parents** are Portage's `portage-2` profile format; PMS only defines relative `parent` paths.
  `em` reads them through `ProfileStack::build_in`, resolving the name through the config root's `repos.conf`.
- **The `eapi` file is required.** Without it Portage reads the profile as EAPI 0, which has no slot
  dependencies, and drops a pin such as `sys-devel/gcc:15` from `package.unmask`.
- **`package.unmask` in a profile** is read by Portage only from profiles allowing `portage-1` directories
  (outside a repository, the user profile, or a `portage-1`/`portage-1-compat`/`portage-2` repository); `em`
  applies the same gate.
- **The saved spec** is what a later command given just the tuple uses: it follows `make.profile` and loads
  `cross-sysroot.toml`.
- **The cross packages** (`cross-<tuple>/*`, built on the host) cannot see the sysroot profile. Their pins go to
  the host's `package.mask/<category>` and `package.unmask/<category>`, and the keyword line stops below the
  release branch of the newest keyworded version a pin allows, so a live ebuild never wins.

## Conversion 2: per-package toolchains (implemented)

- **Each toolchain carries its variables** in `[toolchain.<name>.env]`. A toolchain written without one gets
  the tuple's built-in tools: gcc's are the tuple-prefixed binutils and gcc (`CC=<tuple>-gcc`, `LD=<tuple>-ld`,
  …, `LLVM=""`), clang's the per-target wrappers and LLVM's tools (`CC=<tuple>-clang`, `LD=ld.lld`,
  `AR=llvm-ar`, …, `LLVM=1`). Both set every tool `features/llvm` sets (CPP, OBJDUMP, STRINGS and ADDR2LINE
  included): an unset one inherits the profile's unprefixed LLVM tool, and glibc's header check died running
  the host `clang-cpp`.
- **The system toolchain's** variables go to the derived profile's `make.defaults`, after its parents, so they
  override an unprefixed `CC=clang` from `features/llvm`; `make.conf` can still override them.
- **Each other toolchain** named in `exceptions` gets `<sysroot>/etc/portage/env/<toolchain>-toolchain.conf`,
  and each exception a line in `<sysroot>/etc/portage/package.env/cross-sysroot`, e.g.
  `sys-libs/glibc gcc-toolchain.conf`. Portage has no profile form of `package.env` (it reads it from
  `abs_user_config` only).

## Mixed bootstrap (implemented)

A spec defining both toolchains loads when gcc builds the libc; it stays a GCC-model target (`cross-<tuple>`
category, so glibc is allowed). `toolchain_plan` builds the gcc toolchain and the libc as usual, then the clang
wrappers and compiler-rt, libunwind (`static-libs`), libcxxabi and libcxx, each bound to the LLVM slot, and last
the per-slot clang config packages (`clang-{rtlib,unwindlib,stdlib}-config`, `clang-linker-config`) into the
sysroot: the target's own LLVM runtimes BDEPEND on them but read `${ESYSROOT}/etc/clang/<slot>/gentoo-*.cfg`.

The per-target clang config (`/etc/clang/cross/<tuple>.cfg`) is written twice by the post-step hook: after
gcc-stage2 it points at the cross gcc (`--gcc-install-dir`), after libcxx at LLVM's runtimes
(`--rtlib=compiler-rt --unwindlib=libunwind --stdlib=libc++ -fuse-ld=lld`). An LLVM-only target writes the second
form at `--init-target`, as before. On the host, only the clang roles get the LLVM env (`llvm.conf`, now the
spec's clang `env`) and the slot's keyword line.

## The LLVM slot

The wrappers run the host's `clang-<slot>`, and the target's LLVM runtimes build with `<tuple>-clang-<their
major>`, so host clang, cross toolchain and the target's LLVM must share one slot. Real crossdev picks each package's
newest version independently and can mix slots; `em` binds one:

- the slot is the spec's clang compiler slot, else the newest `llvm-core/clang` with KEYWORDS (live and snapshot
  ebuilds have none), whatever the host has installed; the bootstrap builds that host clang if it is missing;
- the derived profile masks `llvm`, `clang`, `lld` and the LLVM runtimes above the slot, so the target resolves to
  it.

## Stage1

Stage1 builds with `USE="-* build"`, which resets the derived profile's `package.use` (Portage's USE order puts
the environment above `package.use`). A spec's `package-use` states what its toolchain needs, so `em stages
--stage1` passes it as the staged run's own `package.use`, above the reset.

## Clang-main riscv64 glibc, run 2026-10-21/22

Sandbox `em-clang-main` (arm64 host), from a spec file with both toolchains, `profile` = the riscv64 lp64d profile
plus `features/llvm`, `exceptions.gcc` = glibc and gcc, and `static-libs` on the runtimes:

- `em --target clang-main.toml crossdev --setup`: 12 steps, the gcc toolchain and glibc, then clang 23 wrappers,
  the LLVM runtimes and the sysroot's clang config. `<tuple>-clang++` links libc++, libc++abi and libunwind.
- `em --target <tuple> --root /root/stage stages --stage1`: 106 packages, no `sys-devel/gcc` or binutils, LLVM,
  clang and lld 23 for riscv64; the recorded `CC` is `<tuple>-clang` for bash, coreutils, python and llvm and
  `<tuple>-gcc` for glibc. The hand-made experiment reached the same 106 packages.
- Bugs found and fixed on the way: post-install strip ran with em's own PATH (`llvm-strip` not found);
  the missing CPP and friends above; stage1 dropping `package-use`; the target's LLVM resolving above the
  cross slot; the sysroot lacking the clang config packages.

## `-p` before `--init-target`

`-p` writes nothing, so a preview of a target never initialised resolves with what `--init-target` would write,
from memory. Both halves are values that the files are rendered from, so the two cannot drift: the derived profile
(`DerivedProfile`: parents, a `ProfileLayer` of masks, unmasks and `package.use`, `make.defaults`), handed to the
sysroot steps through `SysrootOverride`; and the host's cross-category keywords, masks and unmasks
(`host_cross_config`), handed to the `cross-*` steps as a `PendingConfig`.

## Resuming `--setup`

`--setup` rebuilds every step, like real crossdev, and records each completed one in
`<sysroot>/var/lib/em/crossdev-setup.json` (the spec, the step's atoms and USE, the installed packages it produced).
`--setup -r` skips steps from the start while the record holds: same spec, same step, those packages still
installed. The first step that does not hold runs again, and every step after it, so a changed spec rebuilds from
the start. It resumes, it does not upgrade: a newer version in the tree does not invalidate a recorded step.
The record is keyed on the spec without its `extra` list; extra steps are checked one by one, so a new extra
leaves the toolchain done.

Each step belongs to one of crossdev's stages (`stages::Level`). `--stage N` runs the plan up to stage `N`,
and `--ex-only` runs only the extras once the record shows the rest complete; both are per-run choices, not
spec keys.

## Still open

- **Cross and target gcc can differ** within a pin: the host side accepts unkeyworded snapshots (`**`), the
  sysroot only the target's keywords.
- **`cflags`** still goes to the sysroot's `make.conf`; the derived profile's `make.defaults` is the natural place.

The bootstrap order stays derived in code (`crossdev/stages.rs`) from who builds what, not written in the spec.
