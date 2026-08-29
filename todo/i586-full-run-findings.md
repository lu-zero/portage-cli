# Findings from the first full i586 crossdev --ex-pkg end-to-end run

Status: 🟡 parent feature ([[autounmask-cascading-fresh-slot-vs-version-pin]])
verified working 2026-08-25; this file tracks the residual bugs the run
exposed. Sandbox: `pilot-i586-em`, real (non-pretend) merges.

## What worked

- Full `em crossdev --target i586-pc-linux-gnu --setup`: all 6 steps
  (baselayout → binutils → gcc-stage1 → kernel headers → libc → gcc-stage2)
  built and activated for real after `b8c7ea9`'s reorder.
- `--ex-pkg sys-devel/clang-crossdev-wrappers` via the alias: widened solve,
  auto-persisted slot-scoped accepts, and the wrapper package itself merged
  (`clang-crossdev-wrappers-23` in VDB, 32 `<tuple>-*` wrappers installed).
  Result: **38 ok / 8 failed of 46** with all 8 failures accounted below —
  none in the widened-resolution machinery itself.

## Failure class 1 — brush: `declare -I` not implemented

`dev-vcs/git-2.55.0` src_install dies:

```
error: local: not yet implemented: declare -I
...
die: doman: at least one argument required
```

The unimplemented `declare -I` leaves a list variable empty, so git's ebuild
then calls `doman` with nothing. Real bash supports `declare -I`
(detect-and-skip-inherit for local var declaration); brush needs it, and
until then any ebuild using it fails at install. The doman die itself is
portage-faithful behavior — root cause is the declare.

## Failure class 2 — live-git `.9999` ebuilds need a working git

7 packages (`llvm-core/{llvm-23.1.0.9999, llvm-24.0.0.9999, lld-23.1.0.9999,
clang-23.1.0.9999}`, `llvm-runtimes/{openmp-24.0.0.9999, compiler-rt-
23.1.0.9999, compiler-rt-sanitizers-23.1.0.9999}`) die at src_unpack with
"Unable to fetch from any of EGIT_REPO_URI". Two contributing factors:

1. dev-vcs/git failed (class 1), so no `git` binary inside the sandbox
   ("command not found: git" appears in the logs).
2. Even with git present, these fetch the huge llvm-project monorepo —
   needs a network git fetch policy decision for sandboxes (shallow clone?
   mirror? host-bind the repo?).

Note the tiering interaction: these slots resolved to tagged-live picks
because no accepted/tagged-release candidate satisfied their exact slot
ranges. Once git works they should build like any live ebuild.

## Upstream distfile rot (not an em bug)

`libjpeg8_8d-2.debian.tar.gz` (libjpeg-turbo 3.1.3/3.1.4.1 SRC_URI) is gone
from every Gentoo mirror AND from Debian's pool; only Debian's snapshot
service still has it. Recovered manually into the sandbox DISTDIR from
`snapshot.debian.org/archive/debian/20131204T043919Z/...` (size matches
Manifest). Worth knowing for sandbox provisioning: consider pre-seeding
distfiles or an em-side "fetch from snapshot.debian.org as last resort"
mirror fallback idea.

## RESOLVED 2026-08-25 (later session): the `.9999` mystery + remaining failures

The 7 live-git failures had TWO stacked causes, neither the resolver:

1. **Stale config grants**: the pilot sandbox carried pre-existing
   `package.accept_keywords` entries (`=llvm-core/clang-23.1.0.9999:23 **`
   etc., left by an earlier portage A/B session) that explicitly ACCEPTED
   the live ebuilds — accepted-set newest-wins then preferred them over
   `_rc3`. accept_keywords can only grant; exclusion needs
   `package.mask`. Masking the three live atoms yields an all-`23.1.0_rc3`
   plan in one pass (llvm/lld/clang/compiler-rt/openmp/sanitizers), all
   **built and merged for real** — zero `.9999` needed.
2. ~~git~~: `declare -I` implemented in brush (both on `for-portage-repo`
   and on a `fix/declare-inherit-I` worktree atop origin/main); not
   exercised by the final run since the rc3 chain needs no git fetches.

Widened persistence now writes **live-bounded** slot grants
(`<llvm-core/llvm-23.1.0.9999:24 **`) so accepting a slot never invites a
live pick later; unbounded only when the selection itself is live
(`Version::is_live()` added to portage-atom).

## FIXED: cross activation clobbered host-wide `gentoo-gcc-install.cfg`

`select/compiler.rs::sync_foreign_config` rewrote the GLOBAL
`/etc/clang/gentoo-gcc-install.cfg` on EVERY compiler activation —
including cross ones. After i586 gcc-stage2 activated, the file pointed
every clang in the sandbox (host aarch64 builds included) at
`/usr/lib/gcc/i586-pc-linux-gnu/16`, failing all host links with "file in
wrong format" (reproduced with a bare `aarch64-...-clang t.c`, outside
em).

**Fixed** following the upstream pattern (`clang-crossdev-wrappers` +
real crossdev's LLVM setup): foreign activations now populate the
per-target `/etc/clang/cross/<chost>.cfg`
(`--gcc-install-dir=<cross gcc>` + `--target=<chost>`) and never touch
the host-global file; only native activations (or an undeterminable host
CHOST, legacy fallback) rewrite it. Live-verified in `pilot-i586-em`:
i586 activation leaves the global cfg on the host gcc and writes the
cross cfg; native activation still updates the global; host clang links
again (LINK_OK).

## Design gap: dropped hard deps get exact-pin writes instead of widened picks

Tracked as its own item with the agreed fix direction:
[[widening-on-dropped-hard-deps]]. Recap of the two paths that compose
badly when starting from an empty grant set:

1. **Widened selections** (solve fails → phase 2): slot-scoped,
   live-bounded grants — the good shape (`<llvm-core/llvm-23.1.0.9999:23
   **`).
2. **DroppedDep advisories** (solve SUCCEEDS but a hard dep had zero
   accepted candidates and was dropped): portage-parity exact pins for
   *every* filtered version matching the range — e.g. all six
   clang/lld 23.x/24.x versions including `.9999`.

Since a solve with dropped deps doesn't fail, phase 2 never runs, and
`--autounmask-write` (implied on real runs) persists path-2's
everything-grant — which is exactly how stale exact-pin sets regenerate.

Fix direction: when hard (non-`||`) deps are dropped because every
version was acceptance-filtered, treat that like phase-1 failure —
retry widened and prefer the tiered selection's bounded grant; fall
back to today's per-version pins only if widening can't resolve either.
Found while regenerating the i586 sandbox's llvm accepts from scratch
(2026-08-25); functional today only because `package.mask` keeps the
`.9999` entries dead. Run4's final tally also confirmed both failure
mechanisms above: the `.9999` picks died at EGIT fetch (no git binary —
dev-vcs/git had failed on `declare -I` in run3) and the `_pre`/`_rc3`
picks died at cmake via the cfg clobber.

## Stage1 pilot (2026-08-26): 95/97 ok — two cross-configure blockers triaged

`em --target i586-pc-linux-gnu stages --stage1` ran the full
packages.build core chain into the target sysroot: **95 ok, 2 failed**
(python, diffutils). Both failures are classic pure-cross configure
gaps, not em defects:

### sys-apps/diffutils — gnulib AC_RUN_IFELSE without cross fallback

**Correction 2026-08-29**: this was wrongly written off as "not an em
defect" — it *is* one, and it's the exact case `sys-devel/crossdev`'s
own config.site cache-answer library exists to solve. Root-caused for
real: [[crossdev-config-site-not-found-by-board-packages]]. `em` never
exports `CONFIG_SITE`, so autoconf's own path-based auto-search is the
only way config.site gets found — which can never reach it for
board-destined native packages (their `--prefix` is correctly
`EPREFIX`-less). This test run's apparent "not em's problem" framing
was itself an artifact of a reused sandbox whose bare host happened to
already have `config.site` installed from unrelated prior testing.
Original note kept below for the historical trail.

`checking whether strcasecmp works...` → `cannot run test program while
cross compiling`. configure received correct `--build=aarch64
--host=i586` (verified in config.log); the gnulib snapshot simply lacks
a compile-only fallback for this test, and **thalia has no qemu-user /
binfmt-i586** (`/proc/sys/fs/binfmt_misc` empty), so target binaries
cannot execute for anyone — real crossdev would hit the identical wall
at this exact package. Resolution options: qemu-user + binfmt on the
host; a config.site cache (`gl_cv_func_strcasecmp_works=...`) fed via
CONFIG_SITE in the sysroot make.conf; or upstream gnulib fix.

### dev-lang/python — ✅ FIXED 2026-08-26 (see [[crossdev-pkg-config-sysroot-leak]])

The narrative below is kept for the investigation trail, but its root
cause (BUILD_PKG_CONFIG falling back to the cross flavor) turned out to
be imprecise — the real mechanism (confirmed by direct reproduction) is
`PKG_CONFIG_SYSROOT_DIR`/`PKG_CONFIG_LIBDIR` being static, ambient exports
in the sysroot make.conf, leaking into `econf_build`'s native
sub-configure regardless of which pkg-config binary it resolves to. Full
diagnosis, the fix, its accepted tradeoff, and per-topology verification
status live in [[crossdev-pkg-config-sysroot-leak]].

### dev-lang/python — original (superseded) diagnosis

Precise chronology (build.log is chronological, two full attempts):

1. `src_configure` → `tc-is-cross-compiler` true → `build_cbuild_python`:
   out-of-tree econf_build into `work/python-3.14.7-aarch64-unknown-
   linux-gnu/` (native aarch64/aarch64 — correct), then its `emake`
   **died compiling `Modules/_decimal/_decimal.c`: `#error No valid
   combination of CONFIG_64, CONFIG_32 and _PyHASH_BITS`**.
2. `build_cbuild_python` has no die-on-emake (function ends at `popd`),
   so src_configure continued to the main target econf with
   `--with-build-python=<cbuild>/python` — pointing at a binary that was
   never produced — and died there. Pattern repeats ×2 (two consumers).

Root cause chain for step 1, from the cbuild config.log cmdline:

- `--libdir=/usr/i586-pc-linux-gnu/usr/lib` — the eclass computes
  `cbuild_libdir` via `tc-getBUILD_PKG_CONFIG --libs-only-L libffi`,
  and it resolved to the **target sysroot** instead of the host. So
  BUILD-flavored pkg-config answered with target-sysroot paths under
  em's phase env.
- `--with-system-libmpdec` is unconditional in myeconfargs, and its
  detection also ran through the same cross-flavored pkg-config — so the
  internal `_decimal` build compiled without the CONFIG_64/32 defines
  while expecting external libmpdec, producing the #error.

Suspected em-side divergence (needs one live probe to close): em exports
a single cross-flavored `PKG_CONFIG` (${chost}-pkg-config wrapper with
the sysroot baked in) for the whole phase, and the `BUILD_*` tool
exports (shell.rs ~1340) are gated on host-bin/PATH conditions that are
false inside a sandboxed container (no host gcc on PATH there) — so
`tc-getBUILD_PKG_CONFIG` fell back through to the cross flavor. Real
portage keeps BUILD pkg-config host-resolving, which is why the same
ebuild works under crossdev+emerge.

Decisive next probes: inside the failing phase env replay
`echo ${BUILD_PKG_CONFIG-unset}; pkg-config --libs-only-L libffi` and
compare against real-portage phases; then either export BUILD_* into
the child env unconditionally when cross, or give econf_build a proper
builtin that strips target flavor.

Also observed (cosmetic): baselayout's env-update logs
`failed to redirect ... /proc/mounts` inside the bare sysroot; merge
completes regardless.

## Brush completion edge (from validating the IFS fix)

After the `SourceText` expansion-piece fix (IFS="\n" made literal command
words split — see below), one brush-shell completion test regressed:
`complete_quoted_filenames` ("ls item1\ " now returns ["item1","item2"]
instead of ["item1 item2"]). The compgen `-W` rewrite fixed two other tests
(`complete_command_option`, `complete_find_command`). Net: 1 edge failure vs
2 before; needs someone who knows bash_completion's `_filedir` internals.

## The brush fix that landed (context)

`brush-core/src/expansion.rs`: literal source text was marked field-split-
table, so any script setting exotic IFS (perl-functions.eclass sets
`IFS="\n"`!) broke every command whose name contained an IFS char —
`einfo` became `ei`. Fixed by adding an `ExpansionPiece::SourceText` kind
(subject to globbing, never to `$IFS` splitting), with parameter-expansion
default values classified as expansion results (still splittable, per the
ifs.yaml oracle cases). Three previously-`known_failure` oracle cases now
pass.
