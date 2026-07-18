# Build roadmap: from `em sys-apps/less` to `em firefox`

Status tracking for the build/merge pipeline. Checked items are implemented
*and verified on this host*; the proof column says how. Resolver-side work
(preview parity, multi-repo, Level-C) has its own docs and is not re-listed.

Conventions: every milestone ends with a named **gate** — a concrete command
that must succeed before moving on. Fix forward only what the gate needs;
park everything else here.

## M0 — Foundations (done)

- [x] `em -p` plan parity with `emerge -p` (8-target basket, exit codes,
      preview semantics) — `benchmarks/bench-em-vs-emerge.sh`
- [x] Multi-repo: repos.conf overlays, sourced metadata + user cache,
      crossdev symlinks, `em regen` — cross-riscv64 gcc resolves `[R] 16.1.0`
- [x] `em ebuild <path> <phases>`: fetch (RO-fallback distdirs), unpack,
      src_* chain, install, merge (collision check, slot-occupant unmerge,
      CONTENTS/counter/environment.bz2, pkg_preinst/postinst)
- [x] Hermetic per-ebuild sourcing (baseline shell snapshot)
- [x] `em <atoms>`: resolve → build loop (per-package effective USE from the
      plan) → qmerge, refusing on pending USE changes
- [x] `--prefix DIR`: ROOT+VDB+distfiles+build trees under DIR, unprivileged
      — gate was `em --prefix /tmp/p sys-apps/less` → binary runs, PCRE2 USE
      applied

## M1 — Single-package robustness

Goal: a leaf-package build is trustworthy and debuggable.

- [x] `src_test`: skipped by default in the merge chain, run under
      `FEATURES=test` (explicit `em ebuild … test` always runs it)
- [x] FEATURES parsing from the configured shell (profile + make.conf):
      `test`, `keepwork`, and `nostrip` acted on (the last via the M1.5
      estrip pass), the rest accepted silently; collision check is always-on
- [x] Per-package build log (`<workdir>/build.log`, tee'd via process
      substitution; path attached to failures) and `-q` captured-silent mode.
      Required teaching Rust-builtin children (econf/emake) to honour the
      shell context's redirected fds (`context_stdio`)
- [x] Profile build environment in the phase shell: `make.defaults` vars
      (CHOST, CFLAGS/LDFLAGS defaults, `MULTILIB_ABIS`, `ABI`, `LIBDIR_*`,
      `USE_EXPAND` values) via `ProfileStack::configure_shell`; the plan's
      per-package USE overrides on top — file-5.47 now builds in
      `file-5.47-.arm64` with libs in `/usr/lib64`
- [x] `pkg_pretend` + `pkg_setup` in the merge chain with correct
      `EBUILD_PHASE`/`MERGE_TYPE` (both were already wired; chain extended)
- [x] die-in-subshell: `die` now raises an Arc-shared `DieFlag` visible to
      the phase driver after the phase returns, so a die inside `$(...)` or a
      helper pipeline aborts the build (portage's marker+signal, in-process).
      This flipped two silent corruptions into real failures and led to their
      fixes: `has_version`/`best_version` were metadata stubs returning
      false — they are real VDB-querying builtins now (`-b/-d/-r` against
      BROOT/ESYSROOT/ROOT), un-stubbed for phases; and `econf` was missing
      the PMS `--libdir=${EPREFIX}/usr/$(get_libdir)` argument, so xz-utils
      installed its libraries to `usr/lib` and failed its own sanity check.
      Also: `TMPDIR` joined the phase export list (eltpatch wrote to
      `/libtool-elt.patch`).
- [x] Leaf-basket hardening run (2026-06-11, all unprivileged into fresh
      prefixes, die enforcement active):

      | package | result |
      |---|---|
      | sys-apps/file (autotools+multilib) | PASS |
      | app-arch/gzip (network fetch) | PASS |
      | sys-devel/bc | PASS |
      | app-arch/zstd (meson) | PASS |
      | app-arch/xz-utils (autotools, libdir-sensitive) | PASS |
      | sys-apps/sed | PASS |
      | sys-apps/less (eautoreconf) | PASS |

**Gate:** `em --prefix /tmp/p app-arch/zstd` (meson, as it turns out) and
`sys-apps/file` (multilib) both merge with correct ABI libdirs and a saved
build.log — **passed 2026-06-11**, and **M1 is complete** (7/7 basket with
die enforcement). The parked `command not found: -E` wart turned out to be
two real bugs, both fixed: `usex`'s value positionals rejected
hyphen-leading arguments (meson.eclass's `meson_use`/`meson_feature` emit
`-D...=true/false`), and brush's `export` silently dropped runtime
assignments from expanded words (`export ${var}=value` —
toolchain-funcs.eclass `_tc-getPROG`), so every `tc-getCC` lookup returned
empty and `$($(tc-getCC) -E -P -)` ran `-E` as a command. Fixed in the
brush fork (with bash-oracle compat cases) — builds now get the proper
CHOST-prefixed compiler exported.

### M1.5 — merge/install parity with portage

Post-M1 follow-on so installed images match what portage produces byte-for-byte
in layout (not just "a working binary"). All done:

- [x] `REPLACING_VERSIONS` / `REPLACED_BY_VERSION` (PMS 11.1): computed from the
      target root's VDB + the ebuild's SLOT, visible to
      pkg_pretend/setup/preinst/postinst (and prerm/postrm of the replaced one)
- [x] mtime preservation when copying the image into ROOT — regular files via
      `File::set_modified`, symlinks via `utimensat(AT_SYMLINK_NOFOLLOW)`
      (std always follows links)
- [x] **CONFIG_PROTECT / CONFIG_PROTECT_MASK** (portage's `ConfigProtect` +
      `new_protect_filename`): an existing, differing file under a protected
      path (longest-prefix wins over the mask; `/etc` always protected) is
      written to `._cfgNNNN_<name>` (next index, reusing the latest when its
      md5 already matches) instead of overwritten; new files and unchanged
      content merge directly, zero-size `.keep*` are exempt, symlinks are
      protected by target. CONTENTS records the real path with the new md5
      (the `._cfg` is the pending delivery for `em dispatch`/`em etc`), exactly
      as portage does.
- [x] hardlink preservation: files already hardlinked inside the image
      (`nlink > 1`) are re-created as shared inodes in ROOT via a source
      `(dev, ino)` → first-dest map (portage's `_hardlink_merge_map`), instead
      of copied independently
- [x] missing install helpers: `fperms`, `fowners`, `doinfo`, `dolib.so`,
      `dolib.a`, `domo` (MOPREFIX-aware), `get_libdir`, plus
      `doenvd`/`newenvd`/`newconfd`/`newinitd` — real functions in
      `INSTALL_HELPERS`, overriding the metadata stubs
- [x] `einstall` (pre-EAPI-6, banned in 6+): a Rust builtin mirroring portage's
      phase-helper — `${MAKE} install` with GNU path overrides into `${ED}`,
      libdir override from `LIBDIR_${ABI}`/`CONF_PREFIX`, EAPI-gated, dies on a
      missing Makefile. For completeness with legacy ebuilds (M4 targets are
      EAPI 7+)
- [x] `env-update`: `${ROOT}/etc/profile.env` + `ld.so.conf` regenerated from
      `env.d` (COLON_/SPACE_SEPARATED/last-wins, LDPATH→linker only), `ldconfig`
      refreshed (`-r` for offset roots); run after the merge loop and as `em env`
- [x] **ecompress + estrip** (PMS 12.3.9/12.3.10): post-`src_install` Rust pass
      (`postprocess.rs`) compresses `/usr/share/{doc,info,man}` (plus ebuild
      `docompress` opt-ins, minus `docompress -x` and already-compressed/binary
      suffixes) with `${PORTAGE_COMPRESS}` (default bzip2 → `.bz2`, matching the
      host), retargets symlinks to the compressed names, and strips ELF
      `ET_EXEC`/`ET_DYN` objects with `${STRIP} --strip-unneeded`, honouring
      `FEATURES=nostrip`, `RESTRICT=strip` (+ `dostrip` opt-back-in), and
      `dostrip -x`. Verified against host portage: `sys-apps/less` into a fresh
      prefix yields identical `less.1.bz2`/`lesskey.1.bz2`/`lessecho.1.bz2`,
      stripped `/usr/bin/less`, compressed `README.bz2`/`NEWS.bz2`, and CONTENTS
      recording the `.bz2` names.

      `docompress`/`dostrip` are Rust builtins (`commands/install_paths.rs`)
      that push include/exclude paths into an `Arc`-shared `InstallPaths` state
      (the `DieFlag` pattern), which the merge driver snapshots after
      `src_install` via `EbuildShell::install_paths()` — no shell-variable
      round-trip. `tee` in the build-log process-sub now `cd /`s first so its
      lazy spawn doesn't fault on the just-cleaned `${S}`.

      **Future Rust builtins** (drop the external `bzip2`/`strip` shell-outs):
      - *ecompress*: swap `Command::new(PORTAGE_COMPRESS)` for pure-Rust codecs
        (`bzip2`/`flate2`/`xz2`/`zstd` crates, already in the tree for distfiles
        and `environment.bz2`), keyed off the `PORTAGE_COMPRESS` basename →
        suffix map that `compress_suffix` already encodes. Keep the shell-out as
        a fallback when a user sets an exotic `PORTAGE_COMPRESS`. Wins: no
        per-file fork, streaming straight into the renamed target.
      - *estrip*: replace `strip --strip-unneeded` with an `object`/`goblin`
        ELF rewriter dropping `.symtab`/`.comment`/`.note.*`/debug sections,
        which removes the binutils runtime dependency entirely and lets us split
        debug info (`FEATURES=splitdebug` → `.debug` files) in-process. Higher
        effort than ecompress; `is_strippable_elf` (ET_EXEC/ET_DYN gate) and the
        scope/exclude plumbing are already builtin-ready, so only the rewrite
        core is new.

## M2 — Multi-package orchestration

Goal: dependency chains build in order and failures are resumable.

The root model (`--root` base vs `--prefix` install target, the three location
roots, and the scenario matrix) is specified in [root-model.md](root-model.md);
the items below are its Stage 1–2. The post-Stage-2 ordering — `--root` (native
host) → crossdev → `--local`/`--prefix`, each tier built on the previous — is
specified there under **Sequencing**; Tier 1 is the active work.

- [x] Pre-flight dependency check (`preflight.rs`): before the build loop, each
      plan entry's `DEPEND` (checked against `VDB(base) ∪ VDB(target)`) and
      `BDEPEND` (against the host `BROOT=/`) must be satisfied by that view plus
      the earlier plan entries (within-run visibility); otherwise it bails with
      a clear message naming each package and its unsatisfied atoms. USE
      conditionals are folded with the entry's effective flags; `|| ( )` groups
      pass if any member is present; blockers and `^^`/`??` are not presence
      requirements. The solver already builds a complete closure, so this is a
      fast guard rail that turns a mid-build failure into an early, named error.
- [x] Within-run visibility of earlier merges, planner + builder
      (root-model.md Stage 1/1b): `Roots` = `config`/`base`/`target`; planner
      config from `config_root`, installed = `VDB(base) ∪ VDB(target)`, merge
      into `target`; phases get `PORTAGE_CONFIGROOT=config`, `ROOT=target`,
      `SYSROOT=ESYSROOT=base` (eclasses do the build-system translation). Host,
      full-offset/stage, and single-package `--prefix` builds are correct.
- [x] Overlay (`--prefix`, target ≠ base) — config-driven via `bashrc`: `em`
      sources profile `profile.bashrc` + `/etc/portage/bashrc` per phase
      (portage's hook, not PMS) exposing the roots + `get_libdir`; the user
      wires overlay search paths there, so our code carries no build-system
      knowledge (root-model.md "Overlay support" / "bashrc support"). The
      zero-config merged sysroot is deferred to M3 (namespaces).
- [x] `--keep-going` + resume: resume needs no state file — the target VDB is
      the state, so an entry already recorded there (at the planned version) is
      skipped and a re-run continues from the first unmerged entry
      (`--emptytree` forces a rebuild). `--keep-going` continues past a failed
      package and reports all failures at the end. *Follow-up: dependency-aware
      skipping of a failed package's dependents (needs solver edges, shared
      with `--jobs`).*
- [x] `--ask` prompt before the loop (defaults to no on empty/EOF)
- [x] Failure report: per-package cause (error chain) + build-log path, plus a
      merged/skipped/failed tally
- [x] `--jobs N`: parallel builds respecting build order. The depgraph exposes
      `build_blockers` (per plan entry, the *earlier* entries it needs at build
      time — DEPEND/BDEPEND edges, CPN-matched and index-restricted so it's
      acyclic); `merge_parallel` runs up to N package builds concurrently via a
      `FuturesUnordered` (single-threaded — the `EbuildShell` isn't `Send`;
      parallelism comes from the build subprocesses we await), gated by a
      `Scheduler` that releases a node once its blockers complete. The compile
      phases overlap, but the qmerge critical section (collision check, VDB
      counter, world/profile) is serialised by a shared async `Mutex` so only
      one package mutates the live root at a time. A failed build (without
      `--keep-going`) stops new launches and drains in-flight; a failed build
      dep strands its dependents (never started). `Scheduler` is unit-tested
      (chain/diamond/multi-blocker/failed-blocker). *Known gap: the unguarded
      fetch phase can race on a shared distfile (rare); per-distfile locking is
      a later refinement.*

**Gate:** a 2–3 package uninstalled chain (e.g. `app-text/tree` style leaf +
a lib dep) merges into a fresh prefix in one `em --prefix` invocation;
killing it mid-way and rerunning completes without rebuilding done entries.

## M3 — Sandbox & safety

Goal: phases stop running raw on the host.

- [ ] Decide mechanism: mount/user namespaces (bubblewrap-style, no root) vs
      portage's LD_PRELOAD sandbox vs both-tiered; write the decision here
- [ ] Write-confinement: build can write only WORKDIR/T/D (+ DISTDIR for
      fetch); violations logged, fatal under `FEATURES=strict`-equivalent
- [ ] Network off during src_* phases (fetch is the only network phase)
- [ ] `userpriv` semantics for root invocations (drop to a build user)

**Gate:** a deliberately misbehaving ebuild (writes to `$HOME`, phones home
in src_compile) is caught by both confinements.

## M4 — Heavy-stack eclass coverage (the firefox prerequisites)

Goal: the eclass machinery firefox's stack needs works under our shell.
Iterate target-by-target, hardest last:

- [x] meson package end-to-end — `app-arch/zstd` (meson build system) merged
      into a prefix with working binary and libraries
- [ ] cmake package end-to-end (`app-arch/zstd` if not done in M1)
- [x] python-any-r1 BDEPEND package (host python detection, no target installs)
      — `python_setup` picks python3.14, prepends the wrapper bin dir to PATH
      without dropping the system dirs (verified in isolation and in firefox)
- [x] `check-reqs` (disk-space check runs), `multiprocessing`,
      `toolchain-funcs` audit under our shell — exercised by firefox pkg_setup
- [x] llvm-r1 slot detection against host LLVM — `LLVM_SLOT=21` via the
      USE_EXPAND reverse mapping; firefox's "Skipping Rust … does not match
      llvm_slot_21" / "Using Rust 1.94.0" path works
- [ ] cargo eclass: vendored-crates SRC_URI unpack (`cargo_src_unpack`),
      offline `cargo build`; small rust package end-to-end first
- [x] self-contained install helpers: the `do*`/`new*` family lives in
      `INSTALL_HELPERS` as in-shell bash functions, so **portage need not be
      installed**. `init_build_env` no longer prepends `PORTAGE_BIN_PATH`'s
      `ebuild-helpers/` to `PATH` (it still exports `PORTAGE_BIN_PATH` for the
      eclasses that reference it). Added `doenvd`/`newenvd`/`newconfd`/`newinitd`
      and fixed `into` to mirror `DESTTREE`
- [ ] ebuild-helpers coverage audit: list what firefox's install phase calls
      and confirm each has a self-contained equivalent in `INSTALL_HELPERS`
- [x] firefox dry-run ladder — `setup → unpack → prepare` are **green** and
      `configure` runs deep into mach (`em ebuild firefox setup unpack prepare
      configure`):
      * **setup**: check-reqs → llvm-r1 → rust → python-any-r1 → linux-info all
        complete, no "command not found"
      * **unpack**: both distfiles fetched via the RO-fallback distdir, the
        748 MB `.tar.xz` + patches tarball unpacked, `src_unpack`'s
        language_packs logic ran
      * **prepare**: all 24 firefox-patches applied via the rewritten `eapply`
      * **configure**: mach configure detects clang-22 C/C++, lld,
        llvm-profdata/readelf/objcopy, runs the full compiler/linker flag
        probe matrix, then stops at the first **missing build dep**
        (`pkg-config: Package 'alsa' not found` — media-libs/alsa-lib isn't
        installed on the host). That's environmental, not an em/shell defect:
        the full gate provisions the dep closure first.
      Survey fixes that got it there:
      * PATH established as a shell var (eclasses doing `export PATH=…:${PATH}`
        no longer strand the system bin dirs)
      * USE_EXPAND groups reverse-mapped from final USE (`LLVM_SLOT` et al)
      * sandbox path-reg functions stubbed as no-ops (addread/write/predict/deny)
      * `em ebuild` applies the ebuild's IUSE `+` defaults (merge-path parity)
      * **brush fix**: `$*`/`${a[*]}` now join via the IFS first char in scalar
        contexts, so llvm-utils' `local IFS=:; …; export PATH=${sp[*]}` keeps
        its `:` separators instead of mangling them to spaces
      * **eapply** rewritten for PMS: directory operands (apply a dir's
        `*.diff`/`*.patch` children in C order), the `--` option-parsing rule,
        and a `return 0` so the `|| return` loop doesn't abort after one patch
        (firefox was silently applying only 1 of 24)
      Remaining (cosmetic): linux-info prints `//usr/src/linux` (double-slash
      ROOT join) and falls back to the running kernel — check still `[ ok ]`.
      Next: provision firefox's build-dep closure so `configure` completes the
      gate (this is the M2-orchestration test for real).

**Gate:** `em --prefix /tmp/p www-client/firefox` completes
setup→configure. (Full compile is hours of CPU — gate on configure, run
compile once overnight when configure is clean.)

## M5 — emerge UX completeness (post-firefox, ordered by value)

- [ ] `-b`/`--buildpkg`: binary package creation on merge (decide xpak
      vs gpkg first — gpkg is the future, xpak interops with existing hosts)
- [ ] `quickpkg` from installed files (currently a stub)
- [ ] `-K`/`--usepkg`: install from binpkg, skipping build phases
- [ ] `em -C` unmerge (the slot-occupant unmerge logic already exists —
      expose it standalone)
- [x] `@world`/`@system` set resolution — `expand_sets` (`90803fb`) resolves
      `@system`/`@profile` from the config-root profile and
      `@world`/`@selected`/user sets from the merge target.
- [~] `--update --deep --newuse` — **`-uD` in-slot upgrades done** 2026-07-18
      (`prefer_update` + host-satisfied BDEPEND retention; see
      `todo/deep-in-slot-upgrades.md`); `-D`/`-e` still drive `prefer_newest_slot`
      (`todo/deep-slot-bump.md`). **`--newuse` / `-N` still open**
      (`todo/newuse.md`).
- [ ] `--fetchonly`

## M6 — Prefix polish

- [ ] Prefix bootstrap helper: create baselayout dirs, optionally seed
      `etc/portage` (then `--prefix` could also offset config — today config
      is host-only by design)
- [ ] Environment entry script (`<prefix>/start` exporting PATH/LD_LIBRARY_PATH)
- [ ] Document the ROOT/BROOT/EPREFIX story vs Gentoo Prefix proper

## Standing items (not milestone-gated)

- [ ] Push master to origin (local `master` runs ahead of `origin/master`)
- [ ] Run `benchmarks/bench-em-vs-emerge.sh` after each milestone; parity
      regressions block
- [ ] brush upstream: `todo/checkpoint.md` (checkpoint/restore API)
- [ ] pubgrub upstream: portage-cli#1 ↔ pubgrub-rs#120 (multi-literal
      incompatibilities)
- [ ] Blockers/`::repo` Tier-1 enforcement (advisory today; `::repo` newly
      testable on this host)
