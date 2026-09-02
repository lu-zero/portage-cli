# Package-level profile rules (package.use.force/mask/…) don't follow the cross-alias mapping

Status: 🔴 not fixed — re-confirmed live 2026-09-02. Originally filed as an
ABI_X86 stage1 mismatch; root-caused while investigating why.

One detail below is now stale: `real_cpn_of` **is** read back these days, at
`query/depgraph/mod.rs:1969` and in `crossdev/target.rs` — but only to
redirect the *ebuild file path* to the real package. `force_mask` still
never sees it (`PkgRules` is a plain `HashMap<Cpn, …>`, and the alias
injection at `repo.rs:1465` populates `real_cpn_of`/`cpns_set` without
duplicating any profile rule under the cross `Cpn`), so the bug itself is
untouched.

## Symptom (original finding)

Live-verifying [[crossdev-root-prefix-target-toolchain-anchor]]'s fix
(real, non-`-p` `em --prefix P --root B --target x86_64-unknown-linux-gnu
stages --stage1`, aarch64 host), `sys-apps/sandbox-2.49`'s
`abi_x86_32.x86` multilib-minimal configure pass failed:

```
checking for x86_64-unknown-linux-gnu-gcc... /root/prefix-p/usr/bin/x86_64-unknown-linux-gnu-gcc -m32 -mfpmath=sse
checking whether the C compiler works... no
configure: error: C compiler cannot create executables
```

The cross toolchain's `glibc` was built `USE="... (-multilib)
-multilib-bootstrap ..."` — no 32-bit libc/crt in the sysroot — while
stage1's board build (same `default/linux/amd64/23.0` profile) assumes
`ABI_X86="32 64"` is available.

## Root cause

Real Gentoo's `profiles/features/multilib/package.use.force` forces
`sys-libs/glibc multilib` and `sys-devel/gcc multilib` unconditionally
— this is what makes multilib work at all on amd64 (the base profile's
`profiles/base/use.mask` masks the bare `multilib` flag globally;
`features/multilib/use.mask` unmasks it, and this package-level force
turns it on for glibc/gcc specifically).

`em` tracks the crossdev alias mapping (`cross-x86_64-unknown-linux-gnu/glibc`
→ `sys-libs/glibc`) in `real_cpn_of` (`portage-resolve/src/repo.rs:761`,
populated at `load_repos`, ~line 1465) — but **nothing in the codebase
ever reads `real_cpn_of` back**. Package-level profile rules
(`package.use`, `package.use.force`, `package.use.mask`,
`package.use.stable.{force,mask}`, likely `package.mask` too) are
indexed by literal `Cpn` (`force_mask::index_by_cpn`, keyed off the
atom's own category/package as written in the profile file — e.g.
`sys-libs/glibc`) and looked up at resolve time by the package's own
`Cpv` — which for a crossdev-aliased package is
`cross-x86_64-unknown-linux-gnu/glibc`, not `sys-libs/glibc`. The two
never match, so **every package-level profile rule silently no-ops for
any `cross-*` aliased package**, while it applies correctly for the
same real package.

**Confirmed live**, not just by reading code: comparing the identical
`glibc-2.43-r4` build's USE line under the cross alias (`crossdev
--setup`) against the plain package (`stages --stage1`'s board build,
same profile) — the board build correctly shows profile-forced flags
in parens (`(static-libs)` ← `base/package.use.force`, `(-vanilla)` ←
`base/package.use.mask`, `(-custom-cflags)`, `(-clang)`), while the
cross build shows the *same* flags unparenthesized (plain `static-libs`,
`-vanilla`, `-custom-cflags`, `-clang` — off by ebuild-IUSE-default
alone, not forced/masked at all). `multilib` follows the same pattern:
the global mask/unmask nets out the same either way, but the
package-level *force* that would flip it on for glibc/gcc never
reaches the cross package, so it stays at its (global, unforced)
default.

## Where to fix

`portage-resolve/src/repo.rs` — either populate `force_mask`'s
`PkgRules` index with both the cross `Cpn` and the real `Cpn` for every
aliased package (duplicate the entry at alias-injection time,
~line 1465, alongside where `real_cpn_of` is populated), or have the
`ForceMask::apply`/`effective` call sites (`repo.rs` ~line 1219)
consult `real_cpn_of` and look up under the real `Cpn` when the
package's own `Cpn` has no direct match. The former is probably
simpler and keeps `ForceMask` itself alias-unaware.

## Re-confirmed live 2026-09-02

Same method as the original, on the host's own `::crossdev` repo — no
sandbox needed, `em -pv` is enough:

```
sys-libs/glibc                          USE="(static-libs) (-cet) (-clang) (-custom-cflags) (-multilib) (-selinux)"
cross-riscv64-unknown-linux-gnu/glibc   USE="static-libs  (-cet)  -clang   -custom-cflags  (-multilib) (-selinux)"
```

`static-libs`/`-clang`/`-custom-cflags` lose their parentheses under the
alias — the package-level `package.use.force`/`package.use.mask` entries
never matched. `(-cet)`/`(-multilib)`/`(-selinux)` keep theirs in both,
because those come from *global* `use.mask`, which is alias-independent —
exactly the split this note predicted.

That two-command reproduction is also the regression test to write.

## Scope

This is a resolve-engine bug, not scoped to crossdev or to `multilib`
specifically — any `package.use`/`package.mask`/`package.use.force`/
`package.use.mask` entry that should apply to a real package silently
doesn't apply to its cross-* alias today.
