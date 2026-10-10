# Compiler wrapper links outlive a rebuild that drops their tool

Status: ✅ fixed 2026-10-10 in `em`'s own activation. `gcc-config` itself
has the same blind spot; not reported upstream.

## What happened

`em toolchain --setup` builds gcc with its default USE, fortran
included, and `em`'s activation links everything in `gcc-bin/<slot>`
into `usr/bin`. `em stages --stage1` then rebuilds the same gcc version
with `USE="-* build …"`, without fortran. `usr/bin/gfortran` and
`usr/bin/<CHOST>-gfortran` stayed, pointing at nothing.

gcc's `pkg_postinst` runs the real `gcc-config`, which removes "the
wrappers that the old profile provided but the new ones do not". It
gets both lists by listing the profile's `gcc-bin` directory. For a
rebuild at the same version that is one directory, already replaced by
the merge when `pkg_postinst` runs, so the lists are equal and nothing
is removed. The same would happen under emerge; catalyst does not hit
it because it builds gcc into a stage1 root once.

Upstream: no report found for the same-version case (searched
2026-10-10). The commit that introduced the old-profile comparison
(2012) notes that the list is lost if the old profile is gone before
the switch, and the test added with it covers switching between two
profiles, one with fortran and one without.

## Fix

`select/compiler.rs::prune_stale_wrappers`: before linking, remove the
links in `usr/bin` that point into this `gcc-bin` directory and whose
tool is not there. It recognises the three forms such a link has had:
relative, absolute inside the root (`gcc-config`), and with the root's
path in front. `em stages --stage1` re-activates the native compiler
when it is done; a `--target` stage's compiler is left alone.

Test: three stale links of the three forms go, an unrelated dangling
link and a live one stay.

Live, sandbox `em-stage-slash`: stage1 re-run on the package-installed
root takes the fortran links from 2 to 0; the only broken link left in
the root is `etc/mtab -> ../proc/self/mounts`, and `gcc --version` and
`ld --version` run inside it under `chroot`.
