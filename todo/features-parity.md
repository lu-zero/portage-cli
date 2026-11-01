# FEATURES parity

Status: 🟡 steps 1–3 done 2026-10-27 (see "Done"); ccache and the sandbox family are not implemented. Filed
2026-10-22 from [[portage-config-layering]] (decision D3).
Related: [[build-clean-env]] (the `keepwork`/`keeptemp`/`noclean` subset, done).

## Decision (Luca, 2026-10-22)

The ebuild environment's `FEATURES` lists what `em` provides, not make.globals' set: an
eclass testing `has network-sandbox ${FEATURES}` must not be told about a sandbox em does not
run. Matching Portage's set is this note's work, one feature at a time.

## Where em stands (grep of `features.contains(…)`, 2026-10-22)

- **Read from FEATURES:** `binpkg-request-signature`, `binpkg-signing`, `candy`, `ccache`,
  `keeptemp`, `keepwork`, `noclean`, `nostrip`, `test`, `test-fail-continue`; `sandbox` only in
  a test.
- **Make.globals defaults em has no FEATURES check for** (31): `assume-digests`,
  `binpkg-docompress`, `binpkg-dostrip`, `binpkg-logs`, `binpkg-multi-instance`,
  `buildpkg-live`, `compress-index`, `config-protect-if-modified`, `distlocks`, `ebuild-locks`,
  `fixlafiles`, `ipc-sandbox`, `merge-sync`, `merge-wait`, `multilib-strict`, `network-sandbox`,
  `news`, `parallel-fetch`, `pkgdir-index-trusted`, `pid-sandbox`, `preserve-libs`,
  `protect-owned`, `qa-unresolved-soname-deps`, `sandbox`, `strict`, `unknown-features-warn`,
  `unmerge-logs`, `unmerge-orphans`, `userfetch`, `userpriv`, `usersandbox`, `usersync`.

## Work

1. Sort the 31 into: behaviour em always has (no switch: likely `preserve-libs`,
   `binpkg-multi-instance`, the collision checks), behaviour em lacks (the sandbox family,
   `userpriv`/`userfetch`/`usersync`, `distlocks`), and features with no meaning for em. Check
   each against the code, not the name.
2. Export to the ebuild environment the ones em provides, always-on ones included.
3. For each always-on one, decide whether `-feature` in make.conf must switch it off, as in
   Portage.
4. `em --info`/`portageq envvar FEATURES` show Portage's stacked value (make.globals included)
   once [[portage-config-layering]] step 2 exists; the build environment shows em's. Say so in
   `--info` if the two differ.

## Done (2026-10-27)

What the tree tests (`has X ${FEATURES}` in ebuilds and eclasses): `userpriv` 43, `usersandbox` and
`network-sandbox` 27, `ccache` 26, `preserve-libs` 20, `test` 19, `distcc` 13, `installsources` 8, `noman` 4,
`unmerge-orphans` 2, `sfperms`, `sandbox`, `nodoc` 1.

A build's `FEATURES` is now `settings::provided_features`:
- Honoured switches the configuration sets (`package.env` included): `test`, `keepwork`, `keeptemp`, `noclean`,
  `nostrip`, `binpkg-signing`, `binpkg-request-signature`. (`candy` selects em's merge display and is
  not passed to builds; see below.)
- `preserve-libs` as Portage stacks it, so `-preserve-libs` now switches preservation off (Luca: "interesting to
  implement just to see how it is"). Without make.globals em's own globals layer is `FEATURES="preserve-libs"`.
- `binpkg-multi-instance`, always (every package gets a build id).
- `userpriv` when the build does not run as root; not under real or mapped root, as in crossdev-stages' box
  (Luca: em should not claim it there).
- Everything else is dropped: the sandbox family (em has none), `ccache` (no PATH/CCACHE_DIR setup), `distcc`,
  `installsources`, `splitdebug`, `noman`/`nodoc`/`noinfo`, `sfperms`, `test-fail-continue`.

Checked in a stage3 sandbox: a build saw `binpkg-multi-instance preserve-libs` (it had seen the config's
`parallel-install`); removing a needed library keeps it by default and deletes it with `FEATURES=-preserve-libs`.

Open: ccache support; whether `--info` should say that the build value differs from Portage's stacked one (step 4).

**`candy` (Luca, 2026-10-27):** Portage's is a scrolling-message spinner while the resolver runs; em's resolve is too
fast to need one. Instead, `FEATURES=candy` selects a richer build progress display: a sink on the activity bus
(next to `HumanStdoutSink`, see [[activity-status]]) drawing per-package/phase bars and the ETA from the duration
history. Landed 2026-11-01 with indicatif (`activity/candy.rs`, `console.rs`); live-checked in a sandbox.
- indicatif's `MultiProgress` can keep each running build's last output line under its bar, so parallel
  (`--jobs N`) builds stay readable; the mode works for both verbose and quiet runs.
- It may become the default display once it exists.

