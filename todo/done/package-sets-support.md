# Package-set (`@name`) support gaps

STATUS: **✅ done (2026-08-13)** for everything except `@security`. The gaps
recorded below (`@selected-packages`, `@selected-sets`, `@live-rebuild`,
`@deprecated-live-rebuild`, `@module-rebuild`, `@x11-module-rebuild`) and the
`@profile` bug are all implemented; see `done/for_opencode.md` for the commit
SHAs and implementation notes. `@security` stays open (needs a GLSA
subsystem). This audit is kept as the historical record of what was found.

Luca asked whether `em` supports the built-in sets documented at
https://wiki.gentoo.org/wiki/Package_sets. Audited 2026-08-12 against real
portage's actual set registry — `portage/_sets/__init__.py`'s
`PSetsDict._parse` default `.conf` (the source the wiki page documents),
vendored locally at `/home/lu_zero/Sources/portage-3.0.79`. Status below;
pick items up independently, they don't depend on each other.

`em --info -v` now lists every known `@name` set and its resolved atoms
(or its resolve error) directly — `em`-specific, no real-emerge
equivalent — so re-checking this list against a live host no longer needs
the manual grepping this first audit did.

## Supported today

- `@world` — `portage_repo::SetResolver::direct_members("world")`:
  `@selected ∪ @system`. Missing the `@profile` addition real portage's
  `world` set formula has (`"@profile @selected @system"`) — see the bug
  below for why that's a near-total no-op in practice, not a real gap.
- `@selected` — world file (`var/lib/portage/world`) + `world_sets` refs,
  combined. Matches real portage's `WorldSelectedSet`.
- `@system` — profile `packages`' `*`-prefixed entries
  (`ProfileStack::system_set`). Matches `PackagesSystemSet`.
- `@preserved-rebuild` — special-cased in `emerge.rs::expand_sets` (VDB +
  preserve-libs registry query), not routed through `SetResolver` at all —
  documented inline there as deliberate (`SetResolver` has no VDB access).
- User-defined sets — `/etc/portage/sets/<name>` and `sets.conf` `[name]`
  sections (`SetResolver::user_set_members`/`lookup_sets_conf`).
- `world_sets` add/remove — a world-candidate `@name` typed directly on the
  command line is recorded to (and `-W`-removable from) `world_sets`
  independently of `world` — see below, fixed 2026-08-12.

## `world_sets` read/write — FIXED 2026-08-12

Found live against a real host with `@openwrt-prerequisites` registered in
`/var/lib/portage/world_sets` (put there by real `emerge` at some point,
not by `em`): `em`'s `-vv` purple/bold-magenta coloring correctly picked up
every member of the set as `@selected` (matches real `emerge -p1` bold on
the same host). Reading `world_sets` always worked; writing it didn't, in
either direction.

Real portage's write mechanism (`_emerge/depgraph.py::_calc_deselect`
around line 11049 for the add side, `_emerge/actions.py::action_deselect`
~line 1737 for the remove side): a top-level CLI `@name` argument is
recorded to `world_sets` as the literal, unexpanded `@name` reference —
never its members — and only when the named set is a `world-candidate`
(real portage's default `sets.conf`, `cnf/sets/portage.conf`, sets
`world-candidate = True` only on `[usersets]`, i.e. genuinely
user-defined sets under `etc/portage/sets/`; every other built-in
defaults to `False` and is never recorded no matter how it's invoked).
`world_sets` and `world` are two independent files matched independently:
a `@name` token only ever matches `world_sets` lines, a plain-atom token
only ever matches `world` lines (slot/repo-aware, plus VDB-cpv expansion
on the remove side).

Fixed, mirroring both halves:

- `portage_repo::is_world_candidate` (`portage-repo/src/repo/sets.rs`) —
  the exclusion list of every named built-in set in
  `cnf/sets/portage.conf` (not just the ones `em` implements yet, so a
  future `@security`/`@live-rebuild` etc. stays correctly excluded without
  a second list to update).
- `emerge.rs::select_world_set_refs` — the `add_atoms` sibling:
  deduplicated, world-candidate `TargetOrigin::Set` names from this run's
  root targets, gated identically to `world_atoms` (`update_world`,
  `!oneshot`, `!pretend`, `!buildpkgonly`, `!fetchonly`, `!onlydeps`).
  `select_world_atoms` already excluded `Set`-origin atoms from `world`
  correctly (matches real portage: members never get added individually)
  — that half needed no change, only the missing reference-write.
- `maint::world::add_set_refs` — writes `@name` lines to `world_sets`,
  idempotent like `add_atoms`.
- `maint::world::remove_atoms` — now removes plain-atom tokens from
  `world` and `@name` tokens from `world_sets` independently (was: `@name`
  tokens only ever matched a literal `@name` *line inside `world`
  itself*, this codebase's own pre-existing leniency, which is kept
  as a fallback for `world` but no longer the only place checked).
  Never creates `world_sets` when nothing to remove would land there
  (most installs don't have one).

Live-verified: `em -W @myset --root <scratch>` against a hand-seeded
`world`+`world_sets` removed `@myset` from `world_sets` only, leaving
`world` and `@other-set` untouched. Unit tests cover the add side
(`add_set_refs_appends_a_new_name`, `..._is_a_no_op_when_already_present`)
and the split remove behavior
(`deselect_removes_a_name_from_world_sets_not_world`,
`..._never_touches_a_missing_world_sets`), plus `is_world_candidate`'s
exclusion list and `select_world_set_refs`'s gating/dedup.

## Not implemented at all

✅ **Done 2026-08-13** except `@security`. Implementation landed in
`done/for_opencode.md` (commits `b072add`/`35b1ee5`/`059079d`); the bullets
below are the original audit text, kept for the record.

- ✅ `@selected-packages` — real portage's `WorldSelectedPackagesSet`: just the
  world file's packages, no `world_sets` expansion. (Was: `em` only had the
  combined `@selected`.)
- ✅ `@selected-sets` — real portage's `WorldSelectedSetsSet`: just the
  `world_sets` refs (unexpanded set names), no packages.
- ⏳ `@security` — GLSA-based (`NewAffectedSet`). Big feature (needs a GLSA
  data source); lowest priority of this list. **Still open** — check whether
  the `em glsa` applet already has reusable GLSA machinery before rescoping.
- ✅ `@live-rebuild` / `@deprecated-live-rebuild` — VDB query on
  `PROPERTIES=live` / `INHERITED` matching `portage.const.LIVE_ECLASSES`.
- ✅ `@module-rebuild` / `@x11-module-rebuild` — file-ownership sets
  (`OwnerSet`: packages owning `/lib/modules` / `/usr/lib*/xorg/modules`
  minus `/usr/bin/Xorg`). Implemented with a dedicated `contents_contains`
  (any CONTENTS kind, incl. `dir`) rather than reusing `InstalledPackage::owns`
  (which is `Obj`/`Sym`-only for the `qfile` use case).

## Bug found while auditing — ✅ fixed 2026-08-13 (`c805c1f`)

`@profile`, as implemented (`portage-repo/src/repo/sets.rs`'s
`direct_members("profile")` → `ProfileStack::packages()`), returned **every**
profile `packages` line — both `*`-prefixed (system) and plain — mapped
straight to a flat `GroupEntry` list with the system/plain bit discarded.
That was actually `@system ∪ (real @profile)`, not real portage's `@profile`.

Real portage's `@profile` (`ProfilePackageSet`) is *only* the non-`*`
"advisory" lines (`x[:1] != "*"`), and — critically — **only from profiles
whose `profile-formats` declares `profile-set`**, a rare, mostly-unused
profile-format feature; on essentially every real Gentoo profile, real
`@profile` is empty.

**Fix:** `ProfileStack::profile_set()` returns only `PackageEntry::Plain`
lines, and only from profile nodes whose enclosing repo (resolved by walking
up to the nearest `metadata/layout.conf`) declares `profile-set`. The
site-local `/etc/portage/profile` is hardcoded to `profile-set` (portage's
`LocationsManager.py:182`), so it still contributes — the one real-world case
where stock-Gentoo `@profile` is non-empty. The mislabeled `system_set()`
doc comment is corrected in the same commit. Verified on a live host: `em
--info -v` shows `@profile` empty, matching `emerge -p @profile`.

The original audit text (kept for context) follows:

Real portage's `@profile` (`ProfilePackageSet`) is *only* the non-`*`
"advisory" lines (`x[:1] != "*"`), and — critically — **only from profiles
whose `profile-formats` declares `profile-set`**, a rare, mostly-unused
profile-format feature; on essentially every real Gentoo profile, real
`@profile` is empty. So today's `@profile` isn't just wrong, it's
*differently wrong in a way that's hard to notice*: it'll return real
`@system`'s members (correct-looking, since those really are "profile
packages") plus every advisory plain line from *every* profile in the
stack, regardless of `profile-formats` — never empty the way real `@profile`
almost always is.

There's also a mislabeled doc comment adjacent to this:
`portage-repo/src/repo/profile.rs:481`, on `ProfileStack::system_set()`,
says "Matches portage's `ProfilePackageSet`" — it actually matches
`PackagesSystemSet` (`@system`). `ProfilePackageSet` is the *other* class,
the one behind real `@profile`. Fix the doc comment in the same pass as
whichever of `@system`/`@profile` gets touched next, so the two don't
drift back out of sync again.

Fixing `@profile` properly needs: (a) a way to read `profile-formats` per
profile to gate on `profile-set` (check whether `ProfileStack`/profile
loading already parses `profile-formats` for something else — PMS 5.1
requires reading it regardless for repo-dep and other format flags, so this
may already be sitting in a field somewhere), and (b) filtering
`packages_raw()`'s `PackageEntry::Plain` entries instead of folding them in
with `System`.
