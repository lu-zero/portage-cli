# `sys-libs/readline` can't find `ncursesw` via pkg-config in a board `--root` stage1

Status: 🔴 not started, just found. Found 2026-08-27 continuing
[[crossdev-stage1-board-root-header-search]]'s live board `stage1` run
once that bug was fixed — a completely different mechanism (pkg-config
search path during `src_prepare`/autoreconf, not header search).

## The bug

`em --target i586-pc-linux-gnu --root /board-pentium-mmx stages --stage1`
gets 15/97 packages in (including `sys-libs/ncurses-6.5_p20251220`
itself, merged cleanly at package 5/97), then fails at
`sys-libs/readline-8.3_p3`'s `src_prepare` phase:

```
>>> Emerging (16 of 97) sys-libs/readline-8.3_p3 for /board-pentium-mmx/
...
Package ncursesw was not found in the pkg-config search path.
Perhaps you should add the directory containing `ncursesw.pc'
to the PKG_CONFIG_PATH environment variable
Package 'ncursesw' not found
die:
>>> Failed to emerge sys-libs/readline-8.3_p3 — build log: ...: phase prepare failed: shell error: src_prepare: die:
```

## What's confirmed

- `ncurses` itself installed successfully one package group earlier
  (package 5/97) into the same board root, so the `.pc` file should
  exist somewhere under `/board-pentium-mmx/usr/...` (or the sysroot,
  depending on how `virtual/pkgconfig`/`ncurses` scope
  `PKG_CONFIG_LIBDIR` for this topology) — not yet confirmed which.
- This is `src_prepare`, not `src_configure`/`src_compile` — likely
  readline's build system regenerating its own `configure` via
  autoreconf/aclocal and probing for `ncursesw.pc` as part of that
  regeneration, before the normal econf/PKG_CONFIG_SYSROOT_DIR
  machinery would apply. Not yet confirmed whether the `<CTARGET>-pkg-config`
  wrapper ([[crossdev-pkg-config-sysroot-leak]]) is even being invoked
  here, or whether `src_prepare` calls raw `pkg-config` before any
  wrapper/env setup takes effect.

## How to attack

1. Find the exact `pkg-config`/`pkgconf` invocation this failure comes
   from (build log's `src_prepare` output, above the "not found" line)
   — which binary got called (`i586-pc-linux-gnu-pkg-config`, `BUILD_PKG_CONFIG`,
   or bare `pkg-config`?) and what `PKG_CONFIG_LIBDIR`/`PKG_CONFIG_PATH`
   it saw.
2. Confirm where `ncursesw.pc` actually landed for this board root
   (`find /board-pentium-mmx -name 'ncursesw.pc'` in the sandbox) and
   whether that path is on the search path the failing invocation used.
3. Reproduce minimally: `em --target i586-pc-linux-gnu --root
   /board-pentium-mmx --nodeps sys-libs/readline` (real crossdev-stages
   sandbox, `em-i586-check`, board root already has `ncurses` merged as
   of this writing) — no need to redo the whole stage1 plan each time.
