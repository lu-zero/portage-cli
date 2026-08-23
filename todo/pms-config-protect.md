# CONFIG_PROTECT longest-prefix vs PMS (13.3.3)

Status: ✅ decided 2026-08-23 (Luca) — keep Portage-identical behavior,
documented as a deliberate divergence. Related: [[pms-compliance]].

## PMS

A path is protected if some `CONFIG_PROTECT` directory is a prefix, unless
**any** `CONFIG_PROTECT_MASK` directory is also a prefix (ancestor mask
wins).

## What `em` does

`ConfigProtect::longest_match` (`ebuild.rs`): the longest matching prefix
wins, so `PROTECT=/etc/portage` + `MASK=/etc` still protects. That is
Portage `ConfigProtect`, not the PMS walk.

`/etc` is always added if missing (Portage `make.globals`). PMS-silent.

## Resolution (2026-08-23)

Kept Portage-identical: `ConfigProtect::longest_match`
(`portage-cli/src/ebuild.rs`) and its quickpkg twin
(`portage-cli/src/quickpkg.rs`) now carry a doc comment stating this is
deliberately real-`emerge` behavior, not the PMS-letter ancestor-mask walk.
No behavior change — documentation only, so this stops being listed as a
PMS 13.3.3 gap in `pms-compliance`.
