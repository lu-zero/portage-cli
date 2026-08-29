# `--root` under `--prefix`/`--local` + `--target`: `Cli::roots()` fixed, `outer_roots()` still open

Status: 🟡 `Cli::roots()` fixed and live-verified 2026-08-29 (`a427f42`,
after two reverted attempts — `b2aaa8b`→`bef63f4`, `c0d3dc2`→`8610863`
earlier the same day). `Cli::outer_roots()` has a related, distinct
bug with a full plan from an Opus review (below), not yet implemented.

## `Cli::roots()` — fixed

`em --prefix P --target T crossdev --setup` (no `--root`) builds the
toolchain at `P/usr/T`; `em --prefix P --root B --target T stages
--stage1` now correctly resolves config from `P/usr/T` while installing
packages into `B`. Landed safely only after `8207e0f` made
`crossdev --setup`/`--init-target` reject `--root` outright (the two
earlier attempts each broke `--setup --root B`'s own semantics instead).

**Live-verified real (non-`-p`) builds**, aarch64 host:
- x86_64-unknown-linux-gnu: `crossdev --setup --prefix P` clean (6/6),
  `stages --stage1 --prefix P --root B` correctly split 84/142 packages
  between `B` and `P/usr/T` before hitting an unrelated bug
  ([[crossdev-stage1-abi-x86-32-multilib-mismatch]]).
- riscv64-unknown-linux-gnu: `crossdev --setup --prefix P` failed at
  `gcc-stage1` (`stdio.h: No such file or directory` building `libgcc`
  — matches [[crossdev-prefix-gcc-header-dir]], previously only seen on
  i586 at gcc-stage2; now also riscv64 at gcc-stage1). The subsequent
  `stages --stage1` run (against the broken toolchain) live-reproduced
  the `outer_roots()` bug below for free: its woven-in gcc-stage1
  refresh planned into `/root/board-riscv/` instead of
  `/root/prefix-riscv/`.

## `Cli::outer_roots()` — real bug, full plan, not yet implemented

Opus review (2026-08-29, read-only) confirmed the overlay branch
applies `--root` under `--target` when it shouldn't, but found the bug
is **three broken topologies, not one**, and refuted two of the three
originally-suspected consequences:

**Broken today:**
1. `--prefix P --root B --target T` → `outer_roots().merge_root()` is
   `B`, should be `P`.
2. `--local L --root B --target T` → also `B`, should be `L` — **not**
   fixed by gating the overlay branch on `is_overlay()`, since
   `--local` never enters that branch (`is_overlay()` is false for
   `Local` — it has its own `base`, see `roots.rs:131-133`). Falls
   through to the bare guard instead, which declines because
   `self.local` is set.
3. An `em active`-registered local + bare `--target` (no `--root` at
   all) → resolves to the real host `/`, not the active local. The
   bare guard keys on raw flags, not `topology_source()`'s resolved
   value.

**Refuted:** `activate_toolchain` and `link_abi_osdirs` are NOT
affected — both are `config_overlay`-driven (never read `merge_root()`),
and `link_abi_osdirs` isn't even reachable from the stage1 gcc-refresh
path (`gcc_refresh_plan` emits only two gcc steps, no libc step). The
sole live consequence is the refresh **merge destination**: host-arch
`cross-T/gcc` installs into the board root `B` instead of `P`, the
freshly-built compiler is invisible to the next `gcc_needs_refresh`
check (which still reads `P`), and the refresh **re-runs and re-pollutes
`B` on every subsequent `stages --stage1` invocation** — not a one-shot
bug, an unbounded repeat.

### Fix (Change 1) — `Cli::outer_roots()`, cli.rs:350-377

Rewrite on the *resolved* topology (`self.topology_source()`), not raw
flags — see the full replacement body and rationale in the review
transcript; net effect: `--prefix`/`--local`/active-local all keep
`outer_roots()` anchored at the prefix under `--target`, regardless of
`--root`; bare is unchanged.

### Fix (Change 2) — reject `--root` + `--target` on `toolchain --setup`

Change 1 makes `--prefix P --root B --target T toolchain --setup`
silently switch its destination from `B` to `P` (today's behavior,
verified). The bare twin already hard-errors ("needs
--prefix/--local/--root: a bootstrap into the bare host / is
meaningless"). Mirror `8207e0f`: bail explicitly instead of a silent
redirect, in `crossdev::toolchain()` (mod.rs ~line 593-604), right
after the `!args.setup` check:
`if globals.target.is_some() && globals.root.is_some() { bail!(...) }`.
Land as its own commit — `--prefix P --target T toolchain --setup`
(no `--root`) is untouched.

### Tests

No existing test needs changing (verified against all three
`outer_roots()` tests plus `require_root_distinct_from_host_rejects_the_degenerate_cases`).
New tests needed, all in `cli.rs`'s `mod tests` near
`outer_roots_ignores_bare_root_under_target`:
- `outer_roots_ignores_prefix_root_under_target`
- `outer_roots_ignores_local_root_under_target` (the row a naive
  `is_overlay()` gate would miss)
- `outer_roots_honours_an_active_local_under_target` (needs
  `test_support::isolate_active_state()`, modeled on
  `active_prefix_applies_when_no_explicit_flag`)
- `crossdev::tests::toolchain_setup_rejects_root_with_target`

### Live-verification recipe

`gcc_needs_refresh` reads its active slot from
`P/etc/env.d/gcc/config-<tuple>`'s `CURRENT=` line
(`current_compiler_slot` → `env_d::get_current_profile`) with no
existence check — editing that file to a lower fake slot is a
deterministic, instant, reversible trigger; no rebuild needed. Full
before/after/restore recipe (with `-p` pre-check and real-run
assertions on `$B/var/db/pkg/cross-$T/` vs `$P/var/db/pkg/cross-$T/`)
is in the review transcript. Do this in a `crossdev-stages` sandbox,
paths under `/var/tmp` not `/tmp`, and not while another build is live
on the box.

## Two new bugs found during the same review

**`require_root_distinct_from_host` is dead under `--target`** (cli.rs
~line 598): it tests `resolved.is_overlay()`, but `roots()`
unconditionally sets `.with_base(Some(sysroot))` under `--target`
(cli.rs:310), so `is_overlay()` is always false there and the
degenerate-root check never fires. **Verified live**: `em --prefix
/tmp/pfxprobe --root /tmp/pfxprobe stages --stage1 -p` correctly
rejects (root == host path); adding `--target riscv64-...` to the
exact same command sails straight past the guard into "cannot resolve
make.profile" instead of the intended rejection — i.e. `--prefix P
--root P --target T stages --stage1` would bootstrap a whole stage1
into the live prefix `P` itself. Fix: test `self.base_roots().is_overlay()`
instead of `resolved.is_overlay()` — checked against every case in
`require_root_distinct_from_host_rejects_the_degenerate_cases`,
all stay green. Filed as its own follow-up — higher severity than the
`outer_roots()` bug above (silent corruption of a live prefix, no
`gcc_needs_refresh` precondition needed), should probably be fixed
first/separately.

**`env_d_dir`'s host fallback can double-register a host profile as a
prefix profile** (`select/env_d.rs:83-93`, `list_all_profiles`): when
`P/etc/env.d/<subdir>` doesn't exist yet, it falls back to
`/etc/env.d/<subdir>` but still labels the result `is_host = false`
(prefix), while `list_all_profiles` separately also collects the same
host directory with `is_host = true` — so `activate_latest`'s
`!p.is_host` filter can pick a **host** compiler profile to activate
into the prefix's `config-<target>`. Narrow precondition (prefix's own
`env.d` subdir absent while the host's exists), lower severity — fix
by having `env_d_dir` return the config-root path unconditionally for
"this root's own env.d" callers, let `list_all_profiles` own the
host-fallback decision explicitly.
