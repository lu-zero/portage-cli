# Toolchain links in a `--root` stage point into the path it was built at

Status: ✅ fixed 2026-10-10: no link in a stage root names the root's
path. The 34 that `em` wrote are relative now; the other 37 went with
[[root-variables-trailing-slash]].

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

## Fixed (2026-10-10): the links `em` writes

`select/env_d.rs::symlink_relative` writes a target relative to its
link; `install_binutils_wrappers` and `install_gcc_wrappers` use it.
`usr/<T>/bin/ld -> ../binutils-bin/2.47/ld`,
`usr/bin/<T>-ld -> ../<T>/bin/ld`. Not `binutils-config`'s absolute
`EPREFIX` path, on purpose: `em` runs these tools through the links from
outside a `--root` while it builds it, and an absolute target without
the root would then be looked up on the host.

Tests: the cross layout's exact targets; a native root renamed after
the links are written still reaches `ld` through both.

Live, sandbox `em-stage-final`: the root reinstalled from packages has
37 links naming its path, down from 71, and `usr/bin/ld` resolves to an
executable inside the root from outside it.

## The other 37 are perl's, and the cause is not in `select`

`usr/bin/{ptar,cpan,json_pp,…}` and their man pages point at
`/root/s2//usr/bin/ptar-3.120.0-perl-5.44.0` and the like. perl's
`pkg_postinst` makes them through `alternatives.eclass`, which writes a
relative link when the alternative is in the link's directory and an
absolute, `ROOT`-prefixed one otherwise. Here it takes the second
branch although they are in the same directory. Traced to two
deviations that only bite together, see
[[root-variables-trailing-slash]].
