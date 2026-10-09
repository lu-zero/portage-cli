# Tidy the build-and-merge path: four oversized functions, and the tests to do it safely

Status: 🟢 all four functions cut 2026-10-09, with tests. Two items left,
both under "Open" at the end: more of `run_phase`, and the missing-profile
policy, which is a decision. Decided with Luca: several
hundred lines in one function is a problem in itself, and every
phase-order defect in [[phase-order-and-binpkg]] sat in the seams
between these four.

## The functions

Sizes on branch `phase-order` (2026-10-08):

| Function | Where | Lines | Does |
|----------|-------|-------|------|
| `EbuildShell::run_phase` | `portage-repo/src/build/shell.rs` | 531 | sources the ebuild if needed, derives and exports the whole per-phase environment (directories, roots, `EPREFIX` flip, toolchain selection, `PATH`, `LD_LIBRARY_PATH`), then runs the phase function with its bashrc hooks |
| `run_inner` (since split, see below) | `portage-cli/src/ebuild/mod.rs` | 444 | opens the repo and shell, loads config, cleans the tree, extracts a binary package, restores a worker environment, loops over the phases with locking and activity events, post-processes, packages, dispatches elog, drops the tree |
| `walk_image` | same | 246 | lists the image, decides config protection, and (in merge mode) writes symlinks, directories, files and hardlinks |
| `run_merge` | same | 236 | `pkg_preinst`, collision check, copy, replaced-package removal, VDB registration, preserved libs, `pkg_postinst`, environment save |

## Cuts worth making

- **`run_phase`: environment setup apart from execution.** "Set this
  shell up for a build tree at `work_root` targeting `root`" becomes its
  own step; "run phase function X" uses it. Besides the size, this is
  what lets the merge learn `ED` before any phase has run, so the
  collision check can move ahead of `pkg_preinst` as Portage has it
  (open half of item 5 in [[phase-order-and-binpkg]]). The toolchain
  selection and the `EPREFIX` flip are separable blocks inside the
  setup.
- **`run_inner`: one function per stage.** Shell and config
  construction; tree preparation (clean, binary extract, worker-env
  restore); the phase loop; the epilogues. The loop then reads as the
  sequence the references describe, with packaging and post-processing
  as named steps rather than `if *phase == INSTALL` blocks.
- **`run_merge`: the merge as explicit steps.** Plan (scan, config
  protection, collisions), `pkg_preinst`, apply, remove replaced,
  register, `pkg_postinst`, save environment. Binary installs and
  source installs share it; `pkg_setup` for a binary install (item 2)
  gets an obvious place in front.
- **`walk_image`: plan and apply.** Today one loop does both, with a
  mode flag added on 2026-10-08. A plan value (entries with source,
  destination, kind, digest, protection decision) built once and then
  applied removes the second walk of the image and the flag.

## Are there tests to do this safely? Partly.

- **`run_phase`: reasonably covered.** `portage-repo/src/build/shell/tests.rs`
  has 55 tests, 31 of them driving `run_phase` on small synthetic
  ebuilds: working directory, bashrc hooks, `die`/`nonfatal`, install
  helpers, toolchain selection, `ESYSROOT` under prefix, `PATH`,
  `CONFIG_SITE`. A refactor of the setup has a real safety net. Gaps:
  nothing asserts `MERGE_TYPE`, `ED`/`EROOT` under a prefix, or that two
  consecutive phases see the same environment.
- **`walk_image`: well covered.** 13 unit tests (copy, symlink rewrite,
  prefix, config protection, read-only overwrite, mtimes, hardlinks,
  scan mode).
- **`run_merge`: not covered at all.** No test calls it. The order of
  `pkg_preinst`, collision check, copy, the replaced package's
  `pkg_prerm`/`pkg_postrm`, registration and `pkg_postinst` is asserted
  nowhere. The 2026-10-08 fixes were confirmed only by hand in a
  sandbox.
- **`run_inner`: not covered.** Only the `PhaseGroup` phase lists have
  tests (three). Packaging point, tree cleaning, worker-env restore,
  binary extraction: nothing.
- **Binary packages end to end: live scripts only.** `test-scripts/`
  has six scripts; they need `sudo`, a network fetch and a
  crossdev-stages sandbox, and run by hand, not in CI.

So the two functions that most need restructuring are the two with no
tests.

## Tests to write first

A merge-level harness, unprivileged and offline, in the style of the
shell tests: a temporary repository with a synthetic ebuild that needs
no compiler (its phases write a file and append their name to a log),
merged into a temporary `--root`. Then assert, per case:

1. Source install: the phase log reads `setup … install, preinst,
   postinst`; the file is in the root; CONTENTS lists it.
2. Replacement: the old version's `prerm` and `postrm` fall between the
   new `preinst` and `postinst`; `REPLACING_VERSIONS` and
   `REPLACED_BY_VERSION` have the right values.
3. `-b` then `-k` into a second root: same CONTENTS; the package holds
   the image as `src_install` left it even when `pkg_preinst` deletes a
   file from it.
4. `-B`: a package and no change to the root.
5. Collision with another owner: the merge fails and the other owner's
   file is byte-identical.
6. `MERGE_TYPE` is `source`, `binary`, `buildonly` in the three modes.
7. A variable set in `pkg_postinst` is visible in a later `pkg_postrm`.

### What the harness runs against (decided 2026-10-08, Luca)

The merge path must not run without a real setup, but the setup can be
a populated struct: production fills it from disk, a test fills it by
hand. No "test mode" that skips configuration.

What `run_inner` reads before its first phase, all inline in its first
~200 lines (read 2026-10-08):

| Input | Read from |
|-------|-----------|
| repository and ebuild | the ebuild's path, or `repo_override` |
| masters | `repos.conf` under the config root and the prefix overlay |
| profile variables, effective USE | `make.profile` chain, `etc/portage/profile`, `make.conf` (`apply_profile_env`) |
| bashrc hooks | each profile's `profile.bashrc`, `etc/portage/bashrc`, the overlay's |
| per-package environment | `package.env` and `env/` |
| `LD_LIBRARY_PATH`, `env.d` | the prefix's and host's `ld.so.conf`, `BROOT`'s `env.d` |
| IUSE defaults | the repository's metadata cache entry |
| `REPLACING_VERSIONS` | the VDB of the target root |
| `FEATURES` | read back from the shell once the above is sourced |

So the struct is the result of that block: repository and masters, the
ebuild, the configured shell, the feature set, the work root and the
roots. `run_inner` becomes "load the setup" followed by "run this group
on a setup", and the second half is what tests call.

**Names and the first cut** (applied 2026-10-08; Luca: `run_inner` is
the wrong name). It ran one `PhaseGroup` for one package, so the entry
point is now `run_phase_group(PhaseGroupRun)`, and it is two calls:
`PackageSetup::load(&opts)` (about 200 lines, everything in the table
above) and `setup.run_group(opts)` (about 280, tree preparation, the
phase loop, the epilogues). Code moved, not rewritten; `MERGE_TYPE` and
`REPLACING_VERSIONS` went to `run_group` because they depend on the
group. Both halves still take the whole options struct; narrow that
when the harness shows what a hand-filled run needs. Checked live:
baselayout built and merged into a scratch `--prefix`, 39 CONTENTS
entries as before.

Two things follow:

- **`run_merge` already has this shape.** It takes a configured shell,
  the ebuild, the work root and the root, and reads nothing else but the
  VDB. Cases 1, 2, 5 and 7 can be written today with `repo.shell()` on a
  temporary repository, as the shell tests do, with no change to the
  code. Cases 3, 4 and 6 go through `run_inner` and need the split.
- **A missing profile is accepted today.** `apply_profile_env` returns
  `false` and `run_inner` warns "building without profile defaults" and
  carries on. Under the rule above that is an error in the loader. Not
  changed yet: check who relies on it first (`em ebuild` on a bare
  overlay, the self-contained toolchain bootstrap).

Open: whether the hand-filled part is the shell itself or a plain value
(variables, USE, bashrc list, features) applied to a shell. The profile
is sourced by bash, so a plain value means capturing the result of that
sourcing; the shell is the cheaper carrier and what the 55 shell tests
already populate.

## Order

1. Cases 1, 2, 5 and 7 against `run_merge` as it is (5 and 7 pass only
   on branch `phase-order`).
2. `run_merge` into steps; `walk_image` into plan/apply.
3. `run_inner` into "load the setup" and "run a group on it" (done),
   then `run_group` into its stages; cases 3, 4 and 6 on `run_group`.
4. `run_phase` setup/execution split, then move the collision check.
5. Items 2, 3 and 7 of [[phase-order-and-binpkg]] onto the new shape.

## Progress (2026-10-09)

- **Harness.** `Probe` in `ebuild/mod.rs` tests: a scratch repository,
  work area and root, real phases through `run_one_phase`, whole groups
  through `PackageSetup::run_group` on a setup filled in by hand. Seven
  tests: `pkg_pretend` isolation, merge order, replacement order with
  `REPLACED_BY_VERSION` and a value kept from `pkg_postinst` to
  `pkg_postrm`, collision abort before `pkg_preinst`, build with a
  package then install from it (with and without a prefix, `MERGE_TYPE`
  and restored variables included), build-only. Cases 1 to 7 above are
  all covered except `REPLACING_VERSIONS`.
  The harness found two defects the hand checks had missed: the saved
  environment was not read back at uninstall, and a binary install
  under a prefix installed nothing. Both fixed, see
  [[phase-order-and-binpkg]].
- **`run_merge`: 245 → 176 lines.** `check_collisions`,
  `register_merged`, `report_protected` are functions; the body reads as
  the sequence. The preserved-libs bookkeeping is still inline.
- **`run_group`: 280 → 191 lines**, most of it the phase loop.
  `clean_stale_tree`, `extract_binpkg`, `restore_worker_env`,
  `preset_replacing_versions`, `drop_build_tree` are functions.
- **`run_phase`: one cut.** `effective_eprefix`, `ed_under` and the
  public `image_ed` came out, which was what the collision check
  needed. The function is still about 500 lines.

- **`walk_image`: 246 → 64 lines.** The loop is the traversal; each
  entry goes to `ImagePass`, with one method per kind (`dir`, `symlink`,
  `file`) and `install_file`. Not the plan/apply split noted above: the
  image is listed before `pkg_preinst` and merged after it, so a plan
  cannot be reused, and the second walk stays. Live: baselayout's
  CONTENTS identical before and after.
- **`run_phase`: 531 → 441 lines.** `root_vars` builds the `RootVars`
  value its own comment asked for. Live: the eight root variables of a
  prefix build are unchanged.
- **`run_group` takes a `GroupRun`**, the nine fields it uses, not the
  whole `PhaseGroupRun`.
- **`REPLACING_VERSIONS`** has its test.

Sizes now: `run_phase` 441, `PackageSetup::load` 196, `run_group` 191,
`run_merge` 176, `walk_image` 64.

Open:

- **More of `run_phase`.** Toolchain selection, `PATH` and helper
  shims, and the sourcing with its `E_*` accumulation are separable
  blocks. 55 shell tests cover them.
- **A missing profile: stays a warning** (Luca, 2026-10-09), as in the
  merge driver, `em info`, config protection and `em use`. Note that
  this is *not* what emerge does: `_emerge/main.py::profile_check`
  prints "Your current profile is invalid" and returns 1 for every
  action but help, info, search, sync and version. Luca assumed emerge
  warned; revisit if that changes the decision.
