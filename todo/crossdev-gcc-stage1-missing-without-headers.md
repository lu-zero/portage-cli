# gcc-stage1 missing `--without-headers` for some targets (riscv64)

Status: 🔴 found 2026-08-29, root-caused to a specific missing configure
flag, not fixed. Pre-existing — confirmed against the pre-session
commit (`430cf11`), unrelated to this session's `--root`/`--prefix`
work. Distinct from [[crossdev-prefix-gcc-header-dir]] (that one is a
wrong-but-present header path at gcc-**stage2** on i586; this one is
headers genuinely absent — expected at gcc-stage1, before headers/libc
— but gcc still tries to use them).

## The failure

`em --prefix P --target riscv64-unknown-linux-gnu crossdev --setup`
fails at `[3/6] gcc-stage1`:

```
libgcc/../gcc/tsystem.h:95:10: fatal error: stdio.h: No such file or directory
```

`stdio.h` doesn't exist anywhere under `P/usr/riscv64-unknown-linux-gnu`
— correctly so, since gcc-stage1 runs *before* headers/libc
(`stages.rs`'s own doc comment: "gcc-stage1 (freestanding, no libc/headers
needed)"). Confirmed via the pre-session binary (commit `430cf11`): the
identical build fails identically — not introduced by anything landed
today.

## Root cause

Diffing the two targets' actual `configure` invocations from
`build.log`:

- **x86_64-unknown-linux-gnu** (gcc-stage1, succeeds): `...
  --disable-shared --disable-libquadmath --disable-libatomic
  --disable-threads --without-headers ...`
- **riscv64-unknown-linux-gnu** (gcc-stage1, fails): none of those five
  flags present at all.

`--without-headers` is what makes gcc-stage1 build freestanding; its
absence means gcc's build assumes a full sysroot and tries to compile
`libgcc` bits that `#include <stdio.h>`. Whatever decides this flag set
(real `toolchain.eclass`'s own auto-detection, or an em-side stage1 USE
override — see `crossdev/stages.rs`'s `STAGE1_GCC_USE`-equivalent list)
computes it correctly for x86_64 but not for riscv64.

## Where to look

`portage-cli/src/crossdev/stages.rs` — the gcc-stage1 USE/config
derivation (search for where `USE="-cxx -fortran -openmp ..."` gets
built for the stage1 step, and whatever signal it uses to decide
freestanding-ness). Compare what differs between the x86_64 and
riscv64 code paths — likely something keyed off `elibc`/multilib/ABI
detection (real toolchain.eclass computes `--without-headers` based on
whether target glibc is already merged, via `has_version` against the
target's own category — worth checking whether that check is somehow
riscv64-specific, e.g. an ABI/multilib table lookup
(`crossdev/multilib.rs`) that returns something unexpected for
`riscv64-unknown-linux-gnu`'s `rv64`/`lp64d` ABI and short-circuits the
freestanding-detection logic before it runs).

## How to attack

1. Reproduce: `em --prefix P --target riscv64-unknown-linux-gnu crossdev --setup` in a fresh crossdev-stages sandbox (real, aarch64 host — reproduced there 2026-08-29).
2. Compare the gcc-stage1 USE/configure derivation code path for a
   working target (x86_64) vs riscv64 — find exactly which condition
   diverges.
3. Also worth checking i586 (from [[crossdev-prefix-gcc-header-dir]])
   against this same angle — could turn out to be the same root cause
   surfacing at a different step, or a genuinely separate bug; don't
   assume either way without checking.
