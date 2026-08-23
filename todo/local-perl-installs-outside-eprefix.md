# Perl packages under `--local`: borrowed host perl installs outside `${ED}`

Status: 🔴 not started, precisely root-caused 2026-08-24. Originally
misdiagnosed (see below) while chasing
[[jobs-rdepend-backwards-edge-race]] — `dev-perl/Module-Build-0.423.400-r5`
died at `perl_fix_permissions` (`fperms -R u+w /`) with `chmod: cannot
access '<image>/tmp/.../em-local-test2/': No such file or directory`.

**Renamed from the original title** ("fperms double-prefixes an
already-absolute argument") — that was wrong. `fperms`'s own code is
correct: `under_ed`/`${ED}` computation faithfully matches real PMS
semantics (`ED = D + EPREFIX`), and for `--local` that legitimately means
`${ED}` nests the *entire* absolute prefix path under `image/` — not a
bug, not a double-prefix. Traced further and found the real cause below.

## The actual bug

`dev-lang/perl` is in `setup::provided::TIER1` — a **borrowed host
perl**, not built under `--local`'s own `EPREFIX`. Confirmed on disk
after the failed merge:

- The real installed module files (`Notes.pm`, `PPMMaker.pm`, …) landed
  at `<workdir>/image/usr/lib64/perl5/vendor_perl/5.42/Module/Build/…`
  — under plain `${D}`, no `EPREFIX` component at all.
- `em`'s own native `dodoc` (EPREFIX-aware) correctly wrote
  `<workdir>/image/<full-eprefix-path>/usr/share/doc/Module-Build-…/…`
  — under the real `${ED}`.

Two different install paths for the same package, because they went
through two different mechanisms: `Module::Build`'s own `./Build
install` (perl-module.eclass) uses paths straight out of the **host**
perl's `Config.pm` (`installvendorlib` etc.), which has no concept of
`EPREFIX` — it's a plain system perl. `DESTDIR=${D}` alone doesn't fix
this: `Config.pm`'s reported paths are missing the `EPREFIX` segment
entirely, so `make install DESTDIR=${D}` writes to `${D}/usr/…`, never
touching `${D}/${EPREFIX}/usr/…` (`${ED}`) at all.

By the time `perl_fix_permissions` (`perl-functions.eclass`) runs
`fperms -R u+w /` — correctly resolving to `chmod -R u+w ${ED}/` — `${ED}`
doesn't exist yet (nothing perl-installed ever created it; only `dodoc`
had, and only its own subtree, not the top-level `${ED}` itself in every
case). Hence the die.

## Why this is systemic, not one ebuild

Any `dev-perl/*` package built under `--local` with the borrowed host
perl hits the same mismatch — this is the exact same architectural class
of problem that made `dev-lang/python` need `InstalledPolicy::Provided` +
a real from-scratch build in `TIER1` (`jobs-rdepend-backwards-edge-race.md`),
except `perl` was never added to that "must be a real, EPREFIX-aware
build" category. Real Gentoo Prefix's own bootstrap doesn't borrow the
host's perl for this exact reason — it builds its own Prefix-aware one
early.

## How to attack

Not yet decided — options, roughly in order of how closely they match
what real Gentoo Prefix does:

1. **Build a real `dev-lang/perl` under `--local` too**, same shape as
   `dev-lang/python`'s fix today: add it as an explicit `toolchain_plan`-
   adjacent step (or otherwise force a real build) so its own `Config.pm`
   is EPREFIX-aware. Most correct, most work — perl's own build/bootstrap
   chain needs auditing the same way python's did.
2. **Keep perl provided/borrowed, but make `em`'s own install glue
   relocate files perl installed outside `${ED}`** — a `${D}` → `${ED}`
   reconciliation pass after `Module::Build`'s own install step,
   specific to `perl-module.eclass`-driven packages. Narrower, more
   fragile (has to catch every path perl's own installer might use).
3. **Accept the limitation, skip `perl_fix_permissions`'s failure mode
   specifically** (e.g. tolerate a missing `${ED}` for `-R` chmod rather
   than dying) — papers over the symptom, doesn't fix the real file
   placement mismatch, so packages would still install to the wrong
   subtree and be broken (or invisible to the VDB's real `${EROOT}`)
   even if the merge itself no longer dies.

Needs live investigation into how much of perl's own build genuinely
requires EPREFIX-awareness before picking a direction — same kind of
audit `dev-lang/python`'s own toolchain_plan step already went through.
