# `sys-libs/readline` can't find `ncursesw` via pkg-config in a board `--root` stage1

Status: 🔴 root-caused, not fixed — genuine design question, not a quick
patch. Found 2026-08-27 continuing
[[crossdev-stage1-board-root-header-search]]'s live board `stage1` run
once that bug was fixed.

## The bug

`em --target i586-pc-linux-gnu --root /board-pentium-mmx stages --stage1`
gets 15/97 packages in (including `sys-libs/ncurses-6.5_p20251220`
itself, merged cleanly at package 5/97), then fails at
`sys-libs/readline-8.3_p3`'s `src_prepare`:

```
Package ncursesw was not found in the pkg-config search path.
Package 'ncursesw' not found
die:
```

Real `sys-libs/readline`'s `src_prepare()` calls
`$(tc-getPKG_CONFIG) ncursesw --libs` directly (bug #457558's fix) —
routed through `em select pkgconf`'s `<CTARGET>-pkg-config` wrapper
(`em-cross-pkg-config`).

## Root cause (confirmed live, wrapper instrumented then reverted)

```
SYSROOT=/usr/i586-pc-linux-gnu ESYSROOT=/usr/i586-pc-linux-gnu/ ROOT=/board-pentium-mmx/
PKG_CONFIG_ESYSROOT_DIR=/usr/i586-pc-linux-gnu
PKG_CONFIG_LIBDIR=/usr/i586-pc-linux-gnu/usr/lib/pkgconfig:/usr/i586-pc-linux-gnu/usr/share/pkgconfig
```

`ncursesw.pc` actually lives at `/board-pentium-mmx/usr/lib/pkgconfig/ncursesw.pc`
(confirmed: `ncurses` installed there at package 5/97) — `ROOT`, not
`SYSROOT`/`ESYSROOT`. The wrapper scopes `PKG_CONFIG_LIBDIR` off
`PKG_CONFIG_ESYSROOT_DIR` (derived from `SYSROOT`/`ESYSROOT`), which is
now the crossdev toolchain sysroot — a direct, structural consequence of
[[crossdev-stage1-board-root-header-search]]'s fix (`build_sysroot()`
correctly returning the toolchain sysroot again, so gcc's own header
search works). But that same `SYSROOT` value is *also* what the
pkg-config wrapper uses to find sibling packages — and sibling stage1
packages (like `ncursesw`) install progressively into the **board
root**, not the toolchain sysroot.

This is a genuine conflation, not a simple bug: `SYSROOT`/`ESYSROOT`
serves two different purposes for a board-root build that happen to be
the same path in every *other* topology `em` supports (`--prefix`,
`--local`, bare `--target` with no separate `--root`):

- **Header/lib fallback for the toolchain's own bootstrap-critical
  bits** (glibc, before the board root has its own copy) — correctly
  the crossdev toolchain sysroot.
- **Where sibling packages this build depends on actually live** — the
  board root itself, since stage1 installs its own dependency closure
  there progressively (exactly like real crossdev's own `ROOT=` model,
  where `SYSROOT` and the install root are the same evolving thing).

Only the disposable board-root topology forces these apart; nothing
else in `em` has ever needed to distinguish them.

## What's NOT yet established

Whether the right fix is:
1. The `em-cross-pkg-config` wrapper should scope `PKG_CONFIG_LIBDIR`
   off `ROOT`, not `SYSROOT`/`ESYSROOT`, for this specific topology (or
   even generally — worth checking whether real crossdev's own
   cross-pkg-config wrapper, which this was adapted from, ever
   diverges the two, given real crossdev never has this split).
2. Or `shell.rs` shouldn't be exporting `SYSROOT` = toolchain sysroot
   for *ordinary* target-arch package builds under this topology at
   all (only for the specific bootstrap-critical early steps) — i.e.
   the header-search fix in
   [[crossdev-stage1-board-root-header-search]] may be solving zlib's
   problem via a broader mechanism than necessary, and a narrower fix
   (scoped to just gcc's own `--sysroot`/native header search, not the
   whole `SYSROOT`/`ESYSROOT`/pkg-config-wrapper chain) might avoid
   this class of conflict entirely.

Needs a real design decision, not a quick patch — picking wrong risks
reopening the header-search bug while fixing this one, or vice versa.

## How to attack

1. Read real crossdev's actual `cross-pkg-config` wrapper (or
   `toolchain-funcs.eclass`'s `tc-getPKG_CONFIG`) to confirm whether
   real crossdev ever has `SYSROOT != ROOT` for an ordinary stage
   package build — if never, that's strong evidence for option 2 above
   (this topology split shouldn't leak into `SYSROOT` at all for
   non-bootstrap-critical steps).
2. Reproduce minimally: `em --target i586-pc-linux-gnu --root
   /board-pentium-mmx --nodeps sys-libs/readline` (real crossdev-stages
   sandbox, `em-i586-check`; `ncurses` already merged there as of this
   writing).
3. Whichever fix is chosen, re-run the *whole* board `stage1` (not just
   this one package) plus a bare `--target` `crossdev --setup` as a
   regression check — this topology now touches both.
