# `dev-perl/Module-Build` fperms fails under `--local` (confirmed: upstream eclass gap)

Status: 🟢 root-caused and live-confirmed as an upstream Gentoo eclass
gap, not an `em`-side bug — no fix needed in `em` itself, worth a Gentoo
bug report instead. Found 2026-08-26 testing `--prefix`/`--local` impact of
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
from the setup ladder.

**Confirmed genuinely new territory, not a regression**: inspected the
`em-local-debian` incus container (Debian 12) used for the prior
`--local` setup-ladder verification. Its VDB
(`/root/em-local-check/var/db/pkg`) tops out at `pkgconf`/`baselayout`/
`readline`/`zlib`/`ncurses`/`gnuconfig`/`re2c` — `dev-lang/perl` and the
whole `virtual/perl-*`/`dev-perl/*` closure only ever appear as
`[ebuild N ...]` *plan* lines in old `tc-plan*.log` pretend-output, never
actually merged (`grep fperms *.log` → no hits at all). So this is the
first time a `--local` bootstrap has gotten far enough to actually build
a `perl-module.eclass`-based package — not a previously-working path
that broke.

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

## Root cause (confirmed live)

`dev-lang/perl-5.42.2` is seeded into `package.provided` for this
`--local` prefix (`/localtest/etc/portage/profile/package.provided`) —
the standalone prefix reuses the **host's** system perl rather than
building its own. That host perl's `Config.pm` has no `EPREFIX` baked
into any install path at all (confirmed: `perl -V:siteprefix` on the
sandbox host → `/usr/local`, `-V:installvendorlib` →
`/usr/lib64/perl5/vendor_perl/5.42`, both bare) — it was never
configured as a Prefix build. `Module::Build` derives its own internal
install paths from `Config.pm`, so `--destdir="${D}"` alone can't
reproduce the `EPREFIX` offset the way it apparently does on a real
Gentoo Prefix system (where the bootstrap perl presumably *is* built
Prefix-aware, or Prefix's `${D}` differs — not established which).
Files land at bare `${D}/usr/...` instead of `${D}${EPREFIX}/usr/... =
${ED}/usr/...`, which is exactly where `perl_fix_permissions`'s
`fperms -R u+w /` (resolving against `${ED}`) then can't find them.

**Only one of `perl-module.eclass`'s three build-system branches is
actually affected — and it's shared by two sub-cases, not one.**
`perl-module_src_configure` (same eclass, lines 227–336) handles
`Dist::Build`, `Build.PL` (plain `Module::Build` *or* `Module::Build::Tiny`
— the eclass only branches on which of these two for its QA/BDEPEND
messaging, the actual `Build.PL` configure args below are identical for
both), and `ExtUtils::MakeMaker` differently:

| Branch | Configure-time prefix passed? |
|---|---|
| `Dist::Build` (`Build.PL` + `use Dist::Build`) | `--prefix "${EPREFIX}"/usr` ✓ |
| `Build.PL`, no `Dist::Build` (`Module::Build` *or* `Module::Build::Tiny`) | **none** ✗ |
| `ExtUtils::MakeMaker` (`Makefile.PL`) | `PREFIX="${EPREFIX}"/usr` ✓ |

Only the middle branch — the one `dev-perl/Module-Build` itself takes —
never tells `Build.PL` about `EPREFIX` at all, so it's the only one left
depending on the driving perl's own (possibly EPREFIX-unaware)
`Config.pm`. **Confirmed live** by building `dev-perl/TermReadKey` (a
real `ExtUtils::MakeMaker`/XS package) via `repro/prefix-emerge --nodeps
-v dev-perl/TermReadKey` against the **unpatched** eclass: it merges
cleanly, no QA abort — the `emake DESTDIR="${D}"` line in the
`ExtUtils::MakeMaker` branch was never actually broken, because
`PREFIX="${EPREFIX}"/usr` already made the generated `Makefile`
prefix-aware independent of `Config.pm`. An earlier version of this
doc/fix incorrectly patched that line too, on an unverified assumption
— corrected below.

**Blast radius in-tree** (2026-08-27, real `::gentoo` checkout,
`HEAD=0a8c7730c9`): ebuilds that `inherit perl-module`, declare a
`Module-Build`/`Module-Build-Tiny` BDEPEND, and don't also depend on
`dev-perl/Dist-Build` (which would route them to the unaffected branch):

```sh
grep -lE '^\s*inherit\s+.*\bperl-module\b' */*/*.ebuild \
  | xargs grep -lE 'Module-Build' | xargs grep -L 'Dist-Build'
```

**371 ebuilds** — 352 `dev-perl`, plus `net-misc/clusterssh` (×3),
`sci-biology/bioperl*` (×4), `www-apache/mod_perl` (×2),
`dev-tex/biber` (×2), `app-text/{po4a,App-XML-DocBook-Builder}`,
`perl-core/CPAN`, `media-gfx/graphite2`, `games-arcade/frozen-bubble`,
`dev-util/App-SVN-Bisect`. This is a name-grep upper bound, not
individually confirmed — a source tarball could ship both `Build.PL`
and `Makefile.PL` and (absent `PREFER_BUILDPL=yes`) actually take the
safe `Makefile.PL` branch instead. None of the 371 override
`--prefix`/`EPREFIX` in their own `myconf` for the `Build.PL` step
(checked: zero `myconf.*prefix` hits, and the only 3 files mentioning
"prefix" at all use it for unrelated things — `htslib` include/lib
dirs, `GENTOO_INCDIR`, a mysql basedir) — relevant because it means an
eclass-level fix can't collide with a per-ebuild override anywhere in
this set.

**Three individually live-verified positive reproducers** (real
Portage, `repro/prefix-emerge --nodeps -v <atom>`, unpatched eclass —
all three abort with the same `05prefix` QA gate as the original
`dev-perl/Module-Build` finding), chosen to span the breadth of what's
affected:

- `dev-perl/Module-Build` — the package that started this, plain
  `Module::Build`, self-hosting (bootstraps against its own already-
  merged copy, no extra BDEPEND chain needed).
- `dev-perl/Class-Factory-Util` — a small, otherwise-unrelated plain
  `Module::Build` consumer, to show it's not specific to `Module-Build`
  itself.
- `app-text/po4a` — a real, user-facing application (not a library),
  showing the impact isn't confined to internal Perl tooling.

(`dev-perl/TermReadKey`, from earlier, remains the negative control —
`ExtUtils::MakeMaker`, confirmed *not* affected.) A fourth category,
`Module::Build::Tiny` consumers (e.g. `dev-perl/Cookie-Baker`), is
structurally exposed the same way per the source read above but wasn't
independently live-verified here: reproducing it needs
`dev-perl/ExtUtils-Config`/`-Helpers`/`-InstallPaths` and
`dev-perl/Module-Build-Tiny` itself actually merged into the test
prefix first (they're real `dev-perl/*` packages, not part of the
bundled `virtual/perl-*`/`perl-core/*` set `prefix-emerge` seeds by
default), and the driving host perl can't see modules merged into a
synthetic `EPREFIX` mid-build — Portage's own sandboxed build
environment strips a `PERL5LIB` override aimed at working around that,
which is correct hygiene on Portage's part, not a bug.

All three positive reproducers, and the two negative controls, were
also independently confirmed to install cleanly under **Fix B**
(`app-text/po4a` and `dev-perl/Class-Factory-Util` both merged with no
QA abort once `--prefix "${EPREFIX}"/usr` was added to the `Build.PL`
configure args).

**Two fixes, both live-verified independently** against
`dev-perl/Module-Build` (twice each: once in a real crossdev-stages
sandbox via `em --local`, once standalone via `repro/prefix-emerge
--nodeps -v dev-perl/Module-Build` against a clean real-Portage
prefix):

**Fix A — destdir, install-time** (what was actually shipped/tested
first):

```diff
--- a/eclass/perl-module.eclass
+++ b/eclass/perl-module.eclass
@@ -469,7 +469,7 @@ perl-module_src_install() {
 
 	if [[ -f Build ]]; then
 		mytargets="${mytargets:-install}"
-		mbparams="${mbparams:---destdir="${D}" --pure}"
+		mbparams="${mbparams:---destdir="${ED}" --pure}"
 		einfo "./Build ${mytargets} ${mbparams}"
 		./Build ${mytargets} ${mbparams} \
 			|| die "./Build ${mytargets} ${mbparams} failed"
```

**Fix B — configure-time prefix (recommended)**: matches how
`Dist::Build`/`ExtUtils::MakeMaker` already solve this — tell
`Build.PL` about `EPREFIX` up front instead of patching the destdir
after the fact. Leaves the destdir line untouched.

```diff
--- a/eclass/perl-module.eclass
+++ b/eclass/perl-module.eclass
@@ -295,6 +295,7 @@ perl-module_src_configure() {
 		fi
 
 		set -- \
+			--prefix "${EPREFIX}"/usr \
 			--installdirs=vendor \
 			--libdoc= \
 			--create_packlist=1 \
```

Fix B is the one to lead with in the upstream report: same live-verified
result (`dev-perl/Module-Build-0.423.400-r5 merged.`, no QA abort), more
consistent with the other two branches, and — per the `myconf` check
above — safe to add across all 371 affected consumers with no override
collision.

This confirms the eclass gap but is **not something `em` can ship as a
fix** — the project only runs real, unmodified Gentoo eclasses
([[reimplement-read-the-real-source]]), and this is squarely upstream:
`perl-module.eclass` assumes `${D}` already carries the Prefix offset,
which only holds when the perl driving `Build.PL`/`Config.pm` was
itself built Prefix-aware — untrue here since it's `package.provided`
host perl. Worth a Gentoo bug report; `em`-side, this is a known,
accepted `--local` toolchain-bootstrap gap, not something to special-
case around.

## Standalone reproducer (no `em` involved at all)

[`repro/prefix-emerge`](./repro/prefix-emerge) reproduces this with
**real, unmodified Portage** (`emerge`) and the **real, unmodified**
`dev-perl/Module-Build` ebuild/`perl-module.eclass` — no `em` anywhere in
the loop. It's a small general-purpose wrapper (`prefix-emerge [--prefix
DIR] <emerge args...>`, default `DIR` = `~/.local/share/prefix-emerge`)
that builds a throwaway/persistent `EPREFIX` via `PORTAGE_OVERRIDE_EPREFIX`
(Portage's own documented env var for exactly this kind of Prefix testing,
`portage/const.py`), reusing the host's profile/repo/global defaults and
seeding `package.provided` with the host's real, non-Prefix-built
`dev-lang/perl` plus its installed `virtual/perl-*`/`perl-core/*`
ecosystem — then hands the rest of the command line straight to
`emerge`. This specific bug:

```sh
repro/prefix-emerge --nodeps -v dev-perl/Module-Build
```

Live-verified output (2026-08-27, real Gentoo host, Portage 3.0.81.2):
Portage's own built-in Prefix QA hook catches it before `fperms` even
gets a chance to run — a cleaner, more direct confirmation than the
original `fperms` symptom:

```
QA Notice: the following files are outside of the prefix:
/usr
/usr/bin
/usr/bin/config_data
/usr/lib64/perl5/vendor_perl/5.42/Module/Build
...
ERROR: dev-perl/Module-Build-0.423.400-r5::gentoo failed:
  Aborting due to QA concerns: there are files installed outside the prefix
Call stack:
  misc-functions.sh, line 740:  Called install_qa_check
  misc-functions.sh, line 129:  Called source 'install_symlink_html_docs'
           05prefix, line 115:  Called install_qa_check_prefix
           05prefix, line  29:  Called die
```

This is Portage's own `05prefix` bashrc hook — not `em`, not a `todo/`
inference — hard-failing on exactly the structural mismatch described
above. Solid grounds for a Gentoo bug report.

One side note from the same run: real Portage's `fperms` did *not*
hard-die the way the `em --local` run above did (`perl_fix_permissions`
completed with no visible error, before the QA hook aborted the merge
afterward) — real Portage's `fperms` appears to tolerate a missing target
path rather than hard-failing on it. That's a separate, minor leniency
gap consistent with [[strict-over-portage-leniency]], not part of the
core bug.

## How to attack

1. File upstream: report to Gentoo that `perl-module.eclass`'s `Build.PL`
   branch (plain `Module::Build`/`Module::Build::Tiny`, ~371 in-tree
   consumers) never passes `EPREFIX` to the build system, silently
   breaking Portage's own Prefix QA check whenever the driving perl
   isn't itself Prefix-aware (e.g. a `package.provided` bootstrap perl)
   — unlike its `Dist::Build`/`ExtUtils::MakeMaker` siblings, which
   already pass it. Lead with Fix B (configure-time `--prefix`); Fix A
   is the fallback. The standalone reproducer above is ready to
   attach/link.
2. `em`-side: no action planned; this is an inherent property of
   reusing host perl via `package.provided` during `--local` bootstrap,
   not a resolvable `em` bug. Revisit only if it turns out to block
   more than this one package in practice.
3. Reproduce end-to-end via `em`: `em --local DIR setup` then `em
   --local DIR --target i586-pc-linux-gnu crossdev --setup` (real
   crossdev-stages sandbox; `/localtest` in the `em-i586-check` sandbox
   as of this writing, `package.provided` already seeded — 27 entries,
   including `dev-lang/perl-5.42.2`). Or reproduce standalone via
   `repro/prefix-emerge --nodeps -v dev-perl/Module-Build` above, which
   needs only real `emerge`.
