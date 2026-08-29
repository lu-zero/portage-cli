# `cross-*/gcc` stage2 can't find target libc headers under `--prefix`

Status: 🔴 reproduced again 2026-08-29, riscv64 this time, at a
different step (gcc-**stage1**, not stage2) — not i586-specific after
all. Found 2026-08-26 testing `--prefix`/`--local` impact of
[[crossdev-pkg-config-sysroot-leak]] in a real crossdev-stages sandbox —
unrelated to that fix (confirmed: happens via a completely different
mechanism, gcc's own sysroot/header-dir configuration, not pkg-config).

**2026-08-29 riscv64 recurrence**: `em --prefix P --target
riscv64-unknown-linux-gnu crossdev --setup` failed at `[3/6]
gcc-stage1` building `libgcc`:

```
libgcc/../gcc/tsystem.h:95:10: fatal error: stdio.h: No such file or directory
```

— same symptom family (a header search path one component short
relative to the prefix), but at **gcc-stage1** (before glibc exists —
libgcc here can't be looking for target libc headers the way the
original i586 gcc-stage2 report described; needs its own trace, don't
assume it's the identical bug). Found live-verifying
[[crossdev-root-prefix-target-toolchain-anchor]]'s `Cli::roots()` fix;
unrelated to that fix (x86_64 on the same host, same day, completed all
6 steps cleanly with the identical `--prefix`-only invocation shape).

**2026-08-29 update**: re-ran the exact repro shape (`em --prefix P
--target T crossdev --setup`, no `--root`) with `x86_64-unknown-linux-gnu`
in a fresh crossdev-stages sandbox — all 6 steps, including gcc-stage2,
completed cleanly, `stdio.h` found without issue. Did not re-test with
`i586-pc-linux-gnu` specifically, so this isn't a confirmed fix — could
be arch-specific (i586 multilib?) or could have been resolved
incidentally by unrelated work since 2026-08-26. Also found a
*different*, related, still-open problem the same day:
[[crossdev-root-prefix-target-toolchain-anchor]] (`--root` under
`--prefix`+`--target` needs different semantics depending on whether
it's `--setup` building the sysroot itself or an ordinary operation
against an existing one — two fix attempts both reverted, not the same
failure mode as this one, but worth knowing about if re-investigating.

## The bug

`em --prefix /prefixtest --target i586-pc-linux-gnu crossdev --setup`
(after the correct prerequisite `em --prefix /prefixtest setup` — see
[[crossdev-local-perl-module-eprefix]] for the sibling bug this
prerequisite step also matters for) gets through 5 of 6 toolchain steps
— baselayout, binutils, gcc-stage1, kernel-headers, **and glibc itself**
all build successfully — then fails in gcc-stage2's `libgcc`:

```
.../gcc-16.2.0/work/build/./gcc/xgcc -B... -isystem /prefixtest/usr/i586-pc-linux-gnu/include \
  -isystem /prefixtest/usr/i586-pc-linux-gnu/sys-include ...
.../libgcc/../gcc/tsystem.h:95:10: fatal error: stdio.h: No such file or directory
```

`stdio.h` is genuinely present at
`/prefixtest/usr/i586-pc-linux-gnu/usr/include/stdio.h` (glibc, built
successfully one step earlier) — the compiler is looking one path
component short (`.../i586-pc-linux-gnu/include` instead of
`.../i586-pc-linux-gnu/usr/include`).

## What's confirmed

- **Not a `--prefix` problem in general**: ordinary `--prefix` package
  building (no `--target`) works cleanly — `dev-libs/libffi` built,
  installed, and even generated a correctly-scoped `.pc` file with no
  issues.
- **Not the pkg-config bug**: this is gcc's own `-isystem`/sysroot
  configuration (`--with-sysroot`/native-system-header-dir), an entirely
  separate mechanism from `PKG_CONFIG_SYSROOT_DIR`/`PKG_CONFIG_LIBDIR`.
- Bare `--target` (no `--prefix`) builds the identical
  `cross-i586-pc-linux-gnu/gcc-16.2.0` successfully — full 6-step
  `crossdev --setup` completed end to end. So this is specific to the
  interaction between `--prefix`'s `EPREFIX` and the host-side
  `cross-*/gcc` (`host_codegen`) build path.

## Where to look

Real `toolchain.eclass` (`/var/git/gentoo/eclass/toolchain.eclass` on
this host) computes, for the cross-compiler build:
`PREFIX=${TOOLCHAIN_PREFIX:-${EPREFIX}/usr}`, then
`--with-sysroot="${PREFIX}"/${CTARGET#accel-}` — i.e.
`${EPREFIX}/usr/${CTARGET}`. For bare (`EPREFIX` empty) that's
`/usr/i586-pc-linux-gnu`, which works. For `--prefix /prefixtest` it
should become `/prefixtest/usr/i586-pc-linux-gnu` — same shape, just
prefixed — yet the *runtime* `-isystem` path shows one component
missing. Whatever `em`'s `shell.rs` computes for `EPREFIX` (or a related
var this eclass reads) during this specific host-side `cross-*/gcc`
build under `--prefix` is subtly wrong in a way bare `--target` never
exercises.

`shell.rs`'s own doc comment already flags this exact function as
fragile: "this function derives `ROOT`, `EPREFIX`, `ED`, `EROOT`,
`SYSROOT`, `ESYSROOT` through a chain of local variables computed in
sequence, all keyed off the same `build_class` signal" (the
`host_codegen`/`cross_triple` branches specifically) — a good candidate
first place to instrument/trace.

## How to attack

1. Reproduce minimally: `em --prefix DIR setup` then
   `em --prefix DIR --target i586-pc-linux-gnu crossdev --setup`
   (real crossdev-stages sandbox, i586 target — already set up once,
   marker at `/prefixtest` in the `em-i586-check` sandbox as of this
   writing).
2. Instrument or trace `shell.rs`'s EPREFIX/SYSROOT/ESYSROOT derivation
   specifically for the `cross-i586-pc-linux-gnu/gcc-16.2.0` phase env
   under `--prefix`, comparing against the bare-`--target` case that
   works, to find exactly which variable diverges.
3. Live-verify against a real `--prefix` + `--target` full `crossdev
   --setup` once fixed.
