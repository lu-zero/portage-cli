# `em crossdev` needs a GCC version/slot pin flag (real crossdev's `--gcc`)

Status: 🔴 not started. Found 2026-08-24 while auditing whether `em` can
replace real `crossdev`/`emerge` in `~/Sources/crossdev-stages` (a separate
project building bootable board images) — see that project's own
`todo/em-replaces-crossdev-gcc-pin.md` for the caller-side workaround.

## The gap

Real `crossdev` takes `--gcc VERSION` as a first-class flag, pinning the
cross-compiler to a specific slot or version prefix
(`crossdev-stages/src/sandbox.rs::setup_crossdev` uses this today, driven
by `board.conf`'s `BOARD_GCC_VERSION`). `em crossdev` has no equivalent —
`sys-devel/gcc` always resolves to whatever the current
`package.accept_keywords`/mask config picks
(`resolve_gcc_version`/`maybe_weave_in_gcc_update`, `crossdev/mod.rs`).

A caller can work around this today by writing `package.accept_keywords`/
`package.mask` entries into the target-prefix config before calling
`em crossdev --setup` (same mechanism `setup_crossdev` already uses to pin
the *host* gcc slot) — but that's an indirect, undocumented convention
every caller has to reinvent, not a feature `em crossdev --help` advertises.
Since `em crossdev` bills itself as a "crossdev workalike," lacking the one
flag real crossdev users reach for most for a non-default toolchain is a
real parity gap, not just crossdev-stages' problem to route around.

Same underlying need surfaced again testing `--ex-pkg sys-devel/
clang-crossdev-wrappers`: newest-wins resolution picked LLVM 23 (a
brand-new, entirely unkeyworded major version), cascading through
multiple `--autounmask-write` passes to accept `clang`/`clang-common`/
`llvm` one at a time, when the stable, fully-keyworded `llvm:22` would
have resolved cleanly with zero mask changes. See
[[autounmask-cascading-fresh-slot-vs-version-pin]] — worth designing
together, not separately.

## Shape (not decided yet)

- `--gcc <SLOT|VERSION-PREFIX>` mirroring crossdev's own semantics: a bare
  number ("15") pins the slot, anything longer ("15.2", "15.2.1_p...") is a
  version-prefix glob.
  - Slot pin: equivalent to writing `sys-devel/gcc:<slot> **` into
    `package.accept_keywords` for the target-prefix config, same shape
    `setup_crossdev` already does for the host.
  - Version-prefix pin: `em` doesn't have a `=pkg-ver*` glob-based
    resolve path the way `emerge '=sys-devel/gcc-15.2*'` does — probably
    needs a `package.mask` window (`<sys-devel/gcc-15.3 >=sys-devel/gcc-15.2`
    or similar) rather than a direct glob equivalent. Needs its own design
    pass, not just copying crossdev's CLI shape.
- Where it plugs in: `resolve_gcc_version`'s depgraph probe would need to
  respect the pin (mask/accept_keywords written before the probe runs, or
  the probe itself gains a pin parameter) — and `maybe_weave_in_gcc_update`
  (the stage1 gcc-refresh check) needs to agree with whatever the pin
  resolves to, or it'll flag a spurious "needs refresh" against the
  unpinned default.

## How to attack

1. Decide the flag shape (CLI parse, slot-vs-prefix disambiguation) —
   mirror `crossdev-stages/src/sandbox.rs::setup_crossdev`'s existing
   `PortageVersion`-based slot/prefix split; it's already solved once
   there.
2. Wire it into `resolve_gcc_version` (write the mask/accept_keywords
   before the probe, or thread the pin through the depgraph call
   directly — TBD which is cleaner).
3. Regression-test against a real board build in crossdev-stages once
   landed, per the retest plan.
