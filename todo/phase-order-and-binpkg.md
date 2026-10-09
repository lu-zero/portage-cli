# Phase order around install, packaging and merge

Status: ✅ audit done 2026-10-08; every item fixed 2026-10-08/09 and
covered by tests in `portage-cli/src/ebuild/mod.rs` (`Probe`). The two
limits of item 3 are stated under "Progress (2026-10-09)". Started from
one failure (a binary package of `sys-apps/baselayout` that cannot be
installed) and widened to how `em` orders the phases against PMS and
Portage 3.0.82.

Sources read: PMS (`~/Sources/pms-git`, 2026-07-19): "Ebuild-defined
functions" incl. "Call order", "The state of variables between
functions", `MERGE_TYPE`, "Merging and unmerging". Portage:
`doebuild.py`, `_emerge/EbuildBuild.py`, `_emerge/Binpkg.py`,
`dbapi/vartree.py::treewalk`, read for behaviour only. `em`:
`portage-cli/src/ebuild/mod.rs` (`PhaseGroup`, `run_merge`,
`walk_image`, `merge_binpkg`), `ebuild/binpkg.rs`.

## What the references say

PMS call order, install: `pkg_setup`, `src_*`, `src_install`,
`pkg_preinst`, `pkg_postinst`. Replacing: after `pkg_preinst` come the
old package's `pkg_prerm` and `pkg_postrm`, then `pkg_postinst`.
`pkg_pretend` is outside the sequence and takes no part in environment
saving. Binary packages are out of PMS's scope except for two
sentences: installing one skips only the `src_*` phases, and building
one that is not installed locally does not call `pkg_preinst` /
`pkg_postinst`. `pkg_preinst` may write to `D` as well as `ROOT`.
`MERGE_TYPE` is `source`, `binary` or `buildonly`. A variable set in an
earlier function must keep its value in later ones, including a later
uninstall.

Portage's order, source build with `-b`: phases through `install`, the
image post-processing that follows it, then **package**, then the merge.
Inside the merge: collision check on the image, `pkg_preinst`, copy the
image, remove the replaced instances (`pkg_prerm`, files, `pkg_postrm`),
`pkg_postinst`. Binary install: unpack metadata, run `setup` from the
package's saved environment, unpack the image, then the same merge.
`pkg_pretend` runs for every package before the first build.

## Where `em` differs

1. **Binary package is built after the merge.** `run_inner` packs when
   the phase loop, which ends in `qmerge`, is over; `build_binpkg`
   requires the VDB entry to exist. The image has been through
   `pkg_preinst` by then. baselayout's `pkg_preinst` runs a Makefile
   shipped in the image and then deletes it, so its package has no
   Makefile and installing it fails (`No rule to make target
   'layout-usrmerge'`). Confirmed: 0 `Makefile` entries in the package.
   Affects any ebuild whose `pkg_preinst` changes `D`.
2. **Binary install runs no `pkg_setup`.** `PhaseGroup::BinpkgMerge` is
   `[Qmerge]`, i.e. `pkg_preinst` and `pkg_postinst` only. PMS skips
   only the `src_*` phases. Ebuilds that create users, check the kernel
   or set variables for `pkg_preinst`/`pkg_postinst` in `pkg_setup`
   lose that.
3. **Binary install sources the repository's ebuild**, not the
   environment saved when the package was built (`merge_binpkg`'s own
   doc comment). Values set during the build are gone, and the ebuild
   in the tree may be a different revision or missing. Not traced
   further than that comment.
4. **`MERGE_TYPE` is always `source`** (`portage-repo/src/build/shell.rs`,
   two sites). Never `binary` for a binary install nor `buildonly` for
   `-B`. Ebuilds branch on it, typically to skip build-only checks in
   `pkg_pretend`/`pkg_setup`.
5. **Collisions are detected after the files are written.**
   `walk_image` copies the image into the root while it builds the
   contents list; `find_collisions` runs on that list afterwards and
   then aborts. By then another package's files are overwritten.
   Portage checks first, and before `pkg_preinst`, so files an ebuild
   creates in `pkg_preinst` are not collision-checked there (baselayout
   relies on this for its `/var/lock`, `/var/run` links).
6. **The saved environment predates `pkg_preinst`.** `run_merge` captures
   it first and writes that copy to the VDB. Anything `pkg_preinst` or
   `pkg_postinst` sets is not there for a later `pkg_prerm`/`pkg_postrm`,
   which PMS requires.
7. **`pkg_pretend` is part of the per-package sequence**, run in the
   same shell just before `pkg_setup`. Its variables can carry into
   later phases, which PMS rules out, and a failing check is only seen
   when the build reaches that package. A binary install runs none.

Matching the references: `src_install` → image post-processing
(`post_process_after_install`) happens before anything else; the
replaced package's `pkg_prerm`/`pkg_postrm` sit between `pkg_preinst`
and `pkg_postinst`; `REPLACING_VERSIONS` and `REPLACED_BY_VERSION` are
set; `-B` calls neither `pkg_preinst` nor `pkg_postinst`.

## How much of this PMS actually requires (rechecked 2026-10-08)

PMS lists "Binary packages" among the items it does not specify
("Unspecified items" appendix), and says nothing at all about file
collisions. So only part of the list is a specification matter:

| # | Item | PMS |
|---|------|-----|
| 1 | package built after the merge | **Implied.** Two sentences in "Call order": a binary install skips only `src_*`, and a package built but not installed locally runs no `pkg_preinst`. So `pkg_preinst` runs at install time and must find the image as `src_install` left it. The packaging point itself is unspecified. |
| 2 | no `pkg_setup` on binary install | **Implied.** Same "skips only `src_*`" sentence; and `DEPEND` "may not be installed at all if a binary package is being merged", which only matters if `pkg_setup` runs then. |
| 3 | repository ebuild instead of saved environment | **Required in substance.** "The state of variables between functions": a value set earlier in the sequence must be preserved for later functions. For a binary install the earlier functions ran at build time. How it is stored is unspecified. |
| 4 | `MERGE_TYPE` always `source` | **Required**, EAPI 4 and later: `binary` when installing a binary package, `buildonly` when building one without installing. |
| 5 | collision check after the copy | **Not in PMS.** Portage behaviour, and plain safety. The only related PMS rule is on ebuilds: no merging a file over a non-file. |
| 6 | saved environment predates `pkg_preinst` | **Required.** Same section as 3: preserved "including functions executed as part of a later uninstall". |
| 7 | `pkg_pretend` in the sequence | **Half.** Required: it "does not participate in any kind of environment saving". Not required: when it runs ("some unspecified time before"); running all of them first is Portage's choice. |

Two corrections to the reading above. `D` is listed as legal in `src_*`
only, while `ED` is legal in `src_*` and `pkg_preinst`; the `pkg_preinst`
text nevertheless allows writing under `D`. And the temporary directory
`T` only has to stay consistent within one connected install or
uninstall sequence, so nothing requires it to survive from the build to
a later binary install.

## Shape of a fix

The merge is one opaque `Qmerge` step today. Split the sequence so each
reference step has a place:

```
source:  pretend* | setup, src_*, install, post-process
         → package (if -b)           image as src_install left it
         → collision check
         → preinst → copy → remove replaced → register → postinst
         → save environment
binary:  pretend* | unpack metadata + saved env → setup (MERGE_TYPE=binary)
         → unpack image → same merge as above
```

- Packing before the merge already exists for `-B`
  (`build_binpkg_standalone`, scratch contents and metadata). `-b` can
  use it and drop the "VDB entry must exist" variant.
- `walk_image` needs a planning pass (contents, config protection,
  collisions) separate from the copying pass.
- `pretend*`: decide whether to hoist it before the first build as
  emerge does; at minimum run it in a shell that is thrown away.

## Not checked

- Whether `em`'s GPKG carries an `environment.bz2` usable for item 3.
- Per-phase sandbox/permission rules (`pkg_pretend` and `pkg_nofetch`
  must not write; `pkg_preinst` limited to `ROOT` and `D`).
- `pkg_config`, `pkg_info`, `pkg_nofetch` call paths.
- The `__worker` split (`Compile` / `Install` groups): the same
  ordering applies but the packaging point crosses a process boundary.
- Any of the above against a live emerge run; this is from reading.

## Progress (2026-10-08)

Numbering as in "Where `em` differs". Done on branch `phase-order`:

- **1 — package before the merge.** Packing moved to right after
  `src_install` and the image post-processing, for `-b` and `-B` alike,
  through the one remaining packager (`build_binpkg_standalone`); the
  variant that needed the VDB entry is gone. Live: baselayout built with
  `-b` now ships its Makefile, installs from the package into a second
  root, and both roots have identical CONTENTS paths (39 entries).
- **5 — collisions before any write.** `walk_image` takes a mode: a scan
  that lists what a merge would install and touches nothing, or the
  merge itself. The collision check runs on the scan. Live A/B with a
  staged collision: the old binary overwrote the other owner's file and
  then aborted; the new one aborts with the file intact.
  **Still differs from Portage:** the check runs after `pkg_preinst`, not
  before it. Before any phase has run, the shell does not know `ED` for
  a binary package (it is derived inside `run_phase`), so a scan there
  listed nothing. Checking first needs the per-phase environment setup
  in `portage-repo` separated from running the phase function.
- **4 — `MERGE_TYPE`.** A typed `MergeType` on the build shell, set per
  operation. Live: `source`, `binary` and `buildonly` each seen in the
  saved environment of the matching run.
- **6 — saved environment.** Re-captured and rewritten after
  `pkg_postinst`. Live: the saved copy now records the `postinst` phase.

The packager also no longer copies the whole image into a scratch
directory to compute CONTENTS; it scans it.

Checks: one new unit test (a scan lists what a merge installs and writes
nothing); workspace 2264 tests, clippy, fmt, rustdoc clean.

## Progress (2026-10-09)

- **2 — `pkg_setup` on a binary install.** The binary group is now
  `pretend, setup, qmerge`. Live with a probe ebuild that logs each
  phase: `pretend, setup, preinst, postinst`, all with
  `MERGE_TYPE=binary`.
- **3 — the package's saved state.** Before `pkg_setup` the ebuild is
  sourced for its functions and the package's saved variables are put
  on top, as the install worker already does across its process
  boundary. Live: a plain variable set in `src_install` on the build
  side is seen by `pkg_setup`, `pkg_preinst` and `pkg_postinst` of the
  binary install.
  What is restored: variables that are not exported, not read-only, and
  not already set by the installing system's configuration. Limits:
  - **Functions still come from the repository's ebuild.** The saved
    function bodies are not used because a printed body containing a
    heredoc does not parse back (same reason the worker handoff carries
    variables only). A package whose ebuild has left the tree, or
    changed, is therefore still not installed from its own code.
  - **Exported variables are not restored.** They cannot be told apart
    from the build process's own environment, which the dump also
    holds (`LD_PRELOAD` of the fake-root library, for one). `pkg_setup`
    running again covers the usual exported ones (`PYTHON`, …); a
    variable exported in a `src_*` phase and read in `pkg_postinst` is
    lost.
- **7 — `pkg_pretend`.** Its shell state is thrown away when it returns,
  for ebuilds that define it; others keep the single sourcing. A binary
  install now runs it too, before the saved variables are applied. It
  still runs per package, just before `pkg_setup`, not for the whole
  plan up front as emerge does; PMS leaves the time open.
- **`-B` leaving `lib lib64 usr var` in an empty root: not an `em`
  defect.** baselayout's own `pkg_setup` writes the library layout into
  `EROOT`, and `-B` runs `pkg_setup`. Portage does the same. A probe
  ebuild built with `-B` leaves only `em`'s own `var/` state.

- **6, second half — the saved state at uninstall.** Saving the
  environment after `pkg_postinst` was not enough: `pkg_prerm` and
  `pkg_postrm` source the ebuild copy kept in the VDB and only fell back
  to the saved environment when that copy was missing, so a value set
  at install time never reached them. Found by the first merge-level
  test. The saved variables now go on top of the sourced ebuild there
  too.

Found on the way, fixed 2026-10-09:

- **The saved environment held `em`'s own process environment**
  (`LD_PRELOAD` of the fake-root library, `SSH_AUTH_SOCK`, …), in the
  VDB and in every binary package. An exported variable that still has
  the value `em` was started with is no longer saved; one the build
  changed is. Live: 152 exported entries before, 140 after, `LD_PRELOAD`
  gone, `PATH`/`HOME`/`CFLAGS` kept.
- **A flaky test**, `setup::host_tools::…resolve_takes_the_first_extra_path_hit_that_behaves`:
  the fake tool it writes could be busy (ETXTBSY) when run, because
  another test's fork still held it open. The helper now waits that out.
  Five full runs clean.

- **5, second half — collisions before `pkg_preinst`.** Done without
  waiting for the whole `run_phase` split: the derivation of the
  effective `EPREFIX` and of `ED` moved out of `run_phase` into
  `EbuildShell::image_ed`, which needs no phase to have run. The scan
  and the check now come first; an abort leaves the root untouched and
  `pkg_preinst` unrun, and what `pkg_preinst` adds to the image is no
  longer checked, as in Portage.
- **A binary install under a prefix installed nothing** (found while
  checking the above live). A package holds `ED`; it was unpacked into
  the bare image directory while the merge reads `image/EPREFIX`. Empty
  CONTENTS, no files, exit 0. Now unpacked into `ED`. Test: the
  build-then-install case runs with and without a prefix.
