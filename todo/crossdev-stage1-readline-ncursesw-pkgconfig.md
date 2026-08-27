# `sys-libs/readline` can't find `ncursesw` in a board `--root` stage1

Status: 🟡 partially fixed and live-verified — pkg-config layer done;
linker layer (same underlying tension, one level down) still open.
Found 2026-08-27 continuing
[[crossdev-stage1-board-root-header-search]]'s live board `stage1` run
once that bug was fixed.

## The bug (pkg-config layer — FIXED)

Real `sys-libs/readline`'s `src_prepare()` calls
`$(tc-getPKG_CONFIG) ncursesw --libs` (bug #457558's fix), routed
through `em select pkgconf`'s `<CTARGET>-pkg-config` wrapper. Confirmed
live (wrapper instrumented then reverted): the wrapper scoped
`PKG_CONFIG_LIBDIR` off `PKG_CONFIG_ESYSROOT_DIR` (derived from
`SYSROOT`/`ESYSROOT`) only — the crossdev toolchain sysroot. But
`ncursesw.pc` actually lives at
`/board-pentium-mmx/usr/lib/pkgconfig/ncursesw.pc` — `ROOT` (the board
root), not the toolchain sysroot, since sibling stage1 packages install
progressively into `--root`.

**Fixed**: `portage-cli/src/select/pkgconf.rs`'s `SCRIPT_TEMPLATE` now
unions `PKG_CONFIG_LIBDIR` (and the leaked-host-path safety net's
allowlist) across both the sysroot and `ROOT` whenever they differ — a
no-op in every other topology, where they're already the same path.
Live-verified: `readline`'s `src_prepare` (the `ncursesw --libs` call)
now succeeds.

## The bug (linker layer — NOT yet fixed)

Same run now gets to `src_compile`, and fails differently:

```
/usr/aarch64-unknown-linux-gnu/i586-pc-linux-gnu/binutils-bin/2.47/ld: cannot find -lncursesw: No such file or directory
/usr/aarch64-unknown-linux-gnu/i586-pc-linux-gnu/binutils-bin/2.47/ld: cannot find -ltinfow: No such file or directory
collect2: error: ld returned 1 exit status
```

`pkg-config` now correctly reports `-lncursesw -ltinfow` (the fix
above), but the actual `libncursesw.so`/`libtinfow.so` files — real,
installed at `/board-pentium-mmx/usr/lib/` by the earlier `ncurses`
merge — aren't on the **linker's** own search path. That path comes
from `gcc`/`ld`'s own `--sysroot`-driven default library search
(`$SYSROOT/usr/lib`), which is still scoped to the toolchain sysroot
only — the same underlying tension as
[[crossdev-stage1-board-root-header-search]], one level down: headers
are fixed (pkg-config's `.pc` search is now fixed too), but the
compiler/linker's own built-in `-L` default isn't part of either fix.

Unlike the `PKG_CONFIG_LIBDIR` fix, this can't be patched in a wrapper
script — it's `gcc`'s/`ld`'s own default search behavior, driven by
whatever `em`'s `shell.rs` exports as `SYSROOT`/`ESYSROOT`/`LDFLAGS`
for this phase.

## A structural question worth checking first

`sys-libs/glibc` itself is package **71 of 97** in this stage1 plan —
far *after* `sys-libs/ncurses` (5), `sys-libs/readline` (16), and
several others. That means the board root has no libc of its own at
all until quite late; every package before it necessarily links
against the *toolchain's* libc, not a board-root one. Worth confirming
whether that ordering is actually correct/intentional (dependency-
solver-driven, `USE=build`-simplified deps not requiring board-root
glibc as a build-time dependency) or itself a bug in
`stages::stage1_plan`'s ordering for the cross branch — if `glibc`
should come much earlier, some of this class of "board root vs.
toolchain sysroot" tension might be inherent only to the *early* part
of the plan and resolve itself once the board root has its own libc.

## How to attack

1. Confirm the `glibc`-position-71 ordering question above first —
   changes how much of the rest matters right now.
2. Find where `em`'s `shell.rs` exports `SYSROOT`/`ESYSROOT`/whatever
   drives `gcc`'s effective `--sysroot`/library search default for an
   ordinary (non-host-codegen) target package build under this
   topology, and whether it can gain the same `ROOT`-union treatment
   the pkg-config wrapper just did — or whether, per
   [[crossdev-stage1-board-root-header-search]]'s open question 2, the
   real fix is narrower (don't force `SYSROOT` = toolchain sysroot for
   every board-root package, only the bootstrap-critical early ones).
3. Reproduce minimally: `em --target i586-pc-linux-gnu --root
   /board-pentium-mmx --nodeps sys-libs/readline` (real crossdev-stages
   sandbox, `em-i586-check`; `ncurses` already merged there as of this
   writing, `PKG_CONFIG_LIBDIR` fix already landed).
4. Once fixed, re-run the *whole* board `stage1` (not just this one
   package) plus a bare `--target` `crossdev --setup` as a regression
   check — this topology now touches both.
