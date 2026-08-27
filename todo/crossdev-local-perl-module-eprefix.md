# `dev-perl/Module-Build` fperms fails under `--local` (uncertain: em vs upstream)

Status: 🔴 not started, genuinely unsure yet whether this is an `em`-side
bug or an upstream eclass gap real Gentoo Prefix also has. Found
2026-08-26 testing `--prefix`/`--local` impact of
[[crossdev-pkg-config-sysroot-leak]] in a real crossdev-stages sandbox —
unrelated to that fix; a completely different mechanism (perl install
destdir vs. `fperms`'s path resolution).

## The bug

`em --local /localtest setup` (the correct prerequisite — see below)
followed by `em --local /localtest --target i586-pc-linux-gnu crossdev
--setup` gets deep into the host BDEPEND closure (153 packages;
`--local` starts from nothing installed, unlike bare/`--prefix` which
inherit the stage3's pre-seeded host tools) and fails partway through:

```
./Build install --destdir=/localtest/var/tmp/portage/localtest/dev-perl/Module-Build-0.423.400-r5/image/ --pure
Installing .../image/usr/share/man/man1/config_data.1
...
 * Fixing installed file permissions
chmod: cannot access '.../image/localtest/': No such file or directory
die: fperms failed
```

**Note**: `em --local`'s *setup ladder* (repo sync, profile resolution,
`package.provided` seeding, baselayout merge) has been live-verified
working recently on real Gentoo, Debian, and FreeBSD hosts
([[local-setup-prereq]]) — this is not that path failing again. This
failure is specifically in the *toolchain bootstrap* closure (`em
--local ... crossdev --setup`'s BDEPEND resolution), which
[[local-bootstrap]] already marks as "still the open piece" separately
from the setup ladder. Worth double-checking against a plain **native**
(non-crossdev) `--local toolchain --setup` run to see whether
`dev-perl/Module-Build` (or perl-module.eclass generally) has actually
been exercised under `--local` before, or whether this is genuinely new
territory the crossdev path reaches first.

## What's confirmed

Real, unmodified `perl-functions.eclass`:

```sh
perl_fix_permissions() {
	fperms -R u+w /
}
```

`fperms`'s path argument is interpreted relative to `${ED}` (PMS EAPI7:
`ED = D + EPREFIX`). But real, unmodified `perl-module.eclass` installs
via `./Build install --destdir="${D}"` — bare `${D}`, no `EPREFIX` —
confirmed directly from the actual invocation logged (`--destdir=.../image/`,
no `/localtest` suffix). So files land at `${D}/usr/...`, and `${ED}`
(`.../image/localtest/`) never gets created — a structural mismatch
between where the eclass installed and where `fperms`'s default path
resolves, given `em`'s `${D}` is bare (EPREFIX-free), which is itself
correct PMS EAPI7 behaviour.

Whether real Gentoo Prefix has some compensating mechanism for this
specific eclass pairing (a patch, a different `${D}` convention under
Prefix specifically, or `dev-perl/Module-Build` simply never being
pulled into a real Prefix bootstrap before) is not yet established.

## How to attack

1. First determine whether this is `em`-side or upstream: check whether
   real Gentoo Prefix's own `${D}` differs from `em`'s for a Prefix
   build (i.e. does real Prefix Portage make `${D}` *already* include
   `EPREFIX`, unlike bare EAPI7 semantics?), or whether
   `dev-perl/Module-Build` genuinely has a latent bug real Prefix users
   just haven't hit.
2. If `em`-side: find where `shell.rs` computes `${D}` for a `--local`
   phase and compare against what real Gentoo Prefix's `ebuild.sh`
   would compute for the same inputs.
3. If upstream: decide whether `em` should special-case this (unlikely
   to be worth it for one package) or simply accept it as a known
   `--local` toolchain-bootstrap gap, tracked here.
4. Reproduce minimally: `em --local DIR setup` then `em --local DIR
   --target i586-pc-linux-gnu crossdev --setup` (real crossdev-stages
   sandbox; `/localtest` in the `em-i586-check` sandbox as of this
   writing, `package.provided` already seeded — 27 entries).
