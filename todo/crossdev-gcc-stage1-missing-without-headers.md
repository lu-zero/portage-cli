# `has_version` piggybacks `cross-*` atoms off the host, but the consumer only ever looks under `EPREFIX`

Status: ✅ fixed 2026-09-02. `has_version`'s root selection is now scoped by
atom shape: a `cross-<tuple>/*` atom is answered from the prefix's `EROOT`
alone, never from the host `ROOT`, so neither a stale nor a genuine host-side
`cross-*` record can satisfy a check whose only consumer hardcodes an
`EPREFIX`-relative `--with-sysroot`. Ordinary atoms keep the host piggyback
unchanged, which is what lets a host build tool satisfy a `-b` query under
`--prefix`. `select_vdb_roots`/`is_cross_atom` in `version_query.rs`, with
unit tests for all four shapes.

Note that the symptom stopped reproducing on clean sandbox state well before
this (see "Confirmed 2026-08-29"), and that `ece1fcb` fixed the *same symptom
text* for a plain `--prefix` build with no `--target` — an adjacent ESYSROOT
gap. Neither addressed this mechanism; it was latent until now.

Original root cause, fully confirmed 2026-08-29:Original root cause, fully confirmed 2026-08-29: Not
target-specific — exposed by stale sandbox VDB state, but the
underlying mismatch is real and latent regardless: `toolchain.eclass`'s
`has_version ${CATEGORY}/${needed_libc}` check can be satisfied from
the host `ROOT` (deliberate host-tool piggybacking), but every branch
that follows hardcodes `--with-sysroot=${PREFIX}/${CTARGET}` — always
the prefix, never wherever the satisfying match actually lives.
Distinct from [[crossdev-prefix-gcc-header-dir]] (wrong-but-present
header path at gcc-**stage2** on i586) though it may retroactively
explain that report too (see below).

## The failure

`em --prefix P --target riscv64-unknown-linux-gnu crossdev --setup`
fails at `[3/6] gcc-stage1`:

```
libgcc/../gcc/tsystem.h:95:10: fatal error: stdio.h: No such file or directory
```

`stdio.h` doesn't exist anywhere under `P/usr/riscv64-unknown-linux-gnu`
— correctly so, since gcc-stage1 runs *before* headers/libc by design.

## Root cause (confirmed, not speculation)

Real `toolchain.eclass` (`/var/git/gentoo/eclass/toolchain.eclass:1473`)
decides whether to build gcc-stage1 freestanding via:

```
if ! has_version ${CATEGORY}/${needed_libc} ; then
    confgcc+=( ... --disable-threads --without-headers )
```

— here `${CATEGORY}` is `cross-riscv64-unknown-linux-gnu`,
`${needed_libc}` is `glibc`. The full branch (toolchain.eclass:1474-1500):

```
if ! has_version ${CATEGORY}/${needed_libc} ; then
    confgcc+=( ... --disable-threads --without-headers )
elif has_version "${CATEGORY}/${needed_libc}[headers-only(-)]" ; then
    confgcc+=( ... --with-sysroot="${PREFIX}"/${CTARGET#accel-} )
else
    confgcc+=( --with-sysroot="${PREFIX}"/${CTARGET#accel-} )
fi
```

**Both** non-freestanding branches hardcode a single
`--with-sysroot="${PREFIX}"/${CTARGET}` — always the prefix's own path,
never wherever the satisfying `has_version` match actually came from.

`em`'s `has_version` (`portage-repo/src/build/commands/version_query.rs`,
`vdb_roots_for`, lines 16-52) queries `ROOT`'s VDB by default, **plus**
`EROOT`'s when `EPREFIX` is set and differs — documented as deliberate
for `--prefix`'s "overlay on host tools / seed compiler model": a
host-side build should be able to see host-provided packages via
`ROOT` (the bare host, `/`) as well as whatever's already built into
the prefix (`EROOT`).

The crossdev-stages sandbox used for today's test (`em-i586-check`) had
**stale, pre-existing VDB records on its bare host `/`** from unrelated
prior *bare* (non-`--prefix`) crossdev testing:

```
/var/db/pkg/cross-riscv64-unknown-linux-gnu/{binutils,gcc,glibc,linux-headers}  (dated 2026-08-28)
/var/db/pkg/cross-i586-pc-linux-gnu/...                                         (dated 2026-08-27)
```

— both from before this session started (2026-08-29). Today's
`--prefix P --target riscv64-... crossdev --setup` run's `has_version
cross-riscv64-unknown-linux-gnu/glibc` check found that leftover
`glibc-2.43-r4` record on the bare host and concluded "libc already
exists," so `toolchain.eclass` skipped `--without-headers` — even
though the actual `--prefix` build's own sysroot (`P/usr/riscv64-...`)
has no libc at all yet. **Confirmed via diff against a clean target**:
x86_64 was never bare-tested in this sandbox, has no such record, and
correctly got `--without-headers`.

This likely also explains the original 2026-08-26 i586 report
([[crossdev-prefix-gcc-header-dir]]) via the same mechanism, given the
matching stale `cross-i586-pc-linux-gnu` host record — worth
re-checking that report's sandbox history rather than assuming a
different cause.

## This is not a "clean your sandbox" issue — piggybacking is real, but this eclass path can't consume it

`ROOT`-piggybacking is a genuine, correct design for BDEPEND-style
tools (autoconf, bison, pkg-config): under `--prefix`, `ROOT` really is
the literal host filesystem, so a host-installed binary is directly
usable in place — no path mismatch, because the *consumer* (exec'ing
the tool) doesn't care where it physically lives.

This specific `toolchain.eclass` check is different in kind: its
consequence isn't "is a usable thing available," it's "which single
`--with-sysroot` path do I hardcode." Both branches past the
freestanding check point at `${PREFIX}/${CTARGET}` unconditionally.
So even a **genuine, non-stale** host-side `cross-<tuple>/glibc`
record would misfire here: `has_version` reports "satisfied" (correctly,
per the piggyback design), but the only sysroot path the build will
ever actually look under is the prefix's, which — in that same
branch — is assumed to be where the satisfying install lives. That
assumption is true in upstream Gentoo Prefix (one tree, `ROOT` and
`EPREFIX` coincide) but false in `em`'s overlay `--prefix` model, where
they're deliberately two separate, unrelated trees. The stale VDB
record just made an always-latent mismatch visible today.

## Where to fix

Not a general "stop checking `ROOT`" change — that would break the
legitimate BDEPEND-tool piggyback case. Scope it to exactly the shape
that can't be consumed correctly: when `has_version` resolves a
`cross-<tuple>/<pkg>` atom, and the current build has its own
`--target` `EROOT`, prefer/require the match to come from `EROOT`'s
VDB, not `ROOT`'s — a `cross-*` atom's only legitimate consumer here
is code that then acts on `EPREFIX`-relative paths, so a host-side
match for it is exactly the case that can never be safely piggybacked.
Generic (non-`cross-*`) atoms keep checking both, unchanged.

## Confirmed 2026-08-29

Reran riscv64 in a genuinely fresh sandbox (`em-riscv-clean`, no prior
`cross-*` VDB history at all): `--without-headers` correctly present
in gcc-stage1's configure line, all 6 `crossdev --setup --prefix` steps
completed, `EXIT=0`. Trigger mechanism confirmed. This does **not**
mean no fix is needed — the underlying host/prefix mismatch is real
and latent regardless of whether any given run happens to hit it (a
future bare crossdev test against a shared sandbox will reintroduce
exactly this failure for whoever runs `--prefix` against it next).

## The actual fix

Make the `cross-*`-atom `has_version` path in `vdb_roots_for` (or its
caller) EROOT-only when the build has a `--target`, so a host-side
match — genuine or stale — can never satisfy a check whose only
consumer (`toolchain.eclass`'s `--with-sysroot=${PREFIX}/${CTARGET}`)
hardcodes an `EPREFIX`-relative path.
