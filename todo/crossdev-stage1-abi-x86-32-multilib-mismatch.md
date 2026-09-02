# ABI_X86 stage1 multilib mismatch — the resolve-engine theory was wrong

Status: 🟡 **not a resolve bug — closed that line of investigation
2026-09-02.** The original ABI_X86 symptom (below) is real and still
unexplained; the "package-level profile rules don't follow the cross-alias
mapping" root cause this note carried for a week is **false**, disproven by
direct comparison against real emerge.

## What was wrong with the theory

The note claimed that because `force_mask`'s `PkgRules` is keyed by literal
`Cpn`, profile rules written for `sys-libs/glibc` silently no-op for
`cross-<tuple>/glibc`, and that this was an `em` defect to fix by
duplicating each rule under both keys.

The first half is accurate. The conclusion is not: **real portage does
exactly the same thing**, so `em` was already correct and the proposed fix
would have made it diverge.

On this host, `::crossdev` present, portage 3.0.81.2:

```
$ emerge -pv cross-riscv64-unknown-linux-gnu/glibc
USE="multiarch ssp static-libs … (-cet) -clang … (-multilib) … (-selinux) … -vanilla …"

$ em -pv cross-riscv64-unknown-linux-gnu/glibc
USE="multiarch ssp static-libs … (-cet) -clang … (-multilib) … (-selinux) … -vanilla …"
```

Byte-for-byte identical. Both show `static-libs`/`-clang`/`-custom-cflags`
*unparenthesised* under the alias while `sys-libs/glibc` shows them
parenthesised, and both keep `(-cet)`/`(-multilib)`/`(-selinux)` in either
form. Package-level rules genuinely do not follow a crossdev alias in
portage either — a `cross-*` package is a distinct package name, and
`profiles/features/multilib/package.use.force`'s `sys-libs/glibc` atom does
not match it.

The earlier "confirmed live" evidence measured cross-vs-non-cross and never
compared against emerge, which is the only comparison that could have told
a defect from correct parity.

## What is still open

The original symptom stands: stage1's board build assumed `ABI_X86="32 64"`
while the cross toolchain's glibc had no 32-bit libc/crt. Since portage does
not force `multilib` onto a `cross-*` glibc either, whatever makes this work
in a real crossdev setup is *not* profile-rule aliasing — look at how
crossdev itself configures the target's USE (its own `package.use` under the
cross category, or the target profile), and at what `em`'s stage1 assumes
about the board profile's ABI list. Re-file under a fresh root cause when
one is found rather than reviving this one.

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

## The original (disproven) root-cause writeup, kept for the record

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

## The measurement that looked like confirmation

Same method as the original — but against `em` alone, which is the flaw:

```
sys-libs/glibc                          USE="(static-libs) (-cet) (-clang) (-custom-cflags) (-multilib) (-selinux)"
cross-riscv64-unknown-linux-gnu/glibc   USE="static-libs  (-cet)  -clang   -custom-cflags  (-multilib) (-selinux)"
```

`static-libs`/`-clang`/`-custom-cflags` lose their parentheses under the
alias — the package-level `package.use.force`/`package.use.mask` entries
never matched. `(-cet)`/`(-multilib)`/`(-selinux)` keep theirs in both,
because those come from *global* `use.mask`, which is alias-independent —
exactly the split this note predicted.

Running the same two commands against real `emerge` is what showed this to
be parity rather than a defect.

## Scope

This is a resolve-engine bug, not scoped to crossdev or to `multilib`
specifically — any `package.use`/`package.mask`/`package.use.force`/
`package.use.mask` entry that should apply to a real package silently
doesn't apply to its cross-* alias today.
