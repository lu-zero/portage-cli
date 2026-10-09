# A masked installed version was still a build candidate

Status: ✅ both fixes committed 2026-10-07, the warning 2026-10-09 on top
of the depgraph split. Cross-reviewed by a second model the day of the
fixes; its findings are folded in below.
Found while adding `em crossdev` version preferences
([[crossdev-gcc-version-flag]]). Two separate causes turned up; the
first note written here blamed only the first, and the crossdev symptom
that started it was actually the second.

## Reference: what real emerge does

Measured on this host with a scratch `PORTAGE_CONFIGROOT` (a copy of
`/etc/portage` plus one mask file; `emerge -p` needs no root).
`app-text/tree` installed at 2.2.1, tree has 2.1.1-r1 / 2.2.1 / 2.3.2,
`app-admin/pass` depends on it.

| Mask | Command | emerge |
|------|---------|--------|
| `>=app-text/tree-2.2.1` | `tree`, `-u tree`, `-uD tree`, `-n tree` | downgrade to 2.1.1-r1 |
| same | `pass` (tree as a dependency) | tree kept, "installed packages are masked" warning |
| same | `-uD pass` | tree downgraded |
| `app-text/tree` (all) | `tree` | error, all ebuilds masked |
| same | `-u tree` | nothing planned, warning |
| same | `pass`, `-uD pass` | tree kept, warning |

So: a masked installed version gives way whenever a visible one exists,
except as a dependency outside a deep update; with nothing visible it
stays, and emerge says so.

Before the fix `em` planned `R tree-2.2.1` for the bare atom, planned
nothing for `-u`/`-uD`/`-n`, kept tree under `-uD pass`, and never
warned. After it, all ten rows match (see "Display" below).

## Cause 1 — the solver could build the stub

`portage-atom-pubgrub/src/provider/`:

- `add_installed` inserts the installed version with empty dependencies
  when the policy layer did not offer it, so it can keep satisfying
  dependencies. Masked, keyword-rejected and ebuild-removed all look the
  same there.
- `choose_version` ended in a newest-in-range pick over everything in
  the map, stub included, and the held-back-target check
  (`validate.rs`) counted the stub as the "newest" a root could reach.

Fix: the stub is marked `installed_only`. Where an offered version is in
range, the stub is dropped from every pick that chooses something to
build, a root target no longer keeps it, and a rebuild no longer
reinstalls it. Dependencies outside a deep update still keep it. With
nothing offered in range it stays selectable, so nothing that resolved
before stops resolving.

## Cause 2 — `--prefix` ignored `package.mask` in the overlay

`portage-resolve/src/use_env.rs` read `package.accept_keywords`,
`package.use`, `package.env`, licenses, properties and restrict from the
prefix's config overlay, but `package.mask` and `package.unmask` only
from the main config dir. `em crossdev` under `--prefix` writes its
config to the overlay, so its masks were never seen and the installed
version was simply still visible. Fixed by reading both files from the
overlay as well. A real root (the crossdev-stages sandbox case) was not
affected.

## The warning

Not committed with the solver fix: it lives in `query/depgraph/`
(`mod.rs`, `prepare.rs`, `assemble.rs`, `output.rs`), on top of the
depgraph split that was still uncommitted work in progress, so it lands
with or after that refactor.

`em` now prints, after the plan, the installed packages it keeps although
config masks them, with the reason (`package.mask`, a keyword, …). Two
sources: a kept `installed_only` selection in the solution, and a
dropped dependency that an installed, masked version satisfies (the
all-versions-masked case, where the package never enters the solve). A
kept version whose ebuild is merely gone is not listed, as in emerge.

Review finding, fixed: the first version walked every dropped
dependency in the provider, which includes versions the solver never
picked and `||` branches with an available sibling, and matched by
package name only. It now takes only dependencies dropped from
*selected* versions with no available sibling, respects the slot, and
gets silently-kept root targets from target classification. `em -puD
@world` prints nothing with no masked package involved, like emerge.

## Verified

- Two solver tests (root target moves, with and without `--noreplace`;
  dependency kept unless deep-updating) — both failed before the fix.
- One config test for the overlay read.
- The ten-row emerge comparison above, run against the built `em`.
- `em crossdev --setup -p` with older gcc and glibc preferred, in a
  scratch prefix with newer versions installed, plans the downgrades with
  bare atoms. Run with the version flags that were dropped afterwards.
- Whole workspace: 2287 tests pass, fmt and clippy clean.

## Open

- **Display.** emerge marks a downgrade `UD`; `em`'s `action_tag` returns
  `D`, so the plan shows ` D`. The renderer already handles `UD`. Predates
  this change.
- **No test for the warning** itself; it was checked by running `em`.
- **Measured after review:** a masked `@world` member (`app-misc/tmux`)
  is downgraded by emerge under `-n`, `-u`, `-uD` and plain `@world`;
  `em` matches for the three it was run with (`-n`, `-u`, `-uD`).
- **Unmeasured, from review:** a dependency with USE drift under `-N`
  whose installed version is masked now moves to a visible version
  instead of rebuilding in place; and under a widened (autounmask) solve
  a masked installed root can still be kept when every visible version
  needs unmasking, as before the fix.
- **Not measured against emerge:** `-e`, `-N`/`-U` with a masked
  installed version, a keyword-rejected (not masked) installed version,
  multi-slot packages, and `@world` members whose ebuild left the tree.
- Nothing here has been exercised by a real merge, only `-p`.
