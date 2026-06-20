# `ACCEPT_PROPERTIES` / `ACCEPT_RESTRICT` visibility gates (+ `package.env`)

STATUS: **deferred — lowest priority; prerequisites now DONE.** The rest of the
`package.*` and interning work this note waited on has landed (commits 26fa1d7 →
10f3ffd: `package.accept_keywords`, `package.license`, and the fully-interned
keyword/license/`package.use`/force-mask hot paths — a net 1.16–1.27× resolve
speedup, see below). These two `ACCEPT_*` gates almost never mask anything
(defaults accept all), so they remain parity polish, not a correctness blocker —
the last visibility gate left after keywords/license/mask.

## The gap

`em`'s depgraph honours every `package.*` visibility gate **except**:

- `package.properties` / `ACCEPT_PROPERTIES`
- `package.accept_restrict` / `ACCEPT_RESTRICT`
- `package.env` (per-package env files — a different kind of work; see bottom)

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

## `package.env` (separate, larger)

`em` can *edit* `package.env` (`portage-cli/src/pkg.rs`) but resolution never
*applies* the per-package env files (they can set `USE`, `FEATURES`, `CFLAGS`,
…). This is not a simple visibility gate — it layers environment that can change
USE resolution — so treat it as its own feature, not part of this note.
