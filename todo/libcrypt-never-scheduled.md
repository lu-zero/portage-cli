# Library DEPEND vs @system after `crossdev --setup`

Status: 🟡 reframed **2026-08-07** (not “must always stage1”)  
Live: Sonnet — pam/libcrypt under setup-only clang world  
Related: [[for-sonnet]], [`docs/crossdev.md`](../docs/user/crossdev.md)

## Symptom (setup-only → clang)

- Plan lists `virtual/libcrypt` + `sys-libs/libxcrypt`; virtual Completes;
  libxcrypt never Emerges; pam sees no libcrypt in **sysroot**.
- Underlying shape: **library DEPEND** wants real-Cpn libc; toolchain VDB
  has **`cross-T/glibc`** (or musl), not `sys-libs/glibc` (or musl).

## Two different problems

| Kind | What it means in cross | Full @system needed? |
|------|------------------------|----------------------|
| **Library DEPEND** | Headers / libs to **compile and link** against (`ESYSROOT`) | **No** — need those libs **present and Favor/provided under the Cpn the ebuild names** (profile libc: glibc *or* musl, headers, …). Files often already exist from `crossdev --setup`. |
| **Runtime RDEPEND** | What the package needs to **run** on the target | **Not fully** for a pure cross-build env; a complete target userland later may want stage1/`@system`/stage3. |
| **BDEPEND** | Tools on the build host | Host/prefix dual-root, not sysroot @system. |

So: **@system is not fully needed for a cross environment** when the goal is
cross-compiling into a sysroot. The pain we hit is **library dependency
identity**, not “missing entire @system.”

## What `crossdev --setup` actually gives

- **Files:** toolchain, kernel headers, libc (glibc *or* musl per tuple) under
  the sysroot tree.
- **VDB identity (em today):** `cross-<tuple>/glibc` (etc.), not
  `sys-libs/glibc`.

Ordinary ebuilds Depend on **real** Cpns (`sys-libs/glibc`, `sys-libs/musl`,
`virtual/libcrypt` → libxcrypt with `elibc_*` gates). Favor does not treat
`cross-T/glibc` as `sys-libs/glibc` → plan invents a second libc / stalls
providers (libxcrypt never scheduled class).

## bash-crossdev analogy

1. **Toolchain:** `emerge cross-T/…` (files into `/usr/T`).  
2. **Ordinary packages:** `CHOST-emerge` with `ROOT=SYSROOT` and **real** atoms
   (`sys-libs/zlib`). Wiki “base after crossdev” re-installs baselayout +
   **real** libc into ROOT — same *library identity* role, not full @system
   for every cross package.

Improvements for em (library side, not full @system):

| Approach | Idea |
|----------|------|
| **Alias Favor** | Installed `cross-T/foo` satisfies `real_cpn(foo)` for target DEPEND |
| **package.provided** | After setup, provide real libc/headers (files already there) |
| **Real-Cpn VDB** | Register toolchain merges under real Cpns in the sysroot |
| **stage1** | Still valid for a richer/bootstrap userland; **heavier** than needed if only libc/headers identity is broken |

## pam / clang chain (still real)

```text
clang → python → util-linux → USE=pam → pam → virtual/libcrypt → libxcrypt
```

That pulls **library** needs (libcrypt) into the plan. Fix library Favor for
libc first; do not require full @system just because pam appears.

## Confirm matrix

| Probe | What it tells us |
|-------|------------------|
| Setup-only: sysroot VDB `cross-T/glibc` vs `sys-libs/glibc` (or musl) | Identity gap |
| Setup-only: `em -p --target T sys-libs/zlib` plans full real libc? | DEPEND not satisfied by cross-T |
| After alias/provided/real-Cpn only: zlib/clang plan size | Library fix without stage1 |
| After stage1: same | stage1 as optional richer base |

## Unit note

`cross_target_virtual_rdepend_provider_is_target_not_host` rules out “host
libxcrypt Favor kills Target provider” in a minimal dual-root model. It does
not fix cross-T vs real-Cpn libc in the sysroot VDB.
