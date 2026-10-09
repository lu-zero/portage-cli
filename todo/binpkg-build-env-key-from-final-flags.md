# The binary package build-env key is derived from flags the ebuild changed

Status: ✅ fixed 2026-10-09 with the first option below (Luca: fix it):
the key of the configured flags is recorded in the package as
`BUILD_ENV_KEY`. Found in the stage test of [[phase-order-and-binpkg]];
see [[binpkg-stage-test-defects]].

## Symptom

`-K` on arm64: `sys-libs/glibc: no matching binpkg and source builds
disabled`, with two glibc packages in `PKGDIR`. With `-k` glibc is
rebuilt from source every time.

## Cause

A package is reusable when its build-env key equals the wanted one. The
wanted key comes from the configured flags (`make.conf`, `package.env`).
The package's key is computed from the `CFLAGS`, `CXXFLAGS`, `LDFLAGS`
and `RUSTFLAGS` recorded in it, and those are the shell's values after
the build: what the ebuild left there.

glibc on arm64 appends `-mbranch-protection=none`. The key keeps
machine flags, so the package gets a key (`1323e14d75fb`) and the
configuration gives `generic`.

Recording the final flags is what Portage does too (its VDB `CFLAGS`
for glibc on this host has the same appended flags). Portage does not
match packages on flags at all; the key is `em`'s own.

Other packages record changed flags as well (perl, gmp, openssl,
gettext, m4: `-fno-strict-aliasing`, `-std=gnu17`, …) but those flags
are not part of the key, so they match.

## Options

- Record the configured flags separately at build time and compute the
  package's key from those. Keeps `CFLAGS` as Portage has it.
- Record the key itself in the package.
- Keep deriving it from the final flags and accept that an ebuild adding
  a machine flag makes its packages unreusable.

Luca to decide; the first is the smallest change that makes the key
mean what the user configured.
