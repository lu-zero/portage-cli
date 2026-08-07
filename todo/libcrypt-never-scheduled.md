# Bug #3 — `virtual/libcrypt` / libxcrypt under empty sysroot (after crossdev only)

Status: 🟡 reframed **2026-08-07** — **stage1 is required**, not optional  
Live: Sonnet `a46027b` re-verify — pam configure: no libcrypt in sysroot  
Handoff: [[for-sonnet]]

## Symptom (what Sonnet saw)

- After **only** `crossdev --setup`, `em --target T -b llvm-core/clang` plans
  ~136 packages including virtual/libcrypt + sys-libs/libxcrypt.
- Virtual reports `>>> Completed` early; **no** `Emerging` for libxcrypt.
- pam fails: no libcrypt in the **sysroot**.

## Correct process diagnosis (@system / stage1)

This is primarily an **@system identity** gap, not “clang shouldn’t pull pam.”

| Layer | What lands | VDB identity |
|-------|------------|--------------|
| **`crossdev --setup`** | Cross toolchain | `cross-<tuple>/glibc`, `cross-<tuple>/gcc`, … |
| **`stages --stage1`** | Profile `packages.build` (`USE=-* build`) | **Real** Cpns: baselayout, shadow, … and a coherent system tree ordinary ebuilds Depend on |
| **`em --target T clang`** | Ordinary target packages | Expect **`sys-libs/glibc`**, not only `cross-T/glibc` |

`sys-libs/libxcrypt[system]` DEPEND is `${CATEGORY}/glibc[-crypt]` → for a
normal package, **`sys-libs/glibc`**. Installed **`cross-T/glibc`** is a
**different Cpn** and does not Favor-satisfy that edge. Without stage1 (or
full @system), the clang graph invents a second libc / half-system world and
scheduling failures (virtual done, provider never started) show up as pam
libcrypt misses.

**Product rule:**  

```text
crossdev --setup  →  stages --stage1  →  ordinary packages (e.g. clang)
```

Stage1 is the **@system seed**. Skipping it is unsupported for fat target
packages, same family as catalyst toolchain → stage1 → world.

Docs: [`docs/crossdev.md`](../docs/crossdev.md) worked example (updated).

## Why pam is still “expected” once you *do* stage1 + clang

```text
clang → python → util-linux → USE=pam → pam → virtual/libcrypt → libxcrypt
```

That chain is real. With stage1 first, more of the base is already present
under controlled USE; clang’s remaining plan should be smaller and should see
real system Cpns. Residual em bugs (scheduling, die, baselayout) can still be
chased, but **not** by pretending stage1 is optional scaffolding.

## Ruled out as *sole* root cause (unit)

`cross_target_virtual_rdepend_provider_is_target_not_host` shows host
`add_host_installed(libxcrypt)` does not suppress Target libxcrypt in a
minimal dual-root solve. Useful dual-root hygiene; does **not** replace
stage1 for a post-crossdev empty real-Cpn VDB.

## Residual em bugs (still real, orthogonal to “need stage1”)

| # | Issue | Status |
|---|--------|--------|
| 1–2 | RDEPEND blockers / empty-ED buildpkg | fixed `a46027b` |
| 4–5 | baselayout + bashrc die | fixed `f8ac293` |
| 3 as “libxcrypt never scheduled” after **correct** stage1 | may shrink or change shape — re-verify on setup → stage1 → clang | open |

Optional later: map `cross-T/foo` → real Cpn for Favor (risky dual-identity).
Prefer documenting stage1 as required over silent category aliasing.

## Sonnet homework shape

```sh
em --prefix "$P" --target T crossdev --setup --jobs 8
em --prefix "$P" --target T stages --stage1 --autosolve-use --jobs 8
# Confirm sysroot VDB has real-category system pkgs, not only cross-*/
em --prefix "$P" --target T -b llvm-core/clang --jobs 80
```

See [[for-sonnet]] NEXT HOMEWORK.
