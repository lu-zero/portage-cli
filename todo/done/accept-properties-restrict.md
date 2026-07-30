# `ACCEPT_PROPERTIES` / `ACCEPT_RESTRICT` visibility gates

STATUS: **✅ done (2026-07-29).** Implemented reusing `AcceptLicense`/
`AcceptLicenses` verbatim (type-aliased `AcceptProperties`/`AcceptRestrict`),
per the consolidation decision below — no third hand-rolled manager. Global
`ACCEPT_PROPERTIES`/`ACCEPT_RESTRICT` (process env, else profile/make.conf,
default `*`) + `/etc/portage/package.properties`/`package.accept_restrict`
(site + config-overlay) load in `use_env.rs`; gate wired into
`Adapter::version_accepted` and `filter_reasons_for` (`FilterReason::
Properties`/`Restrict`); autounmask writes `package.properties`/
`package.accept_restrict` entries same as license. `RestrictExpr`'s
USE-conditional branches evaluate against effective USE, same as `LICENSE`.
Tests: `portage-resolve/src/repo.rs`'s `accept_properties_and_restrict_reuse_
the_license_engine` (unit) and `restrict_not_accepted_filters_the_version`
(end-to-end via `filter_reasons_for`/`version_accepted`).

## The gap

`em`'s depgraph honours every `package.*` visibility gate **except**:

- `package.properties` / `ACCEPT_PROPERTIES`
- `package.accept_restrict` / `ACCEPT_RESTRICT`

`PROPERTIES` and `RESTRICT` are already parsed into metadata
(`portage-repo/src/build/env.rs` — `restrict`, `properties` fields), but nothing
gates package visibility on them during resolution.

## How portage implements it (reference)

- `portage/package/ebuild/config.py`
  - `ACCEPT_PROPERTIES` → `config._accept_properties`; `ACCEPT_RESTRICT` →
    `config._accept_restrict` (incremental tokens, like `ACCEPT_LICENSE`).
  - per-package files grabbed in `__init__` into `config._ppropertiesdict`
    (`/etc/portage/package.properties`) and `config._paccept_restrict`
    (`/etc/portage/package.accept_restrict`), via `grabdict_package` +
    `ExtendedAtomDict` — same machinery as `package.license` → `_plicensedict`.
  - check methods `config._getMissingProperties(cpv, metadata)` and
    `config._getMissingRestrict(cpv, metadata)`: start from the global accept
    list, fold in matching per-package entries via
    `ordered_by_atom_specificity`, `use_reduce` the ebuild's
    `PROPERTIES`/`RESTRICT` (they can be USE-conditional, e.g.
    `bindist? ( bindist )`), and return the tokens not accepted.
- `portage/package/ebuild/getmaskingstatus.py` — `_getmaskingstatus()` calls
  those two alongside the keyword/`package.mask`/license checks and appends a
  mask reason; that is what makes them a *visibility* filter.

### Token semantics

Space-separated; `*` accepts all, `-token` denies. **No** `@GROUP` expansion
(simpler than license). Defaults (`make.globals`): `ACCEPT_PROPERTIES="*"`,
`ACCEPT_RESTRICT="*"` — why they rarely mask.

Real-world uses: `ACCEPT_RESTRICT="* -bindist"` (refuse non-redistributable),
`ACCEPT_PROPERTIES="* -interactive"` (refuse interactive ebuilds in batch/CI).

## How to mirror it here

A third visibility gate parallel to `AcceptLicenses`
(`portage-cli/src/query/depgraph/repo.rs`):

- an `AcceptProperties` / `AcceptRestrict` bundle = global accept list +
  per-package overlay, `effective_for(cpv, slot)` borrowing the global decision
  on the common no-override path;
- evaluate against the `use_reduce`'d `PROPERTIES`/`RESTRICT` field (USE-cond
  branches against the version's effective USE, like the license path already
  does with `accepts_expr`);
- contribute a `FilterReason` + an autounmask suggestion
  (`package.accept_restrict` / `package.properties`).

Cheap once the accept-list/overlay pattern from keywords/license is reused.

## `package.env`

DONE — see `todo/done/package-env.md`: both the non-USE build-environment
slice and the resolver-side USE-from-`package.env` slice landed 2026-07-20
(`load_package_env_use` in `portage-resolve/src/use_env.rs`).

## Consolidation decision (2026-07-29, Sonnet + Fable second opinion)

Considered whether `AcceptKeywords`/`AcceptLicenses` (and this file's planned
properties/restrict) should share one generic engine. Checked real PMS
(`profile-variables.tex`, cloned from `anongit.gentoo.org/git/proj/pms.git`):
the spec's own "incremental variable" mechanism (the formal `-token`/`-*`
algorithm) is mandatory only for `USE`/`USE_EXPAND*`/`CONFIG_PROTECT*` —
profile-stacking only, no per-atom-override dimension. `ACCEPT_KEYWORDS`/
`ACCEPT_LICENSE`/`ACCEPT_PROPERTIES`/`ACCEPT_RESTRICT` are **not mentioned
anywhere in PMS** — pure package-manager policy that borrows the same token
syntax by convention, not because the spec unifies them. Real Portage's own
`KeywordsManager`/`LicenseManager` are separate hand-written classes.

**Verdict: two tiers, not three, and not one.** Keywords stay separate
(`ArchAccept`: arch-scoped bool flags, no USE-conditional evaluation ever,
since PMS's `KEYWORDS` is a flat non-conditional list per `ebuild-vars.tex`).
Properties/restrict join the license family instead of getting a third
hand-rolled implementation: `LICENSE`/`PROPERTIES`/`RESTRICT` (unlike
`KEYWORDS`) use the full dependency-spec grammar and can be USE-conditional,
needing the same `use_reduce`-style evaluation license already has.

Fable's follow-up sharpened the license-family plan further: `@GROUP`
expansion is a **parse-time-only** concern — `AcceptLicense::from_tokens` is
the only method touching `LicenseGroupRegistry`; `merge`/`accepts`/
`accepts_expr` never do. So the group-free "core" properties/restrict needs
already exists as `AcceptLicense` itself — construct it via a group-free path
(properties/restrict tokens have no groups) and reuse the `AcceptLicenses`
wrapper shape verbatim (one type, instantiated three times), rather than
writing new abstraction.

The `AcceptKeywords`/`AcceptLicenses` per-package storage shape difference
(raw tokens refolded per query vs. pre-merged overlay objects) looked like an
inconsistency worth normalizing, but isn't — it's cost-driven, not gratuitous
(`ArchAccept` is `Copy`, free to refold, and the pending `-arch` fix needs raw
tokens since incremental `-tok` cancellation is order-dependent; licenses
pre-merge to avoid re-interning/re-expanding groups per query). Leave both as
they are.

### Known gap carried forward: per-package match ordering

Both `AcceptKeywords::decision` and `AcceptLicenses::effective_for` apply
matching `package.*` overlay entries in **file order** (`Vec` iteration).
Real Portage applies them via `ordered_by_atom_specificity` — more specific
atoms win regardless of file position. Not yet confirmed to cause an actual
visible bug (most `package.*` files don't have conflicting overlapping atoms
for the same cpv), but it's a real divergence from spec behavior and should
be fixed in one shared spot once something depends on it — do not silently
carry it into the properties/restrict implementation without at least a
comment noting the gap.
