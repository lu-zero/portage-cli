# `installed_missing_from_repo` over-updates a revbumped installed dep

STATUS: FIXED 2026-06-18. Removed the `installed_missing_from_repo` field
entirely; under `Favor` `choose_version` now keeps any satisfying installed
version regardless of whether its exact cpv is in the repo. `em -p firefox`
reaches **0 diffs** vs emerge (was 126 vs 125). Collateral improvement:
`em -p app-office/libreoffice` went from **7 diffs** (over-pull) to **3 diffs**
(a separate, pre-existing w3m/boehm-gc gap — see
`nonemptytree-bdeps-gap.md`). `cargo test` (137), clippy, fmt clean.

Discovered 2026-06-18 (sandbox aarch64). The lone remaining `em -p firefox` vs
`emerge -p firefox` difference (126 vs 125): em lists `dev-build/cmake-4.3.3-r1
[4.3.3]` (`U`) where emerge keeps the installed `cmake-4.3.3` unlisted.

## Mechanism

- `dev-build/meson` **DEPENDs** on `dev-build/cmake` (plain, no version). DEPEND
  resolves on SYSROOT, so it is *not* broot-filtered (unlike the many BDEPEND
  `>=cmake-3.28.5` edges, which the host's 4.3.3 satisfies and broot_filtered
  drops).
- The installed `cmake-4.3.3` is **not** a repo version — the tree only has
  `cmake-4.3.3-r1` (the revbump superseded `-r0`). So `add_installed` flags it
  `installed_missing_from_repo`.
- `choose_version`'s `Favor` arm
  (`portage-atom-pubgrub/src/provider/solve.rs:87-100`) skips the installed
  version when it is `installed_missing_from_repo` and falls through to the
  newest repo version (`4.3.3-r1`) — "update-on-prune". So em updates cmake.

## Why it's wrong here

`emerge -p firefox` (no `-u`) keeps the installed `cmake-4.3.3`: it satisfies
meson's plain `dev-build/cmake` and a revbump is not pulled without `--update`.
"update-on-prune" is genuinely `-u`/deep behaviour; under `Favor` (non-update)
an installed version that satisfies the dep should be kept even when its exact
cpv is pruned from the tree (the added empty-deps installed stub is fine — the
package is satisfying a dep, not being rebuilt).

`em -pe firefox` is unaffected (383 == emerge): emptytree rebuilds cmake either
way, so the set still matches.

## Fix direction

Under `Favor`, keep the installed-satisfying version regardless of
`installed_missing_from_repo`; reserve the fall-through-to-newest for update /
deep / emptytree (`Rebuild`) modes. Check the existing tests that pin
update-on-prune (search `installed_missing_from_repo`) and re-verify firefox
`-p` (-> 125) and `-pe` (383) plus the suite.
