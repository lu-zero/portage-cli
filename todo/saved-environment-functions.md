# Phases should run from the saved environment, functions included

Status: ✅ done 2026-10-09: the printer is fixed in the brush fork and a
binary install and an uninstall run the package's saved functions, as
Portage does. The ebuild remains the fallback and is still read for
the EAPI.

## What Portage does

Read in `ebuild.sh`, `phase-functions.sh` and `save-ebuild-env.sh` of
Portage 3.0.82, for behaviour only.

- After a phase, the whole shell state is written to `${T}/environment`:
  the variables and then every function, as bash prints them. Portage's
  own helper functions are removed first.
- A later phase does not source the ebuild again. If `${T}/environment`
  exists, it is filtered (variables that bash or Portage own are dropped)
  and sourced, and that is the package. The ebuild is sourced only for
  metadata, for the first phase of a source build, when the ebuild
  changed, or under `FEATURES=noauto`.
- A binary install and an uninstall start from the same file, unpacked
  from `environment.bz2`. Neither needs the ebuild or its eclasses to
  exist in the tree.

So in Portage the saved environment is the package's code as well as
its state. That is what lets an old installed package be removed after
its eclass has changed or gone.

## What `em` does

Phases of one build share a live shell, so nothing is reloaded between
them. Where state has to cross a boundary (compile parent to install
worker, binary install, uninstall), `em` sources the ebuild again for
the functions and puts the saved *variables* on top. The functions in
the saved environment are used only as a fallback at uninstall, when
the ebuild copy in the VDB is missing.

The stated reason, in a comment on `capture_variables`: brush's function
printer does not round-trip heredoc bodies.

## What is actually wrong with the printer

Measured 2026-10-09 with the fork at `3f1ab573`:

- Plain, quoted and `<<-` heredocs in a function print correctly and
  `bash -n` accepts the result. The fork already keeps a body out of
  the indentation and lets a redirect after the heredoc stay on the
  operator line.
- **A heredoc followed by a pipe is printed wrong.** For
  `sed … <<-EOF | newins - os-release` the printer writes the body
  and the delimiter right after the first command, and `| newins …`
  on the line after it, which does not parse. bash keeps `|` on the
  operator line, then the body, then the rest of the pipeline. The same
  must hold for `&&` and `||` after a heredoc.
- Corpus: the saved environments of the first 19 packages of a native
  toolchain bootstrap, 337 heredoc lines between them. 18 parse with
  `bash -n`; the one that does not is `sys-apps/baselayout`, through
  exactly that construct in `src_install`.

In the fork, `brush-parser/src/ast.rs` defers a heredoc body to the end
of the prefix or suffix list it sits in. It has to be deferred to the
end of the pipeline element's line instead: `Pipeline` and `AndOrList`
print their operators without knowing a body is pending.

## What a fix buys

- The saved environment becomes sourceable in every case, so the
  uninstall fallback stops failing on such packages.
- `em` could then do as Portage does for a binary install and an
  uninstall: load the saved environment and not need the ebuild. The
  two limits recorded in [[phase-order-and-binpkg]] (functions from the
  repository's ebuild, eclasses from the current tree) go away.
- The worker handoff could carry functions too, and drop its re-source.

## Order

1. Fix the printer in the fork, with the baselayout construct and the
   `&&`/`||` variants as tests; bump the `rev` here.
2. A test in `em` over a saved environment: every function printed must
   parse back.
3. Then decide whether binary install and uninstall switch to the saved
   functions, keeping the ebuild as the fallback.

## Progress (2026-10-09)

Step 1 is done in `~/Sources/brush`, branch `heredoc-line-print`, one
commit on top of the pinned `3f1ab573`: bodies are queued while an
and-or list is rendered and written after it. Four printer tests
(print, parse back, print again) and a compat case that prints a
function, re-evaluates it and runs it. Parser crate tests and the whole
compat suite against bash pass (2495 succeeded, 0 failed).

Landed on `for-portage-repo`, pushed, and the `rev` here bumped the same
day. Live: baselayout's saved environment, the one that failed, now
parses with `bash -n`. Step 2 is done: `the_saved_environment_can_be_sourced_again` builds a
package whose functions hold here-documents before a pipe and before
`||`, sources its saved environment in a fresh shell and runs them.
Step 3, whether binary installs and uninstalls switch to the saved
functions, is a decision and not taken.

## Step 3 done (2026-10-09, Luca: Portage's behaviour is the right one)

`restore_package_environment` sources the saved environment's functions
together with its shielded variables and does not source the ebuild.
For a binary install this happens when the package is unpacked, before
`pkg_pretend`: that phase's changes are still thrown away, but it sees
the package's environment, as in Portage. An earlier step here applied
the variables only before `pkg_setup`; that over-read "takes no part in
environment saving".

- `em`'s own shell helpers (`default`, `insinto`, `edo`, …) are in the
  saved environment too. `run_phase` defines them again on every phase,
  so the running `em`'s versions are the ones used.
- **Fallback.** A saved environment with no functions, or one the shell
  does not read back, gives way to the ebuild for the functions, with
  the saved variables on top. The shell reports a syntax error in a
  sourced file and still returns success, so the file ends with a marker
  assignment and its absence means "not read".
- **Still needs the ebuild file**: `run_phase` reads the EAPI from it and
  sets `EBUILD` and `FILESDIR`. Its eclasses are no longer needed.

Tests: a binary install runs the function the package was built with
after the tree's ebuild changed; an uninstall runs the function the
package was installed with after the VDB's ebuild copy changed; a saved
environment that does not parse falls back to the ebuild and keeps its
variables.

Live, sandbox `em-binpkg-stage`: the second root installed from the
packages built before the printer fix. 157 binary installs, both steps
exit 0, 141 packages with CONTENTS identical to the source-built root.
Three of those installs (baselayout's) hit the unparseable environment
and took the fallback; the first attempt, without the marker, skipped
baselayout's `pkg_preinst` and left a split-usr layout.
