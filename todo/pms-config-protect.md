# CONFIG_PROTECT longest-prefix vs PMS (13.3.3)

Status: 🔴 not started. Related: [[pms-compliance]].

## PMS

A path is protected if some `CONFIG_PROTECT` directory is a prefix, unless
**any** `CONFIG_PROTECT_MASK` directory is also a prefix (ancestor mask
wins).

## What `em` does

`ConfigProtect::longest_match` (`ebuild.rs`): the longest matching prefix
wins, so `PROTECT=/etc/portage` + `MASK=/etc` still protects. That is
Portage `ConfigProtect`, not the PMS walk.

`/etc` is always added if missing (Portage `make.globals`). PMS-silent.

## How to attack

Confirm product intent: PMS-letter vs Portage-identical. A live canary is
easy (`CONFIG_PROTECT=/etc/portage`, `CONFIG_PROTECT_MASK=/etc`). If we
keep Portage, document it next to the other PMS-silent config items and
stop calling the walk PMS 13.3.3.
