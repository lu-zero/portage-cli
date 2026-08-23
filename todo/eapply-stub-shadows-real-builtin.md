# `eapply` silently no-oped in every real build — PATCHES never applied

Status: ✅ landed 2026-08-23. Found while investigating a new bug report:
`em --root DIR unzip` "tries to build bzip2 for no real reason [and]
bzip2 fails horribly to build". The bzip2 failure was real and led here;
the `app-arch/bzip2` pull-in itself is correct (unzip's `IUSE="bzip2 ..."`
is default-on, `DEPEND="bzip2? ( app-arch/bzip2 )"` — matches real
portage, not a bug).

## The bug

`portage-repo/src/build/stubs.rs` defines a bash function
`eapply()  { :; }` (a no-op, needed only so metadata extraction — which
never runs real phases — doesn't abort on "command not found" while
sourcing an ebuild). `portage-repo/src/build/shell.rs` separately
registers a real Rust `eapply` builtin (`commands::EapplyCommand`,
`f811d8a`) that actually shells out to `patch`.

`init_build_env` (`shell.rs:1387-1390`) runs an `unset -f` list every
phase specifically to remove these metadata-mode stubs so the real
builtins take over for an actual build — `econf`, `emake`, `einstall`,
`unpack`, the `e*` output builtins, `docompress`/`dostrip`, all the
`do*`/`new*` install helpers. **`eapply` was missing from that list.**
A bash *function* always shadows a same-named *builtin* in this shell
(confirmed empirically — `emake`/`econf`, which *are* unset, produce real
subprocess execution; `eapply`, not unset, silently ran the stub instead
of the registered builtin), so every real build's `eapply` call —
whether from an ebuild calling it directly or, far more commonly, from
`default`/`default_src_prepare` (`__eapi6_src_prepare`/
`__eapi8_src_prepare`) walking `PATCHES` — returned success and modified
nothing. No error, no `die`, no log line: `src_prepare` completed
normally either way.

This affects **every real build (`--local`/`--root`/`--prefix`) this
whole session**, not just bzip2: any ebuild with a `PATCHES` array (the
overwhelming majority of the tree) built from genuinely unpatched
upstream sources. It only surfaced as a hard failure when the missing
patch was load-bearing for the exact configuration being built (bzip2's
`out-of-tree-build.patch`, needed for `multilib-minimal`'s per-ABI
`BUILD_DIR` — unpatched `Makefile-libbz2_so`'s hardcoded
`blocksort.o: blocksort.c` rule can't find the source file when `make`
runs from a different directory than `${S}`). Most other packages either
have no PATCHES, or PATCHES that aren't strictly required to produce a
working (if slightly non-canonical) build, so this went unnoticed.

## Root-caused with a minimal repro, not by staring at bash

Reasoning about the `restore_baseline`/`invalidate_baseline`
carried-shell-state machinery in isolation looked correct on paper (and
is) — the real defect was one function name missing from an unrelated
`unset -f` string. Found by: (1) a `#[tokio::test]` driving
`Shell::run_phase` across `unpack`→`prepare` with a synthetic ebuild and
a trivial one-line patch, confirming the patch never applied even though
the `PATCHES` array and the `default`/`default_src_prepare` function
chain were all correctly wired; (2) a manual `eapply "${PATCHES[@]}"`
call from the same live shell, still silently no-op with `RC=0`; (3) a
direct Rust-level call to `eapply::apply_all` with identical arguments,
which worked — isolating the bug to the brush-level name resolution, not
the Rust patch-application logic; (4) grepping for `unset -f` call sites,
which turned up the exact list missing `eapply`.

## The fix, three passes

**First (superseded)**: add `eapply` to the `unset -f` list in
`init_build_env`. Fixed the reported bug, but a mechanical audit found
this class of bug could recur anywhere a metadata-mode stub name and a
Rust builtin name coincide without being unshadowed — bash-text stubs and
the builtin registry are two independent mechanisms kept in sync only by
convention.

**Second (superseded further)**: eliminated the bash-shadowing mechanism
for all 38 names that had both a stub and a builtin — `commands/
dual_mode.rs`, one table, `set_tool_mode(shell, mode)` registering either
the real builtin or a small Rust stub, no bash function ever defined for
these names. Also fixed a *second*, reverse-direction bug found live: a
reused shell doing metadata-only work after a real build previously kept
dispatching the real, unshadowed `eapply` — `source_ebuild` now
re-asserts metadata mode on every call.

**Third (landed)**: `source_ebuild` only sources ebuild/eclass global
scope and *defines* phase functions — it never *calls* one. `econf`/
`emake`/`eapply`/`einstall`/`unpack`/`docompress`/`dostrip`/the `do*`/
`new*` install helpers are PMS phase-body-only helpers, so they were
never reachable during metadata-only sourcing regardless of what was
registered for them — the "stub" half of those 28 entries did nothing,
ever. Trimmed the table to the 10 names eclasses actually can call from
global scope (`einfo`/`einfon`/`elog`/`ewarn`/`eerror`/`eqawarn`/
`ebegin`/`eend`/`has_version`/`best_version`); the other 28 are now
registered once, unconditionally, exactly like `die`/`has`/`use`. Note:
the original stubs existed because this shell's founding purpose was
md5-cache computation (source global scope, extract `DEPEND`/`IUSE`/…,
detect `DEFINED_PHASES` from which functions got defined — never execute
one), where build/install helpers are simply irrelevant — not for any
safety or performance reason. Measured to confirm: pinned `hyperfine` A/B
(NUMA node, 12 runs each) of a full non-incremental `em regen` over the
real `::gentoo` tree (32,934 ebuilds) comparing the 38- vs 10-entry
table — 1.01× (noise, not signal): performance-neutral, as expected once
the call sites were confirmed unreachable either way.

## Verified

- `cargo nextest run --workspace`: 1967/1967 pass (final architecture);
  `cargo fmt --check` / `cargo clippy --workspace --all-targets`: clean.
- `build::shell::tests::default_src_prepare_applies_patches_set_during_an_earlier_phase`:
  synthetic ebuild, `unpack` then `prepare` phase, asserts the
  EAPI-8-default `PATCHES` handling actually rewrites the file.
- `build::shell::tests::metadata_scan_after_a_real_build_gets_stubs_not_real_builtins`:
  proves the reverse-direction bug is closed — a real phase, then
  `source_ebuild` on a different package, then confirms `eapply` is back
  to its safe no-op.
- Live, real (non-pretend) `em --root DIR unzip`, re-run twice (once
  after the one-line fix, once after the final architecture, both from a
  clean root): previously `EXIT=1` with `app-arch/bzip2-1.0.8-r5` dying
  (`cc1: fatal error: blocksort.c: No such file or directory` — the
  unpatched `Makefile-libbz2_so` compiling from the wrong per-ABI
  `BUILD_DIR`). After the fix: `EXIT=0`, all three packages (`bzip2`,
  `app-alternatives/bzip2`, `unzip-6.0_p31`) complete, and the installed
  `unzip` binary runs.

## Side effect: likely also explains (part of) the deferred GCC-16 unzip finding

This same repro's dependency resolution picked `app-arch/unzip-6.0_p31`
directly (not the `~arch`-gated `p29-r2` [[local-bootstrap-unzip-gcc16-prototype]]
was filed against), so that specific, already-documented `p29-r2`/GCC-16
K&R-prototype bug was not exercised here and is **not** confirmed fixed
by this change — that bug's root cause is a real source-level conflict
in `p29-r2`'s own `unxcfg.h`, unrelated to any Gentoo `PATCHES` entry (the
todo file already confirmed `p31`'s patch set doesn't touch that file
either; the fix is in `p31`'s newer upstream tarball). Left open as its
own item, now just noting that today's `--root` run went through `p31`
instead and never hit it. Separately, `p31`'s own `PATCHES` (a Debian
patch directory plus several fixes) had *never* been applied by any
real build until this fix landed — worth a fresh look at whether any
previously-"successful" `--local`/`--root` build silently shipped
unpatched sources for something that matters.

## Residual: other real builds done earlier this session may need re-verifying

Every package built for real earlier this session (`dev-lang/python`,
`sys-apps/gentoo-functions`, etc.) was built without any of its
`PATCHES` applied. None hit a hard failure, so they weren't flagged, but
"built successfully" for those runs doesn't mean "built exactly as
Gentoo intends" — worth keeping in mind if something built earlier
behaves oddly later.
