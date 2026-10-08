# Experiment: a riscv64 stage1 with clang as the system compiler, gcc only for glibc

Status: ✅ reached 2026-10-07/08, by hand-written config around an unmodified
`em` (master `5e1098df` plus `--print-spec`), in crossdev-stages sandbox
`em-riscv-clang-glibc` (arm64 build host). Only `-p` plans and real merges
were run; nothing in the stage was executed (no riscv64 emulator), and no
stage3 was attempted. Feeds [[cross-sysroot-spec]] step 4.

## Result

`/root/stage` in the sandbox: 106 packages, 849 MB.

- No `sys-devel/gcc` and no `sys-devel/binutils` installed in it.
- `llvm`, `lld`, `clang` 23.1.3 cross-built for riscv64, plus `compiler-rt`,
  `libunwind`, `libcxxabi`, `libcxx` and the clang config packages
  (`--rtlib=compiler-rt`, `--stdlib=libc++`).
- `sys-libs/glibc-2.44-r3` built by gcc: 6584 `riscv64-…-gcc` invocations
  in its build log, 0 clang — for both the sysroot and the stage copy.
- Everything else by clang (e.g. bash: 162 clang invocations).
- Of 927 dynamic ELF objects in the stage, **0 link `libstdc++` or
  `libgcc_s`**; 121 link `libc++`/`libunwind`. `clang` itself needs
  `libc++.so.1`, `libc++abi.so.1`, `libunwind.so.1`, `libc.so.6`.

Deviation from a pure `em stages --stage1`: the last three packages
(`libcxx`, `clang-stdlib-config`, `clang-runtime`) were merged directly
with `--nodeps` after the stage1 run stopped on `libcxx`.

## Timings on this host (128 cores, shared)

- GCC cross toolchain (`em crossdev --setup`): 11.5 min.
- Host llvm+lld+clang 23 from source, `--jobs 1`: 74 min, under load ~250
  for the first half.
- The four cross LLVM runtimes: about 1 min.
- stage1 from source, `--jobs 3`: 20 min for 107 packages, including LLVM
  and clang cross-built for riscv64.

## Recipe

```sh
T=riscv64-unknown-linux-gnu; C=cross-$T
# 1. GCC cross toolchain
em -T $T crossdev --setup --autounmask-write --jobs 4
# 2. host clang + the <tuple>-clang wrappers, and the runtimes as extras
em -T $T crossdev --init-target --ex-pkg sys-devel/clang-crossdev-wrappers \
   --ex-pkg llvm-runtimes/compiler-rt --ex-pkg llvm-runtimes/libunwind \
   --ex-pkg llvm-runtimes/libcxxabi --ex-pkg llvm-runtimes/libcxx
em --autounmask-write --jobs 1 $C/clang-crossdev-wrappers
# 3. by hand: /etc/portage/env/$C/llvm.conf (CC=$T-clang … as -L writes it),
#    appended to the four runtimes' package.env lines; and
#    /etc/clang/cross/$T.cfg, first with --gcc-install-dir, then:
#      --target=$T  --sysroot=/usr/$T  --rtlib=compiler-rt
#      --unwindlib=libunwind  --stdlib=libc++  -fuse-ld=lld
for p in compiler-rt libunwind libcxxabi libcxx; do
  em --nodeps --jobs 1 "=$C/$p-23*"; done
# 4. sysroot config, by hand, in /usr/$T/etc/portage:
#    make.profile/parent = profiles/default/linux/riscv/23.0/rv64/lp64d
#                        + profiles/features/llvm      (no riscv llvm profile exists)
#    make.conf:   CC="$T-clang" CXX="$T-clang++" CPP="$T-clang-cpp"
#    package.env: sys-libs/glibc gcc-toolchain.conf   (CC=$T-gcc, LD=$T-ld, …)
#    package.use: llvm-runtimes/{libcxx,libcxxabi,libunwind} static-libs
# 5. clang config packages into the sysroot, then the stage
em --target $T --nodeps llvm-runtimes/clang-rtlib-config \
   llvm-runtimes/clang-unwindlib-config llvm-core/clang-linker-config
em --target $T --root /root/stage stages --stage1 --autosolve-use --jobs 3
```

## What `em` should have done itself (inputs to the spec work)

1. **Write `/etc/clang/cross/<tuple>.cfg` whenever a clang toolchain is
   defined**, not only under `-L`. `--ex-pkg clang-crossdev-wrappers` on a
   GCC-model target installs wrappers that cannot run.
2. **Give the runtime extras the clang env and the LLVM slot.** As
   `--ex-pkg` they got host-gcc env and keyword bounds up to the next
   major (`<…/libunwind-24.0.9999`), with clang 23 on the host.
3. **Order a mixed bootstrap**: gcc → glibc → compiler-rt → libunwind →
   libcxxabi → libcxx, then switch the cfg to the LLVM runtimes. Done by
   hand here; this is `built-by` in the spec.
4. **Compose the profile.** No LLVM profile exists for riscv; the sysroot
   needs `make.profile/parent` listing the arch profile and
   `features/llvm`. A profile `CC=clang` is unprefixed and must be
   overridden with the tuple's wrapper for a cross build.
5. **`static-libs` on the runtimes** is set by the LLVM profile's
   `package.use` but lost under stage1's `USE="-* build"`, and
   `--autosolve-use` did not resolve it.
6. **Who owns the sysroot's runtimes.** The cross-category runtimes
   (host VDB) and the target packages `llvm-runtimes/*` (sysroot VDB)
   install the same files into `/usr/<tuple>`. No collision was reported,
   but they disagree: the sysroot ended up without `libunwind.a`, which
   broke a target `openmp` build (`-l:libunwind.a` not found).

## Bugs found on the way, not specific to clang

- **Soname symlink race** in the post-merge ld cache refresh — fixed in
  `ldconfig` 0.2.0, see [[ld-cache-refresh-symlink-race]].
- **Binary packages are created after `pkg_preinst`** (fixed 2026-10-08,
  see [[phase-order-and-binpkg]]).
  `ebuild/mod.rs` packs "after qmerge". baselayout's `pkg_preinst` runs a
  Makefile from the image and deletes it, so its binpkg has no Makefile
  and installing from it fails (`No rule to make target
  'layout-usrmerge'`). Portage packs the image as `src_install` left it.
- **`em maint binhost` corrupts entries with blank lines in a value.**
  sed/grep/tar have an empty line inside `DEPEND`; the regenerated
  `Packages` splits their stanza and loses `PATH`, and the merge then
  tries to untar the package directory. Unknown whether the index written
  during a normal build has the same problem.
- **A stale `Packages` index is trusted**: with a binpkg file deleted but
  still listed, `-k` selects it and fails instead of falling back to
  source.

## Upstream ebuild issues (cross builds outside the crossdev category)

- `llvm-runtimes/libcxxabi` (and libunwind, libcxx) read
  `${ESYSROOT}/etc/clang/<N>/gentoo-{rtlib,unwindlib,linker}.cfg` but list
  the packages providing them under `BDEPEND`. Worked around by merging
  the three config packages into the sysroot.
- `llvm-runtimes/libcxx` passes `LIBCXX_CXX_ABI_INCLUDE_PATHS` as
  `${EPREFIX}/usr/include/c++/v1`, the build host's path. Worked around by
  copying `cxxabi.h` and `__cxxabi_config.h` there.

## Not done

- Running anything from the stage (qemu-user or a board).
- `--stage3`, and a full `@system` under the composed profile.
- The same on musl with `-L` only, for comparison.
- Checking whether a clean second `em stages --stage1` on a populated root
  works; the earlier claim that it does not was a misdiagnosis of the
  binpkg bug above.
