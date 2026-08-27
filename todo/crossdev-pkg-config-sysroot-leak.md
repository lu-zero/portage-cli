# `PKG_CONFIG_SYSROOT_DIR` leaked into `econf_build`'s native sub-configure

Status: 🟡 fixed for bare `--target`, live-verified; `--prefix`/`--local`
under `--target` not yet confirmed clean (blocked on two unrelated bugs,
see [[crossdev-prefix-gcc-header-dir]] and
[[crossdev-local-perl-module-eprefix]]). Found 2026-08-26 hand-driving
`em stages --stage1`/plain merges in a fresh crossdev-stages sandbox — see
[[i586-full-run-findings]]'s `dev-lang/python` section for the original,
less-precise diagnosis this supersedes.

## The bug

`dev-lang/python` (and, in principle, any package whose build system calls
a resolved pkg-config binary directly rather than reading a build-specific
variable) failed under **any** `--target` cross build:

```
aarch64-unknown-linux-gnu-gcc -I/usr/i586-pc-linux-gnu/usr/include ... -c .../Modules/_decimal/_decimal.c
.../_decimal.c:4915:6: error: #error "No valid combination of CONFIG_64, CONFIG_32 and _PyHASH_BITS"
...
checking for --with-build-python... configure: error: invalid or missing build python binary
```

Reproduced two ways, confirming it's general and not `stages`-specific:
`em --target i586-pc-linux-gnu stages --stage1` (packages.build set) and a
plain `em --root R --target i586-pc-linux-gnu --nodeps dev-lang/python`.

## Root cause (confirmed by direct reproduction, not just theory)

`build_cbuild_python` (part of real, unmodified `python-utils-r1.eclass`)
builds a native "build python" via `econf_build`/`tc-env_build` (real,
unmodified `toolchain-funcs.eclass`). `tc-env_build` clears `SYSROOT=`
and sets `ESYSROOT=${BROOT}` for that sub-invocation, and resolves
`PKG_CONFIG=$(tc-getBUILD_PKG_CONFIG)` — which `em`'s own `shell.rs`
(the `BUILD_*` tool-export block) correctly resolves to the **plain**
native `aarch64-unknown-linux-gnu-pkg-config` (confirmed at
`build.log:736`, not `em`'s own `<CTARGET>-pkg-config` wrapper).

The problem: `em`'s crossdev sysroot make.conf statically exported
`PKG_CONFIG_SYSROOT_DIR`/`PKG_CONFIG_LIBDIR` for the *whole* phase. Real
pkgconf honours those two env vars unconditionally regardless of which
binary invokes it — confirmed directly:

```
$ PKG_CONFIG_SYSROOT_DIR=/usr/i586-pc-linux-gnu PKG_CONFIG_LIBDIR=/usr/i586-pc-linux-gnu/usr/lib/pkgconfig:... \
  aarch64-unknown-linux-gnu-pkg-config --cflags libffi
-I/usr/i586-pc-linux-gnu/usr/lib/libffi/include
```

Same correct native binary, wrong (target) answer, purely from the
ambient env vars. Real crossdev never has this problem because it never
sets these two vars as static exports anywhere — only `em select
pkgconf`'s `<CTARGET>-pkg-config` wrapper computes them, dynamically,
per-invocation, from `ESYSROOT`/`SYSROOT`/`ROOT`.

Precisely, re-read off the generated wrapper in the sandbox 2026-08-27:
`PKG_CONFIG_LIBDIR` is cleared (`export PKG_CONFIG_LIBDIR=`) at the top
and re-exported unconditionally at the bottom, so the wrapper always owns
it; `PKG_CONFIG_SYSROOT_DIR` uses `: "${PKG_CONFIG_SYSROOT_DIR=${SYSROOT}}"`
(assign-if-unset), so an already-exported ambient value wins over the
phase's real `SYSROOT`. Removing the static export therefore also makes
the wrapper *more* correct, not just the unwrapped native binary: with
nothing ambient, it now derives from the phase's own `SYSROOT`. Routing
`BUILD_PKG_CONFIG` through the wrapper would not on its own have fixed
the native sub-configure, since the ambient `PKG_CONFIG_SYSROOT_DIR`
would still have taken precedence there.

## The fix (landed, uncommitted as of this writing — see below)

`make_conf_body()` (`portage-cli/src/crossdev/mod.rs`) no longer emits
static `PKG_CONFIG_SYSROOT_DIR`/`PKG_CONFIG_LIBDIR`. Target-code packages
still get correctly-scoped `PKG_CONFIG` via the existing `<CTARGET>-pkg-config`
wrapper (`em select pkgconf`), which self-derives per invocation and is
unaffected. `BUILD_PKG_CONFIG_LIBDIR` (the existing meson-specific
workaround) is untouched.

Live-verified: bare `--target` `dev-lang/python` cross build completes
past the previously-failing point (full `crossdev --setup`, all 6 steps,
succeeded end to end).

## Accepted tradeoff

Reopens a narrower, previously-fixed regression: `make_conf_body_scopes_
pkg_config_to_the_sysroot` (now `make_conf_body_no_longer_sets_static_
pkg_config_sysroot_scoping`, its inverse) was originally a regression test
for iproute2's `configure` calling **bare, unwrapped** `pkg-config`
(bypassing `$PKG_CONFIG` entirely) and linking the host's
`net-libs/libtirpc.pc`. That specific failure mode can recur until
`BUILD_PKG_CONFIG` (and potentially plain unprefixed `pkg-config` on
`PATH`) also gets routed through a wrapper that force-scopes regardless
of ambient env — a real, separate follow-up, not done here. Deliberately
traded: the python-class bug blocks *any* cross build of a very common
bootstrap package; the iproute2-class bug only bites packages that
bypass `$PKG_CONFIG` directly.

## Verification status by topology

| Topology | Result |
|---|---|
| bare `--target` | ✅ live-verified: full `crossdev --setup` (6 steps) succeeds, `dev-lang/python` builds |
| `--prefix` + `--target` | 🟡 partial: 5/6 toolchain steps (incl. `sys-libs/glibc`, itself a target-code package reading the same make.conf) succeeded; blocked from full completion by an unrelated bug, [[crossdev-prefix-gcc-header-dir]]. Ordinary `--prefix` package building (no `--target`) with real pkg-config confirmed clean (`dev-libs/libffi`) |
| `--local` + `--target` | 🔴 blocked before reaching any pkg-config-relevant package by an unrelated bug, [[crossdev-local-perl-module-eprefix]] |

## How to attack (remaining)

1. Resolve the two blocking bugs above enough to get a target-code
   package build under `--prefix`/`--local` + `--target`, to directly
   confirm this fix (not just the surrounding mechanism) is clean there
   too.
2. Commit the pending `crossdev/mod.rs` change (currently uncommitted in
   the working tree) once that confirmation lands, or sooner if the
   bare-`--target` evidence is judged sufficient on its own.
3. Separately: give `BUILD_PKG_CONFIG` its own force-scoping wrapper to
   close the reopened iproute2-class gap without reintroducing this bug.
