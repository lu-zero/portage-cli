# `dev-python/flit-core` prefix-doubling under `--local` (new, unfixed)

Status: 🔴 not started, found incidentally, root cause not yet
investigated. Found 2026-08-26 continuing
[[crossdev-local-perl-module-eprefix]]'s live `--local` + `--target`
`crossdev --setup` run (with the perl-module eclass patch applied,
which got the run 36 packages further than before).

## The bug

`dev-python/flit-core-3.12.0`'s `gpep517`-based wheel build fails
looking for its own installed `_sysconfigdata` under a **doubled**
prefix path:

```
python3.14 -m gpep517 build-wheel --prefix=/localtest/usr ... --sysroot /localtest
gpep517 INFO Searching for sysconfig in /localtest/localtest/usr/lib/python3.14
...
RuntimeError: should have found one _sysconfigdata file, found []
die: Wheel build failed
```

`/localtest/localtest/...` — `/localtest` (the `--local` EPREFIX)
appears twice. Same shape of bug as the already-fixed
[[crossdev-pkg-config-sysroot-leak]] and the still-open
[[crossdev-prefix-gcc-header-dir]]: some EPREFIX/sysroot-derived path
is being joined against an already-EPREFIX-inclusive value instead of
a bare one, likely in `distutils-r1`/`gpep517`'s own sysroot handling
or in whatever `em` exports as `--sysroot`/`--prefix` for this
build-system invocation.

## How to attack

1. Reproduce minimally: `em --local DIR setup` then `em --local DIR
   --target i586-pc-linux-gnu crossdev --setup`, same sandbox as the
   sibling perl bug (`em-i586-check`, `/localtest`) — reaches this
   point at package 37/110 once the perl-module fperms bug is bypassed.
2. Find where `em` computes the `--prefix`/`--sysroot` args passed to
   `gpep517 build-wheel` (likely in `python-utils-r1.eclass`
   integration inside `shell.rs` or a `distutils-r1` phase helper) and
   compare against what value is already `${EPREFIX}`-inclusive vs
   bare at that call site.
3. Not yet confirmed whether this is `--local`-specific or would also
   hit `--prefix` + `--target` — bare `--target` never reaches this
   package (no `dev-python/*` in that closure) so no cross-topology
   comparison exists yet.
