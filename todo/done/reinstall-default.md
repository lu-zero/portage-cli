# Explicit-target reinstall default (emerge `foo && foo` → two installs)

STATUS: **done — core fixed 2026-06-22; follow-up symptom live-verified closed
2026-07-29 (did not reproduce).**
Behaviour difference vs emerge: emerge *reinstalls an explicitly-requested atom
by default* (`[ebuild   R   ]`), so `emerge foo && emerge foo` builds foo twice;
`--noreplace`/`-n` opts out. `em` instead **skipped** a target already in the VDB
at the planned version.

DONE:
- `fix(merge): reinstall an explicitly-requested atom already in the VDB`
  (`7f43c27`): `PlannedMerge.reinstall` (= cpv already installed yet still in the
  plan ⇒ explicit target / USE rebuild); the merge loop builds it instead of
  resume-skipping.
- `fix(merge): treat a same-cpv reinstall as a self-replace` (`bb89327`): dropped
  the `find_slot_occupant(...).filter(|old| old.cpv() != ebuild.cpv())` so the
  installed package is the replace target — own files exempt from collision
  detection, unmerged after the new content lands.

Verified: cross `--setup` now rebuilds **all 6 steps, no skips, no collisions**.

CLOSED (2026-07-29, live sandbox) — **staged-glibc-headers-only did not
reproduce.** Ran a genuine fresh `em --target riscv64-unknown-linux-gnu
crossdev --setup` end-to-end in a real `crossdev-stages` sandbox
(`em-reinstall-usecheck-0729`, aarch64 host stage3), no shortcuts. All 6 steps
completed; inspected the actual on-disk result rather than the plan/pretend
output:
- `cross-riscv64-unknown-linux-gnu/glibc-2.43-r2` VDB: `IUSE` lists
  `headers-only`, `USE` does **not** — the step-5 reinstall correctly recorded
  it off. `CONTENTS` is 1581 lines / 165KB (not the reported 622-byte stub);
  real `libc.so`/`libc.so.6` present under the sysroot.
- `cross-riscv64-unknown-linux-gnu/gcc-16.1.1_p20260718` VDB: `USE` has `+cxx`;
  real `libstdc++.so`/`libstdc++.so.6.0.35` present under
  `/usr/lib/gcc/riscv64-unknown-linux-gnu/16/` (gcc-stage2's own runtime-lib
  path, not the sysroot tree — the earlier "missing" read on this same
  investigation was just a wrong search path, not an absence).

Both symptoms this note described are simply not present in a real run. This
matches the earlier full static trace (this session) of the USE-flag
propagation chain, which found no caching/staleness bug at any of ~7 layers
from `crossdev/stages.rs` down to the spawned build shell. Whatever produced
the original 622-byte/no-libstdc++ observation did not reproduce here —
likely fixed incidentally by one of the two 2026-06-22 commits below, or was
specific to a since-changed environment. Not chasing further without a fresh
repro.

## Why it matters now

The cross toolchain bootstrap ([[crossdev-target]]) is staged as repeated merges
of the *same CPV* with different USE: `glibc[headers-only]`→`glibc[]`,
`gcc[stage1]`→`gcc[stage2]`. Each later stage explicitly names the package, so
the emerge **replace** default would rebuild it. `em` skips it →
full-glibc/gcc-stage2 never build (no `libc.so`; gcc is stage1-only).

## Mechanism (where em diverges)

- `run_merge_plan` (main.rs ~335) treats a plan entry already recorded in the
  target VDB at the planned version as *resume → skip*. That conflates two cases:
  (a) merged earlier **in this same run** (legit resume), and (b) pre-existing
  from a prior invocation (emerge would still reinstall an explicit target).
- The resolver also needs to *list* an installed explicit target as `R` rather
  than dropping it (it mostly does for the toolchain steps — they showed `R`/`U`).

## Fix direction

Distinguish "merged during this run" from "already in VDB at start". Skip only
the former (true resume). An explicitly-requested atom is reinstalled even at the
best installed version (emerge default); a `--noreplace`/`-n` flag restores the
skip. Satisfied *dependencies* are still not reinstalled (only named atoms get
the replace treatment), so this is not `--emptytree`.

Relationship: [[newuse]] is the USE-aware, deps-included rebuild; this item is
the blunt "named atom always reinstalls" emerge default. The toolchain needs the
latter (stages name the package); both are worth having.
