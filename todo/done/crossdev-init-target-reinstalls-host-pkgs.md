# crossdev: `--init-target` reinstalls config-site + crossdev every run

STATUS: ✅ fixed 2026-09-27 — requested with noreplace; live-verified on an installed and a fresh host.

`init_target` (`portage-cli/src/crossdev/mod.rs`) calls
`ensure_config_site_packages`, which runs `emerge_atoms` for
`sys-apps/config-site` and `sys-devel/crossdev` with no noreplace. Explicitly
named atoms get rebuilt like a bare `emerge <atom>`, so every `--init-target`
or `--setup` shows `[ebuild   R    ]` for both and merges them again on the host.

Likely fix: request them with `--noreplace` semantics (install only when
missing). Check whether anything relies on the reinstall, e.g. refreshing
config-site's files after an update, before changing it.
