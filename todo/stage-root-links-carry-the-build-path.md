# Toolchain links in a `--root` stage point into the path it was built at

Status: 🔴 found 2026-10-09 by comparing the stage roots of
[[binpkg-stage-test-defects]]; not fixed. Predates that work: the first
sandbox's source-built root has the same 71 links.

## What is there

`em --root /root/s1 toolchain --setup` leaves, in the root:

```
usr/aarch64-unknown-linux-gnu/bin/ld -> /root/s1/usr/aarch64-unknown-linux-gnu/binutils-bin/2.47/ld
usr/bin/aarch64-unknown-linux-gnu-ld -> /root/s1/usr/aarch64-unknown-linux-gnu/bin/ld
usr/bin/ld -> aarch64-unknown-linux-gnu-ld
```

71 symlinks whose target starts with the build-time root: 16 in
`usr/<CHOST>/bin`, 36 in `usr/bin`, 19 in `usr/share/man/man1`. Used
from the host at that path they work. Entered as `/`, or unpacked from a
tarball anywhere else, they dangle: no `ld`.

The gcc links next to them are right (`usr/bin/gcc ->
/usr/<CHOST>/gcc-bin/16/gcc`).

## Where

`select/binutils.rs::install_binutils_wrappers` links
`<eprefix>/usr/<T>/bin/<tool>` to `<eprefix>/…/binutils-bin/<ver>/<tool>`
with the full path, where `eprefix` is the merge root. That is right
for a prefix used in place (`--local`, `--prefix`) and wrong for a
`--root`, where the packages are configured for `/`. The man page links
have not been traced.

## Direction

Relative targets (`../binutils-bin/2.47/ld`, `../<T>/bin/ld`) are right
under either placement and need no knowledge of which one it is. Real
`binutils-config` writes targets without `ROOT`.

Check the cross layout too (`usr/<CBUILD>/<T>/binutils-bin`, links via
`usr/libexec/gcc/<T>`), and what `gcc-config` versus
`select/compiler.rs` writes: the gcc links are correct, by which of the
two is not established.
