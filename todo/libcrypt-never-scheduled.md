# Bug #3 — `virtual/libcrypt` completes, `sys-libs/libxcrypt` never starts

Status: 🟡 investigating (2026-08-07)  
Live: Sonnet `a46027b` re-verify — pam configure: no libcrypt in sysroot  
Handoff: [[for-sonnet]] NEXT HOMEWORK `f8ac293`

## Symptom

- Plan lists both `virtual/libcrypt-…` and `sys-libs/libxcrypt-4.5.2` into the
  **sysroot** (`…/usr/riscv64-…/`).
- Virtual reports `>>> Completed` early.
- **No** `Emerging` line for libxcrypt in the whole run.
- After EXIT=1: no VDB, no `crypt.h` / `libcrypt.so*` in sysroot.
- pam fails meson “libcrypt / libxcrypt found: NO”.

## Why pam is in the clang plan (not a bug)

```text
llvm-core/clang → ${PYTHON_DEPS} → dev-lang/python
  → sys-apps/util-linux (unconditional)
  → USE=pam (default/linux make.defaults)
  → sys-libs/pam → virtual/libcrypt → sys-libs/libxcrypt
```

Empty sysroot ⇒ full target closure. Stage1 is optional scaffolding, not the
missing prerequisite for this failure class.

## Ruled out (unit)

`portage-atom-pubgrub` test
`cross_target_virtual_rdepend_provider_is_target_not_host`:

- Cross + foreign-arch + host already has libxcrypt (and virtual) as
  `add_host_installed`.
- Solve `sys-libs/pam` still selects **Target** `libxcrypt`.
- Install order: libxcrypt → virtual → pam.
- `dependency_graph` has Target RDEPEND edge virtual → libxcrypt.

So “host Favor kills the Target provider” is **not** the minimal dual-root
story.

## Strong hypotheses (live evidence needed)

### A. Cross toolchain VDB category ≠ ordinary target Cpn (likely)

`crossdev --setup` plan entries use **virtual** CPVs
`cross-<tuple>/glibc` (registered under that category in the sysroot VDB —
see `PlannedMerge.cpv` / `real_cpn_of` only redirects **ebuild path**).

`sys-libs/libxcrypt` with forced `USE=system` has:

```text
DEPEND=" system? ( elibc_glibc? (
  ${CATEGORY}/glibc[-crypt(-)]
  !${CATEGORY}/glibc[crypt(-)]
) ) "
```

For a normal target emerge, `CATEGORY=sys-libs` → needs **`sys-libs/glibc`**.
Installed **`cross-T/glibc`** is a **different Cpn** and does not Favor-
satisfy that edge.

Consequence: clang world may plan a **second** full `sys-libs/glibc` into the
sysroot. libxcrypt’s DEPEND waits on it. If that glibc is late / blocked /
never started before pam fails, libxcrypt never `Emerging`s.

**If virtual does not wait on libxcrypt** (missing edge or soft order without
blockers), virtual can complete empty while the provider is still stuck behind
the second glibc — matches Sonnet’s “virtual done, provider never started.”

`a46027b` already adds RDEPEND to `build_blockers`; if the edge is present,
virtual should wait. Live re-check on `f8ac293` still required.

### B. Silent skip

Merge skip only if `merge_root/var/db/pkg/<cpv>` exists and `!reinstall`.
For Target, that is the **sysroot** VDB (not host). Unlikely for a fresh
sysroot unless something wrote libxcrypt without Emerging (should not).

### C. USE-empty virtual RDEPEND

Virtual RDEPEND is gated on `!prefix-guest` + `elibc_glibc`. If those USE
bits are wrong for the Target fold, virtual is empty and does not pull
libxcrypt — but then libxcrypt should not appear in the plan from that edge.
Contradicts “both in plan dump” unless another package pulls libxcrypt.

## What Sonnet should capture

See [[for-sonnet]] homework: sysroot VDB tree after `crossdev --setup` and
after clang fail (`cross-*/glibc` vs `sys-libs/glibc` vs `libxcrypt`);
`rg` for those names in the log; whether `sys-libs/glibc` is in the N/M plan.

## Possible fix directions (after live confirm)

1. **Register cross toolchain packages under real Cpn in the sysroot VDB**
   (or alias installed cross-T/glibc as sys-libs/glibc for satisfaction), so
   ordinary target packages see the libc that `--setup` already built.
2. Or **map DEPEND satisfaction** for target packages through `real_cpn_of`
   reverse (installed cross Cpn satisfies real Cpn) — careful with
   dual-identity.
3. Ensure `build_blockers` always wait on RDEPEND providers for category
   `virtual/*` even if install_order soft-cycles (belt and suspenders).

Do **not** “fix” by removing pam from the clang plan; pam is legitimate.
