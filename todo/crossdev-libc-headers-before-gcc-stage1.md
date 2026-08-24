# `toolchain_plan`'s libc-headers-before-gcc-stage1 order breaks x86 glibc configure

Status: 🔴 not started. Found 2026-08-24 during the crossdev-stages
replacement retest plan (Phase 1 pilot, i586/`pentium-mmx` board).

## The bug

`em --target i586-pc-linux-gnu crossdev --setup` reliably fails at step
4/7 ("libc headers"):

```
CC=aarch64-unknown-linux-gnu-gcc ... configure --host=i586-pc-linux-gnu ...
...
checking if -Os inlines trunc... failed to check if -Os inlines trunc.
die: failed to configure glibc
```

`portage-cli/src/crossdev/stages.rs`'s `toolchain_plan` orders cross-target
steps `kernel headers → libc headers (--nodeps, USE=headers-only) →
gcc-stage1 → libc (full) → gcc-stage2` (lines ~352-393). At the "libc
headers" step, no target-arch cross-compiler exists yet (gcc-stage1 hasn't
run), so `CC` falls back to the host's own native compiler
(`aarch64-unknown-linux-gnu-gcc` on this machine) — wrong arch entirely.
Real `sys-libs/glibc`'s `do_src_configure` runs the same full autoconf
configure for `headers-only` builds as for a real build, including
`sysdeps/x86`-specific codegen-sensitive checks (`-Os inlines trunc`)
that a wrong-arch compiler can't answer correctly. A handful of
`libc_cv_*` cache variables are pre-answered for `is_crosscompile` in the
real ebuild, but not this one.

## Why riscv64 doesn't hit it

`-Os inlines trunc` is an `sysdeps/x86` fragment (confirmed in the build
log: "running configure fragment for sysdeps/x86"). `em --target
riscv64-unknown-linux-gnu crossdev --setup` completed all 7 steps clean
and produced a working cross-compiler (verified: compiled and linked a
real riscv64 ELF PIE binary, correct ABI). riscv64's sysdeps tree never
runs this particular check, so the wrong-arch-CC problem never surfaces
there — but the underlying issue (wrong CC at the libc-headers step) is
real regardless of arch; other arches' sysdeps fragments may have their
own codegen-sensitive checks that would hit the same class of failure.

## What real crossdev does differently (empirical, same board/session)

A real `crossdev i586-pc-linux-gnu --gcc 15.3.9999 --ex-pkg ...` run in
the same sandbox succeeded through binutils, gcc-stage1,
linux-headers-stage1, **glibc**, gcc-stage2, clang-crossdev-wrappers, and
rust-std (only failed at the unrelated final `grub` ex-pkg step — a
separate, pre-existing crossdev/grub issue, not connected to this). Its
logged step order was:

```
binutils → gcc-stage1 → linux-headers-stage1 → glibc → gcc-stage2
```

No separate "glibc headers-only" pass at all — by the time real crossdev
touches glibc in any way, gcc-stage1 (a real, if minimal, i586-targeting
compiler) already exists, so glibc's configure gets a working target CC
and the x86 codegen checks pass normally. It also builds gcc-stage1
*before* kernel-headers, contradicting `stages.rs`'s code comment
("gcc-stage1 needs [libc headers]") — worth re-verifying whether that
comment's premise (that GCC's own stage1 configure needs
`--with-headers` pointing at real extracted headers) is actually true,
or whether GCC's stage1 (freestanding, `-nostdlib`-style per
`GCC_DISABLE_STAGE1`) can configure without them the way real crossdev's
own sequence implies.

## How to attack

1. Read real crossdev's actual bash implementation (`/usr/bin/crossdev`
   or the eclass it drives) to confirm its exact step order and *why*
   gcc-stage1 can run before any headers exist — settle whether
   `stages.rs`'s "gcc-stage1 needs libc headers" comment is accurate.
2. If gcc-stage1 genuinely doesn't need real headers first, reorder
   `toolchain_plan`'s cross branch to match real crossdev: binutils →
   gcc-stage1 → kernel-headers → libc (single pass, no separate
   headers-only step) → gcc-stage2 — dropping the standalone "libc
   headers" step entirely rather than just moving it.
3. Re-verify across every arch already covered by
   `portage-cli/src/crossdev/target.rs`'s tests (riscv64, aarch64, i586,
   musl, newlib/bare-metal) — a reorder that fixes x86 must not
   regress the archs that currently work, and bare-metal (newlib,
   `has_kernel: false`) skips the kernel-headers step entirely so needs
   separate attention.
4. Re-run this same i586 pilot (`crossdev-stages sandbox pilot-i586-em`)
   end to end once changed, plus the riscv64 one as a regression check.
