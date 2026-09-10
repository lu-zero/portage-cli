# portage-resolve

Resolution-policy and plan layer for Gentoo Portage dependency resolution.

Turns repository facts + configuration into a solved, ordered merge plan on
top of the
[`portage-atom-pubgrub`](https://crates.io/crates/portage-atom-pubgrub)
solver bridge: USE/keyword/mask/license/properties/restrict policy,
root-aware post-solve trimming, and plan assembly. Used by the
[`em`](https://github.com/lu-zero/portage-cli) Portage CLI.

Computes policy; renders nothing (no usage-rs / anstream dependency — that
boundary is deliberate). CLI presentation lives in `portage-cli`.

## Status

Workspace-local (`publish = false`). Depends on `portage-repo` (brush git
fork), so it is unpublishable past the placeholder `v0.0.1` name reservation
on crates.io. Migrated out of `portage-cli`'s former `query/depgraph/*` in
staged landings (2026-07-15/16).

## License

MIT
