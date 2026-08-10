# PMS Notes for portage-repo

Research notes on the [Package Manager Specification (PMS)](https://projects.gentoo.org/pms/9/pms.html)
chapters relevant to profile/config resolution in this crate (`src/build/profile.rs`).
Distilled quotes, not a full mirror — re-fetch the source URL above if a
section needs re-checking.

## Incremental Variables (PMS 5.3.1)

> "Incremental variables must stack between parent and child profiles in the
> following manner: Beginning with the highest parent profile, tokenise the
> variable's value based on whitespace and concatenate the lists. Then, for
> any token T beginning with a hyphen, remove it and any previous tokens
> whose value is equal to T with the hyphen removed."

Variables requiring incremental treatment in all EAPIs:
`USE`, `USE_EXPAND`, `USE_EXPAND_HIDDEN`, `CONFIG_PROTECT`, `CONFIG_PROTECT_MASK`.

For EAPIs supporting profile-defined IUSE injection (table 5.6, EAPI 5-9),
also: `IUSE_IMPLICIT`, `USE_EXPAND_IMPLICIT`, `USE_EXPAND_UNPREFIXED`.

For EAPI 7-9 (table 5.7), also: `ENV_UNSET`.

`ACCEPT_LICENSE` is **not** in this list (confirmed independently against
`portage.const.INCREMENTALS` on a live system — also absent there).

## USE_EXPAND (PMS 5.3.2)

> "Defines a list of variables which are to be treated incrementally,
> exported to the ebuild environment, and whose contents are to be expanded
> into the USE variable as passed to ebuilds."

Read carefully: this is not describing `USE_EXPAND` itself — it's saying the
variables *listed by* `USE_EXPAND` (`VIDEO_CARDS`, `L10N`, `PYTHON_TARGETS`,
…) are themselves to be treated incrementally, via the 5.3.1 stack-and-cancel
algorithm, same as `USE`. Easy to misread as "only `USE_EXPAND` the variable
name is incremental, its listed variables are plain last-wins" — that
misreading does **not** hold up against the verbatim text.

## make.defaults (PMS 5.2.4)

> "make.defaults is used to define defaults for various environment and
> configuration variables. This file is unusual in that it is not combined
> at a file level with the parent—instead, each variable is combined or
> overridden individually as described in section 5.3."

No profile-specific stacking algorithm beyond 5.3.1/5.3.2 — make.defaults
defers entirely to the general incremental-variable rules.

## Empirical confirmation against real portage (3.0.81.2)

Live-tested with a synthetic `base`/`arch` profile chain (`base` sets
`VIDEO_CARDS="dummy fbdev"` + `L10N="en"`, child `arch` sets
`VIDEO_CARDS="fbdev"` + `L10N="de"`, no make.conf involved):

```
VIDEO_CARDS raw: 'fbdev dummy'   # union survives, not last-wins
video_cards_ flags in USE: ['video_cards_dummy', 'video_cards_fbdev']
```

Negation also works as the 5.3.1 algorithm describes: `arch` setting
`VIDEO_CARDS="-dummy amdgpu"` correctly drops just `dummy`, keeping `fbdev`
from `base` and adding `amdgpu`.

**Once make.conf explicitly sets the same variable, it fully replaces** the
profile-chain-accumulated value (verified: make.conf `VIDEO_CARDS="amdgpu"`
+ `L10N="de"` → raw values are exactly `amdgpu` / `de`, nothing from the
profile chain survives). PMS is silent on make.conf — this is reference
`portage`'s (`config.py`'s `source_incremental`-equivalent) actual behavior,
not a spec requirement, but it's what real `emerge`/`portageq` produce.

## Implication for `em` — FIXED 2026-08-10

`profile.rs`'s `INCREMENTAL_VARS` deliberately excludes individual
`USE_EXPAND`-listed variables (`VIDEO_CARDS`, `L10N`, …) — they need
different treatment per-layer, not the single raw-merge rule that list
drives. `ProfileStack::profile_env` (the profile chain) now translates each
layer's own value into signed USE tokens and folds them into the
accumulator's `USE` as it goes, matching real portage's `make_defaults_use`
(each profile layer's own translation + own `USE=` joined into one combined
string, all layers concatenated, run through one shared incremental fold —
`config.py`'s `if curdb is configdict_defaults: continue` guard). A
consequence of that shared fold, verified live: a later layer's `USE="-*"`
wipes an earlier layer's USE_EXPAND-derived tokens too, even if that later
layer never touches the variable itself.

`source_incremental` (make.conf/package.use/environment) instead does an
explicit prefix-strip-then-replace when a layer explicitly assigns a
USE_EXPAND-listed variable — including an assignment to `""`, which is how a
user fully clears a group (needs `unset VAR` before sourcing, not `VAR=""`,
to tell "never touched" apart from "explicitly emptied").

See `use-expand-incremental-profile-stacking-bug` in the memory index for
the full before/after, the Opus second-opinion review that caught two real
gaps in the first draft, and the test names.
