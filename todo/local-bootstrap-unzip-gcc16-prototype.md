# Stable `app-arch/unzip-6.0_p29-r2` fails to build under GCC 16.1.1 (strict prototype conflict)

Status: 🔴 not started. Found 2026-08-23 during the first genuine, full,
real (non-pretend) `em --local` toolchain bootstrap run
([[local-bootstrap-provided]]) — `dev-lang/python`'s own dependency
closure pulls in `app-arch/unzip`, which fails to compile.

**Corrected same day** after Luca pushed back on an unverified first pass
that got two things wrong: it blamed "GCC 15" (the plan's *later,
unreached* `sys-devel/gcc-15.3.0` stage step, never inspected the actual
compiler that ran) and claimed no fix exists in the tree (never actually
tried building the alternative). Both wrong — see below.

## The bug

`unix/unxcfg.h:120` in `unzip-6.0_p29-r2`'s own source declares
`gmtime()`/`localtime()` with a bogus K&R-style no-argument prototype
(`struct tm *(void)`), conflicting with glibc's real declaration
(`struct tm *gmtime(const time_t *)`). The build log's own compile
commands confirm the actual compiler was
**`aarch64-unknown-linux-gnu-gcc`, resolved to this host's real seed
compiler, `gcc-16.1.1_p20260613`** (`--local`'s native bootstrap uses the
host's own seed compiler for early ladder steps, catalyst-model) — not
the later, unreached `gcc-15.3.0` stage-6 target. GCC 16 rejects the
conflicting redeclaration as a hard error where older GCC only warned.

## Verified: a fix already exists in `::gentoo`, just not on the stable channel

`app-arch/unzip-6.0_p31` (`~arch`-only, `KEYWORDS="~alpha ~amd64 ...
~arm64 ..."`) **does build successfully with this exact compiler** —
confirmed two ways: (1) real system `emerge` on this same host built and
installed it for real today (`/var/db/pkg/app-arch/unzip-6.0_p31`,
`BUILD_TIME` timestamped during this session — unrelated shared-machine
activity, not caused by this session's `--local` testing, which is
structurally confined to its own prefix); (2) `p31`'s own patch set
(diffed against `p29-r2`'s) doesn't touch `unxcfg.h` either, so the fix
is in `p31`'s newer *upstream* tarball itself, not a Gentoo-side patch —
an earlier pass's patch-only diff missed this.

`p31` is not reachable from a fresh `--local` prefix's own default
(stable) `ACCEPT_KEYWORDS` — confirmed: `em -p --local DIR --nodeps
=app-arch/unzip-6.0_p31` reports `all ebuilds masked (~arm64 keyword)`.
This is correct, expected `em` behavior (a fresh prefix defaults to
stable, same as real portage, and does not silently inherit the host's
own `ACCEPT_KEYWORDS=~arm64` from `make.conf`) — not a solver bug.

## Why it surfaced now

Not new — first time anything has run `dev-lang/python`'s full real
dependency closure to completion under `--local` (previous passes either
used `--nodeps` shortcuts or never got this far before timing out).
Unrelated to the `package.provided`/`InstalledPolicy::Provided` fix
landed the same day (`4e3d2a9`).

## How to attack

Not an `em` code bug — the solver correctly picked the stable candidate;
the stable candidate is genuinely broken on GCC 16, and the fix (`p31`)
is real but not yet stabilized in `::gentoo`. Options for unblocking
`--local` bootstrap specifically: (a) accept `~arch` for just this one
package during bootstrap (`package.accept_keywords`, e.g. via a Tier-1-
style bootstrap override, matching how `setup::provided` already curates
a bootstrap-specific package list) — narrowest, most correct fix; (b)
carry a local one-line patch to `unxcfg.h` (`#include <time.h>` or drop
the bogus redeclaration) if pinning to `p29-r2` is preferred; (c) wait
for Gentoo to stabilize `p31` upstream (out of `em`'s control, and no ETA
known).
