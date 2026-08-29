# `--root` under `--prefix`/`--local` + `--target`: not fixed, needs a real design

Status: 🔴 open, design needed — two attempted fixes both landed and
reverted the same day (2026-08-29: `b2aaa8b`→`bef63f4`, `c0d3dc2`→`8610863`).
Found live-testing whether the riscv64 crossdev-stages recipe (see
[[crossdev-setup-pretend-cold-target-gap]] and neighbors) also works
under `--prefix`.

## The original symptom

`em --prefix P --target T crossdev --setup` (no `--root`) works cleanly —
verified live end-to-end (x86_64, all 6 steps). But `stages --stage1`
against that *same* toolchain, given a separate board `--root B`, fails:

```
!!! cannot resolve make.profile at B/usr/T/etc/portage/make.profile: No such file or directory
```

— it looks for config at `B/usr/T`, not `P/usr/T` where `--setup` (run
without `--root`) actually put it.

## Why this isn't a simple "anchor on eprefix instead of merge_root" fix

`Cli::roots()` is the single function computing the sysroot's config/base
*and* destination for **every** caller under `--target` — both
`crossdev`/`toolchain --setup`'s own internal merges of the sysroot
itself (baselayout/headers/libc) and ordinary operations against an
already-existing target (`stages --stage1`, plain merges). These two
callers need genuinely different, and *both individually correct*,
`--root` semantics:

- **`stages --stage1` / ordinary merges**: `--root B` should mean "toolchain
  config still comes from `P/usr/T`; install this operation's packages
  flat into `B`" (`B` is a whole separate board filesystem, not a sysroot
  subdirectory).
- **`crossdev`/`toolchain --setup`'s own sysroot-building steps**: today,
  `--root B` symmetrically displaces the *whole* sysroot to `B/usr/T`
  (config via the separate `sysroot()` helper, package merges via
  `Cli::roots()`) — self-consistent as it stands. Meanwhile host-side
  `cross-*/binutils`/`gcc` (the compiler's own files) already correctly
  install *flat* at `B` via a separate, untouched path (`outer_roots()`),
  since the compiler's own `--prefix=/usr` build wants `B/usr/bin/gcc`,
  not further nesting.

Both landed attempts changed `Cli::roots()` globally to serve the first
caller (correctly verified live), which broke the second: headers/libc
during `--setup --root B` planned installing into flat `B` while
`sysroot()`-derived config (`--with-sysroot=`) still pointed at
`B/usr/T` — an actual `--with-sysroot` vs. real-file-location mismatch,
confirmed live (`powerpc64le-unknown-linux-gnu`, `-p` preview), not just
a "different but valid" shape.

## Open question

Some mechanism needs to distinguish "this merge is part of building the
sysroot itself" from "this merge is an ordinary operation against an
existing target," so each can get its own `--root` semantics — without
just threading a new override field generically through every call site
(rejected as unneeded complexity in the second attempt). Needs a cleaner
design before touching `Cli::roots()` again. Both fix attempts are fully
reverted; current (pre-2026-08-29) behavior is back in place, symmetric
displacement under `--setup --root B`, and `stages --stage1 --root B`
under `--prefix` still fails as described above.
