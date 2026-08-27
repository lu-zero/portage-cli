# `sys-libs/readline` can't find `ncursesw` in a board `--root` stage1

Status: 🟢 fully fixed and live-verified. Found 2026-08-27 continuing
[[crossdev-stage1-board-root-header-search]]'s live board `stage1` run
once that bug was fixed. The "linker layer" section below was the
working theory at the time this doc was first written — it turned out
to be the wrong layer entirely; see "The real fix" for what actually
shipped.

## The bug (pkg-config layer — landed, superseded below)

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

`portage-cli/src/select/pkgconf.rs`'s `SCRIPT_TEMPLATE` unions
`PKG_CONFIG_LIBDIR` (and the leaked-host-path safety net's allowlist)
across both the sysroot and `ROOT` whenever they differ — a no-op in
every other topology, where they're already the same path. This landed
first and made `readline`'s `src_prepare` succeed, but it's
defense-in-depth, not the actual fix — see below.

## The bug (linker layer — this theory was wrong)

The same run then got to `src_compile` and failed differently:

```
/usr/aarch64-unknown-linux-gnu/i586-pc-linux-gnu/binutils-bin/2.47/ld: cannot find -lncursesw: No such file or directory
/usr/aarch64-unknown-linux-gnu/i586-pc-linux-gnu/binutils-bin/2.47/ld: cannot find -ltinfow: No such file or directory
collect2: error: ld returned 1 exit status
```

The working theory at the time was that this was one more "board root
vs. toolchain sysroot search path" instance of
[[crossdev-stage1-board-root-header-search]], and that the fix was
somewhere in how `em`'s `shell.rs` exports `SYSROOT`/`ESYSROOT`/`LDFLAGS`
for the linker's own default `-L` search. That diagnosis was never
implemented — digging into *why* real `crossdev`/`cross-emerge` never
hits this at all (asked directly: "how come cross-emerge doesn't have
such problems and we did not have such problems before?") led somewhere
else entirely.

## The real fix

Real Portage's own dependency resolver **double-plans** a `DEPEND`-class
provider into the toolchain sysroot as a second, separate merge-list
entry (PMS table 8.2: `DEPEND` resolves against the base system —
`SYSROOT`/`ESYSROOT`, not `ROOT`, whenever `--target` and `--root`
diverge). Confirmed via a from-scratch, apples-to-apples real-crossdev
control test (`real-i586-check` sandbox, genuinely fresh/empty-VDB
toolchain sysroot): `ROOT=/realtarget i586-pc-linux-gnu-emerge -b -k
sys-libs/ncurses sys-libs/readline` produces **3** merge-list entries —
`ncurses` into the board root (satisfies `readline`'s `RDEPEND`) *and*
`ncurses` into the toolchain sysroot (satisfies `readline`'s `DEPEND` —
what the compiler/linker actually see at build time) — plus `readline`
itself. `em`'s solver never did this: a single-rooted solve only ever
produced the first entry, so the sysroot never got its own copy of
`ncursesw`/`tinfow`, and the linker search path was never the bug — it
was correctly looking at the sysroot, which was simply missing the
library it needed.

Two commits:

- `ee8339c` — `portage_resolve::base_copies`, a post-solve closure walk
  (sibling to the existing `host_copies`) that schedules a `DEPEND`
  provider's toolchain-sysroot copy as a new `MergeRoot::Base` plan
  entry, wired into `depgraph()`. The easy-to-miss second half: the
  merge **execution** layer's `entry_roots()` (`merge/mod.rs`) only
  distinguished Host vs. everything-else, so a `Base` entry silently
  routed to the board root and got skipped as "already installed" once
  the Target entry for the same cpv landed there first — the sysroot
  copy never actually built even though the plan looked correct under
  `-p`. Fixed with `Cli::sysroot_roots()` + a `base_roots` field on
  `MergeRun`.
- `48e0fb3` / `fc50a9e` — the activity-log display and `Arc<str>`/typed
  `Cpv`/`Cpn` follow-up (unrelated to the fix itself, found while
  reviewing the `Base`-entry banner text).

The `PKG_CONFIG_LIBDIR` union fix above stays landed as defense-in-depth
(covers a `.pc` file genuinely only present under `ROOT` for some other
reason), but it is no longer the thing making `readline` build — the
sysroot now has real `ncursesw`/`tinfow` `.so` files via `base_copies`,
so pkg-config *and* the linker both find them at the toolchain sysroot,
same as real crossdev.

## Live verification

`em --target i586-pc-linux-gnu --root /board-fresh-check sys-libs/readline`
against `em-i586-check` (fresh board root, no prior `ncurses`/`readline`
state): plan shows the expected 3 entries (`ncurses` → board root,
`ncurses` → sysroot, `readline` → board root), matching the real-crossdev
control run byte-for-byte in shape. Real (non-`-p`) build: all 3 merge,
`libncursesw.so`/`libtinfow.so` land in `/usr/i586-pc-linux-gnu`,
`readline` links successfully — the `ld: cannot find -lncursesw` failure
this doc opened with is gone.

## Structural question from the original investigation (still open, lower priority)

`sys-libs/glibc` is package **71 of 97** in the full board `stage1`
plan — far *after* `sys-libs/ncurses` (5), `sys-libs/readline` (16), and
several others. That means the board root has no libc of its own at all
until quite late; every package before it necessarily links against the
*toolchain's* libc, not a board-root one. Worth confirming whether
that ordering is actually correct/intentional (dependency-solver-driven,
`USE=build`-simplified deps not requiring board-root glibc as a
build-time dependency) or itself a bug in `stages::stage1_plan`'s
ordering for the cross branch. Not blocking — `base_copies` now handles
the general case regardless of ordering — but worth a look if other
"board root vs. toolchain sysroot" surprises turn up in the full
`stage1` re-run below.

## Follow-up still needed

Re-run the *whole* board `stage1` (not just `readline`) plus a bare
`--target` `crossdev --setup` as the final regression check — this
topology now touches both, and neither has been re-verified end-to-end
since `base_copies` landed (only individual atoms have been).
