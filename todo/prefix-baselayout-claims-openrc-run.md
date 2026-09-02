# Under `--prefix`, baselayout's VDB entry claims `/sbin/openrc-run`

Status: 🔴 not started, found 2026-09-02 by `regression-matrix.sh --full`.
Fails `toolchain --setup --prefix` at 54/65 packages.

## Symptom

```
>>> Installing (54 of 65) sys-apps/openrc-0.63.3 to /root/regress-toolchain-prefix/
!!! collision: /sbin/openrc-run is already owned by sys-apps/baselayout-2.18-r1
      1 file collision(s) detected — aborting merge
    (53 ok, 0 already installed, 1 failed of 65)
```

The collision check is doing its job — the problem is that baselayout should
never have owned that path.

## Why baselayout owning it is wrong

On the host, with real portage:

```
$ qfile /sbin/openrc-run
sys-apps/openrc: /sbin/openrc-run
$ grep -l openrc-run /var/db/pkg/sys-apps/baselayout-*/CONTENTS
(no match)
```

`sys-apps/openrc` owns it; baselayout does not, and does not ship it.

In the failing prefix it is recorded against baselayout as a real 68 KB
executable — openrc's own binary, not a symlink or a stub:

```
$ grep openrc-run P/var/db/pkg/sys-apps/baselayout-2.18-r1/CONTENTS
obj /sbin/openrc-run 11959bead1ecd90b9bc3f83648b80e41 1788349948
$ ls -l P/sbin/openrc-run
-rwxr-xr-x 1 … 68160 … P/sbin/openrc-run
```

So baselayout's recorded contents picked up a file belonging to a package
merged later in the same run.

## Scope

`--prefix` only. The `--root` toolchain run in the same matrix did not hit
it (it failed separately and transiently on `dev-lang/perl`), and `crossdev
--setup` bare merges baselayout into a sysroot without incident.

Note that `em setup`'s `merge_baselayout` installs baselayout with
`USE=build` into the **outer** EROOT, which under `--prefix` is a different
tree from where the toolchain packages then land — that asymmetry is the
first thing to look at. Whether the wrong file lands in baselayout's
`${D}`/image, or is only mis-attributed when `CONTENTS` is walked, is not
yet established; check the image directory before assuming the VDB write is
at fault.

## Not caused by this session's VDB change

`3b67885` changed only *where* a VDB entry is written before publication
(`-MERGING-` staging plus a rename), never which files an entry lists. The
same commit's live verification merged `sys-apps/gentoo-functions` twice
with a complete, correct 31-field entry. Confirm by reproducing against a
binary from before it if this is ever in doubt.

## Reproduction

```sh
./test-scripts/regression-matrix.sh --full     # toolchain --setup --prefix leg
```
