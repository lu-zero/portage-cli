# Phases should run from the saved environment, functions included

Status: 🟡 printer fixed in the brush fork 2026-10-09 (`e80dfeaf` on
`for-portage-repo`, pushed) and `em` moved to it; step 2 done, step 3 a decision. A parser
defect found on the way is open: [[brush-heredoc-line-continuation]]. Follows from item 3 of
[[phase-order-and-binpkg]].

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
