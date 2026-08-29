# cross stage1 tries `ABI_X86=32` against a non-multilib cross toolchain

Status: 🔴 found 2026-08-29 live-verifying
[[crossdev-root-prefix-target-toolchain-anchor]]'s fix (real, non-`-p`
`em --prefix P --root B --target x86_64-unknown-linux-gnu stages
--stage1`, aarch64 host). Orthogonal to that fix — the root/prefix
split itself worked (84/142 packages built correctly split between
`B` and `P/usr/T`); this is a separate, real bug.

## The failure

`sys-apps/sandbox-2.49`'s `abi_x86_32.x86` multilib-minimal configure
pass fails:

```
checking for x86_64-unknown-linux-gnu-gcc... /root/prefix-p/usr/bin/x86_64-unknown-linux-gnu-gcc -m32 -mfpmath=sse
checking whether the C compiler works... no
configure: error: C compiler cannot create executables
```

The cross toolchain's `glibc` was built with `USE="... (-multilib)
-multilib-bootstrap ..."` — multilib was never enabled, so there is no
32-bit libc/crt in the sysroot for `-m32` to link against. Yet stage1
picked the (default `default/linux/amd64/23.0`, multilib) profile and
tried `ABI_X86="32 64"` anyway.

## Where to look

Stage1's `packages.build` USE/ABI derivation should either force
`ABI_X86` down to the toolchain's actual built ABIs, or `crossdev
--setup` should build the toolchain multilib-enabled when the picked
profile expects it. Needs a decision on which side owns the fix.
