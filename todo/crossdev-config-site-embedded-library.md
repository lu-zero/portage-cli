# `em crossdev` should generate the config.site cache-answer library itself

Status: ✅ the install-it-as-a-package mechanism below is real and
correct for `cross-*`-category packages built during `crossdev --setup`
(their own `--prefix` argument is EPREFIX-anchored, so autoconf's
auto-search finds it). It did **not** cover native packages cross-built
during `stages --stage1` (board-destined, correctly `EPREFIX`-less) —
every verification of that case, including this file's own
"Live-verified" note below, ran in a reused sandbox whose bare host
happened to already have `config.site` from unrelated prior testing,
masking the gap. Fixed for real 2026-08-29:
[[crossdev-config-site-not-found-by-board-packages]]. Found 2026-08-26
hand-driving `em crossdev --setup` in a completely fresh crossdev-stages
sandbox (no `sys-devel/crossdev` installed on the host). Landed
2026-08-28.

## What actually shipped

The original "Direction" below (embed a hand-written loader/selector
plus a hand-derived answer library) turned out to be the wrong call: it
briefly landed as `config_site_entries` generating `LOADER`/
`CROSSDEV_SELECTOR` as embedded Rust string literals, closely following
real crossdev's own (GPL-2) `wrappers/site/config.site` — a licensing
problem in this MIT-licensed tree, caught and rewritten independently
the same day (see AGENTS.md's "never embed a real shell script" rule).

While chasing the remaining gap (the per-target cache-answer library
itself, `ac_cv_file__dev_ptmx`-style), the simpler fix became obvious:
`sys-apps/config-site` (the loader) and `sys-devel/crossdev` (the
selector plus the full answer library) are ordinary Gentoo packages.
`init_target` now just ensures both are merged into the host root
(`ensure_config_site_packages`, `crate::emerge_atoms`) before laying
down the rest of the target config — real crossdev's own install phase
writes `/usr/share/config.site`, `config.site.d/80crossdev.conf`, and
every `/usr/share/crossdev/include/site/*` answer file itself. No
loader/selector/answer text lives in `em`'s own source at all anymore;
`config_site_entries`/`pick_config_site_library`/`LINUX_SITE_ANSWERS`
are gone.

Live-verified on the em-i586-check crossdev-stages sandbox: a fresh
`em --target i586-pc-linux-gnu crossdev --init-target` merges both
packages, and the previously-blocked `dev-lang/python` cross-configure
(dying on `ac_cv_file__dev_ptmx`) now builds and installs cleanly.
