# Pending — stage-building arc (roadmap)

Open items from the toolchain → stage → binhost work, grouped. Each links to the
file with the detail. Status: 🔴 not started · 🟡 partial/decided · ✅ done (kept
here briefly for context). Updated **2026-07-18**.

**Also closed outside this arc (2026-07-18):** emerge-style **`-uD` in-slot
upgrades** — [[deep-in-slot-upgrades]]; **`-N`/`-U` USE-drift rebuilds**
(Portage-aligned, no eager BDEPEND) — [[newuse]]; parallel + mmap
**elfscan** vs scanelf; **`benchmarks/bench-resolve-modes.sh`** (`-p`/`-up`/
`-uNp`/`-uDp`/`-uNDp` vs emerge). Slot-only `:*` bumps remain
[[deep-slot-bump]].

## RESUME HERE (2026-07-18) — next pending queue

Resolver deep-update / newuse are landed. Prefer the next work by **value for
stages + host parity**, not by file age. Detail lives in the linked notes;
this table is the triage index.

### Suggested order

| Pri | Item | Status | Detail |
|-----|------|--------|--------|
| **1** | **Native `--root`/`--prefix` builds prefer the ROOT’s own `<chost>-gcc`** — likely already solved (user will re-verify); build shell PATH prefer if still open | 🟡 verify | [[select-toolchain]] |
| **2** | **CLI short-flag remnants** — `-P`/`--prune`, `-r`/`--resume`, `-W`/`--deselect`, `-F`/`--fetch-all-uri` | 🔴 | [[cli-flag-parity]] |
| **3** | **Binhost polish** — GPG verify/sign still open; **URI BASE_URI, auto-reindex, stages default `-b`, CHOST reuse gate** done 2026-07-18 | 🟡 | [[em-stages-and-binhosts]]; binhosts section below |
| **4** | **Parser audit** — incremental `-*`, package.*, sets, USE-deps, md5-cache faithfulness | 🔴 | [[parser-audit]] |
| **5** | **`package.env` USE in the resolver** — build-env slice ✅; USE from env files still ignored at plan time | 🟡 | [[package-env]] |
| **6** | **Blocker Tier-1** — detect/report ✅; auto-unmerge **slated last** (user) | 🟡 last | [[blocker-enforcement]] |

### Verified closed / demoted (2026-07-18)

| Item | Result |
|------|--------|
| **#36 inherit / `E_IUSE`** | ✅ **Already fixed.** `inherit` accumulates into `E_*`; `source_ebuild` merges ebuild + eclass values (PMS 10.2). Unit tests in `portage-repo/tests/brush_compat.rs` (`inherit_*`) all green. Host md5-cache for `app-alternatives/gpg` shows full IUSE. |
| **task #17 BROOT / riscv systemd-utils resolve** | ✅ core fix live-verified 2026-07-17. **Full** `--emptytree sys-apps/systemd-utils` still blocked by **Gentoo-side** gaps (`acl` undeclared `attr` dep; `pam.eclass` `dopamd -r`), not em BROOT. Track under shakeout, not as an open em feature. |
| **`em quickpkg`** | ✅ implemented 2026-07-18 — GPKG from VDB `CONTENTS` + live files; Portage `gpkg.get_metadata` accepts containers. |
| **`-f`/`--fetchonly`** | ✅ 2026-07-18 — fetch distfiles / remote binpkgs only; no build or install. |

### Smaller / polish (pick opportunistically)

- Shallow `-p` package-set still ~72 vs emerge ~79 on firefox hosts (pre-existing; not a `-uD` regression) — [[nonemptytree-bdeps-gap]], [[deep-in-slot-upgrades]]
- Residual provider choice on deep plans (`rust` vs `rust-bin`, tooling set) — polish, not blocking
- Numeric `--deep=N` (boolean only today); Resolvo `set_prefer_update` still trait default no-op
- Releng lean stage profile vs default profile (5-package @system delta) — [[stage3-vs-real-comparison]]
- `ACCEPT_KEYWORDS` `-arch` removal; `ACCEPT_PROPERTIES`/`ACCEPT_RESTRICT` — [[accept-properties-restrict]] (deferred)
- Privilege: in-session binpkg/stage tar as real `root:root`; hakoniwa wall-test; brush procsub deadlock pin bump — [[fakeroot-privilege-backends]], [[stage-build-shakeout]]
- Large design (not near-term): full [[root-topology-refactor]]; availability-walk [[dedup-availability-walks]]; M3 sandbox (namespaces)
- Upstream-ish: brush `PIPESTATUS`/`declare -a` on Dynamic — [[brush-pipestatus-not-reset]] (em workaround in place)

### Recently closed (keep for orientation)

| Item | When | Notes |
|------|------|--------|
| `-uD` in-slot upgrades | 2026-07-18 | `prefer_update`; keep host-satisfied BDEPEND on deep update |
| `-N`/`-U` USE-drift rebuilds | 2026-07-18 | Portage-aligned; eager BDEPEND reinstall mode **rejected** |
| elfscan parallel + mmap/strtab | 2026-07-18 | install-image scan ~scanelf recursive parity |
| resolve-mode hyperfine matrix | 2026-07-18 | `benchmarks/bench-resolve-modes.sh` |
| `-c` depclean, `-C` unmerge, world write, `-1` oneshot | earlier | [[cli-flag-parity]] |
| `-b`/`-B` GPKG, `-k`/`-K`/`-g`/`-G`, binrepos, index cache | earlier | `-K` no-source **enforced** in merge loop |
| `em quickpkg` | 2026-07-18 | GPKG from installed files; CONFIG_PROTECT skip default |
| `-f`/`--fetchonly` | 2026-07-18 | distfile / remote-binpkg download only |
| inherit / `E_IUSE` (#36) | earlier (verified 2026-07-18) | stash/accumulate + post-source merge |
| native `toolchain --setup` activation + `select pkgconf` | 2026-07-17 | wrappers re-rooted; open = PATH prefer (row 1) |
| `portage-resolve` extraction (depgraph → library) | 2026-07-16 | stages 1–7 done |

**How to use this file:** start from the table above; jump to the linked
note for design. Long historical narrative for the 2026-07-05 riscv shakeout
and the full binhost/migration log remains below (do not delete — it is the
audit trail).

---

**2026-07-09 (later)**: the global `--cross <tuple>` flag is now
**`--target <tuple>`/`-T`** (no clash), and `em crossdev` no longer has its
own `-t`/`--target` — one flag drives both "set up a target"
(`em --target T crossdev --init-target`) and "use one"
(`em --target T stages --stage1`). Also fixed: `crossdev`'s own setup
helpers were using the already-`--cross`-substituted `roots()`, so a
redundant global flag doubly-nested the sysroot — see
[[root-topology-refactor]] for the full story (same `Roots`-accessor-
confusion class as the `--root` BROOT fix above, one level up: flags, not
just methods).

**Since 2026-06-27**: the cross overlay was retired —
[[cross-derive-on-the-fly]] landed ✅ (2026-07-08, `d7ac770`+`b3df565`+
`363e9aa`+`42d9903`): `cross-<tuple>/<pkg>` is now derived from `::gentoo`
at resolve time via `Location::Alias`, no on-disk symlink overlay, `--prefix`
crossdev setup runs unprivileged end-to-end. [[crossdev-target]] reflects
the same landing. Unrelated to this: `portage_cli` was split into a lib
crate + `main.rs` thinned to 62 lines, inline tests extracted to sibling
`tests.rs` files, and `#![warn(missing_docs)]` enabled on `portage-repo`
(commits through `27de5af`) — pure code-health work, no behaviour change,
verified clean (`fmt`/clippy/full test suite) independently of this arc.

## RESUME HERE (2026-07-05) — historical; independent-review checklist

> **Superseded for prioritization** by the 2026-07-18 next-pending queue at
> the top of this file. Keep this section as the riscv64 task #17 /
> findings #22–37 audit trail.

**⚠️ Before trusting anything below: read
[[session-status-2026-07-05-needs-review]] first.** That file lists
claims that rest on indirect evidence (VDB spot-checks, log
greps) rather than clean re-runs, plus one confirmed methodology mistake
(a task-notification "exit code 0" that didn't actually reflect `em`'s
own exit status, caused by an `echo` masking it — worked around later by
writing the exit code into the log file directly, but any earlier
exit-code reasoning in this arc should be treated as suspect until
re-checked).

Riscv64 stage3 (`--emptytree @system`) shakeout, [[stage-build-shakeout]]
findings #22-37 — full detail there, this is the compressed pointer.

Five Host/Target root-conflation bugs (#28/#30/#31/#32/#33), a genuine
gawk↔bison↔gettext↔libxml2↔meson↔python bootstrap cycle (#34, broken by
hand with `--nodeps`), a `--nodeps`/`preflight` bug, and an `unpack`
fatal-die-on-unrecognized-suffix bug are all committed with regression
tests (`13bb26d`, `fa27567`, `bc236f7`). Task #20 (`em stages --stage1`
complete on `base_roots()`) was marked done based on a VDB-presence spot
check of the 4 packages a run reported as "failed" — **not yet
independently re-verified**, see the review file.

One more real, deferred bug (#36): `app-alternatives/gpg`'s VDB-written
`IUSE` drops eclass-injected flags (`ebuild.rs`'s `iuse: env.iuse`
sources from live post-execution shell state, not the metadata cache).
Unblocked via a **manual, hand-edited VDB file patch** (not a code fix).
**2026-07-09: root cause now precisely nailed down** (not just "needs a
design pass") — read real Portage's own `inherit()` in `ebuild.sh`
(`/usr/lib/portage/python3.13/ebuild.sh:284-344`): before sourcing each
eclass it stashes any pre-existing `IUSE`/`*DEPEND` into `B_IUSE` etc. and
unsets the live var, sources the eclass, appends whatever the eclass set to
an `E_IUSE` accumulator, then **rolls the live var back** to its pre-inherit
value — so an ebuild's own later plain `IUSE=` assignment never stomps on
what an eclass contributed. At the end, `IUSE+="${IUSE:+ }${E_IUSE}"`
appends the accumulated eclass contribution to the ebuild's own final value.
Verified against real portage's md5-cache for this exact ebuild:
`IUSE=nls ssl +reference freepg sequoia` (ebuild's own tokens first, eclass
tokens appended — reproduces exactly). em's `collect_env`/`EbuildEnv` has no
equivalent stash/accumulate/rollback around `inherit`, hence the drop. Fix
is well-specified now (mirror this for IUSE/REQUIRED_USE/*DEPEND/PROPERTIES/
RESTRICT around em's `inherit` implementation) but touches the core shell
interpreter, not attempted yet — **keep the manual VDB patch until it
lands**, don't revert it prematurely.

**Task #17 — re-verified 2026-07-09 (fresh `em` build, clean run, not a VDB
spot check), root-caused for real, and fixed.**
`--autosolve-use --privilege pseudoroot --root /var/tmp/cross-stage1-riscv64
--cross riscv64-unknown-linux-gnu --emptytree sys-apps/systemd-utils
--with-bdeps --keep-going --jobs 16 --buildpkg` → `docbook-xml-dtd-4.2-r3`
merges, `systemd-utils` dies: `Program python3 (jinja2) found: NO`. First
pass here blamed "this EROOT needs a full native stage1" — wrong. Real
cause, found after the user corrected the framing: real `ROOT=`/
`{target}-emerge` always resolve `BDEPEND` against the **host `/`**, only
the install target moves; `cli.rs::base_roots()` had `--root R` set
`base: R, target: R`, so every BDEPEND check (`preflight`,
`bdepend_avail::initial_bdepend`, `load_host_installed`) read the *offset's*
VDB instead of the host's — which already has jinja2 for python3.13,
exactly what the host's own `meson` needs. **Fixed**, in two passes (see
[[root-topology-refactor]] for the full story — the first pass changed
`base_roots()` directly and broke crossdev's own unprivileged toolchain
install path; landed as a new, separate `Cli::broot()` instead, leaving
`base_roots()` untouched): `--root`'s BROOT is now always the host via
`broot()`; `--local` is now parameterized (`--local DIR`, was a bare bool
hardcoded to `~/.gentoo`) so the old self-contained-BROOT-in-an-offset
workflow (what this sysroot was actually doing) stays expressible under its
own name. Unit-tested (`root_broot_is_host_not_offset`,
`local_with_path_uses_dir_directly`); full workspace test suite green.
**Re-verified live**: `em --root R --cross riscv64-unknown-linux-gnu
crossdev -t riscv64-unknown-linux-gnu --init-target` now completes cleanly
and unprivileged (this exact command hit a real, self-inflicted permission
wall during the first pass — fixed, see [[root-topology-refactor]]).
**Not yet re-verified against a full rebuilt riscv64 sysroot through
`sys-apps/systemd-utils`** (the old one was wiped per the user's
instruction) — that's a long real build, left as a follow-up. **`app-text/
opensp` is unrelated** — it isn't in `sys-apps/systemd-utils`'s dependency
closure at all; the 2026-07-05 "2 failures" pairing was a broader run's
artifact, not this target's. See [[stage-build-shakeout]] finding #28's
2026-07-09 addendum and [[root-topology-refactor]].

**Next**: re-run the full task #17 target end-to-end against a freshly
bootstrapped riscv64 cross sysroot to confirm the fix closes it completely
(the fast pretend-mode check already confirms jinja2 is seen as
host-satisfied). The rest of the 2026-07-05 review checklist (re-derive
"stage1 is clean" from a fresh run; spot-check the 4 committed fixes) is
still open.

**2026-07-17: task #17 finally run for real (not pretend) against a complete
riscv64 cross toolchain** (`regress-crossdev-root` from `regression-matrix.sh`,
`gcc-stage2` activated, real host-native `riscv64-unknown-linux-gnu-gcc`
confirmed working). `em --root <dir> --target riscv64-unknown-linux-gnu
sys-apps/systemd-utils --emptytree --with-bdeps --autounmask-write --jobs 8
--buildpkg` (no `--keep-going`, see [[no-keep-going-flag]]):

- **The core task #17 fix is confirmed**: `-p` resolution succeeds cleanly
  (46 packages, correct `Root-aware cross plan: CHOST=riscv64-unknown-linux-gnu
  CBUILD=aarch64-unknown-linux-gnu` banner), and the real build genuinely
  cross-compiles and merges packages (glibc's own dependency chain, zlib,
  ncurses, xz-utils, etc. — 21+ packages merged with real
  `riscv64-unknown-linux-gnu-gcc` output) into the sysroot before hitting
  unrelated failures below. This is the first real (non-pretend) confirmation
  since the 2026-07-05/07-09 BROOT fix landed.
- **Blocker found and fixed first**: the fixture's own sysroot make.conf had
  `ACCEPT_KEYWORDS="~arm64"` (the *host's* arch) instead of `"riscv ~riscv"` —
  traced to `regression-matrix.sh`'s `run_crossdev` running a redundant
  `em --target $T $dir_flag $dir setup` pre-step before `crossdev --setup`.
  With `--target` set, that hits `Applet::Setup => setup::bootstrap(&globals
  .roots())` (the sysroot-substituted view, not the outer root), writing the
  generic self-contained-root make.conf template directly into the cross
  sysroot; `crossdev --setup`'s own correct write was then silently skipped by
  `FillGapsOnly` (`config_plan.rs`'s `ConfigEntry::File` treats "exists" as
  "done" regardless of content). `init_target` already bootstraps the outer
  root correctly itself (`setup::bootstrap(&globals.outer_roots())`) — the
  pre-step was never needed. Fixed by removing it from the script; the two
  already-contaminated fixture sysroots were repaired via `crossdev
  --init-target` (`Sync` policy, force-regenerates). Not an `em` correctness
  bug — a test-script mistake — but a real trap for anyone reaching for
  `em setup` manually before `crossdev --setup` with `--target` already set.
- **Three genuine, independent findings surfaced by this being the first real
  from-scratch closure test**, none blocking the actual task #17 fix:
  1. `sys-apps/acl-2.4.0`'s configure hard-fails on a missing
     `attr/error_context.h` even though its own `DEPEND`/`RDEPEND` never
     mentions `sys-apps/attr` at all — a real, pre-existing Gentoo ebuild gap
     (undeclared implicit dependency) that's dormant in virtually all real
     installs because `sys-apps/attr` is already present from `@system`.
  2. `sys-auth/pambase`'s `dopamd -r stack/.` forwards `-r` as a positional
     arg to `cleanpamd()` (`pam.eclass`), which blindly iterates every arg as
     a filename with no flag-stripping — a real, pre-existing `pam.eclass`
     bug, dormant because its guarding `sed` only runs when `sys-libs/pam`
     ISN'T already installed (true almost nowhere in practice, but true here
     in a genuine from-scratch closure where pam/pambase can build in
     parallel).
  3. ✅ **FIXED, 2026-07-17 (`80e0fd9`) — not a brush bug after all.**
     `sys-libs/readline`'s `src_prepare` (real portage's own "we don't have
     pkg-config yet" bootstrap guess path is scoped to `use prefix &&
     [[ -n ${STAGE} ]]`, which doesn't apply to `em`'s cross/native bootstrap)
     falls through to a real `$(tc-getPKG_CONFIG)` call, which died with
     "command not found" against the absolute path
     `/usr/bin/riscv64-unknown-linux-gnu-pkg-config` — a file confirmed via
     `readlink`/`ls`/`command -v` to not exist anywhere. Initially looked like
     a `brush` PATH-resolution bug (`type -p` returning a false positive);
     **root-caused instead by instrumenting the ebuild directly** (a debug
     build printing `PKG_CONFIG` right before `tc-getPKG_CONFIG` ran):
     `PKG_CONFIG` was **already set** to that dead path *before* the ebuild's
     own logic ever touched it, so real `_tc-getPROG`'s "already set, trust
     it" fast path fired and its own `type -p` PATH-search/bare-name fallback
     never ran at all — `type -p` itself was correct the whole time.
     Traced to `portage-repo/src/build/shell.rs`'s cross-toolchain-selection
     block: it gates all 12 tool vars (`CC`/`CXX`/…/`PKG_CONFIG`) on a single
     check that `${chost}-gcc` exists, then set every var unconditionally
     from that same assumption. The other 11 are built by crossdev's own
     toolchain steps alongside gcc, so they genuinely exist whenever gcc
     does — but nothing `em` builds ever creates a `${chost}-pkg-config`
     wrapper (real crossdev relies on a separately-installed
     `cross-pkg-config` this project doesn't reproduce), so `em` was setting
     `PKG_CONFIG` to a promise it never keeps. Fixed by verifying each tool's
     own existence instead of trusting the gcc-only gate (new test:
     `cross_toolchain_selection_skips_pkg_config_when_wrapper_missing`).
     **Verified live**: `sys-libs/readline` now merges cleanly, and a full
     `sys-apps/systemd-utils --emptytree` re-run gets all the way through
     `sys-libs/glibc`'s own rebuild with no new pkg-config failures anywhere
     — only the two already-documented acl/pambase bugs above remain.
     **Closed the underlying gap for real, same session (`5a11f13`,
     corrected `6df928e`)**: `80e0fd9` only stopped `em` from lying about
     `PKG_CONFIG` — packages that genuinely need a working
     `${CTARGET}-pkg-config` still had nothing. Added **`em select
     pkgconf`**, a new select module creating the `<CTARGET>-pkg-config`
     wrapper real crossdev provides. First cut (`5a11f13`) was just a
     symlink to the chosen backend — **caught as insufficient**: real
     crossdev's own `cross-pkg-config` script does real work beyond
     forwarding the call (self-derives `PKG_CONFIG_SYSROOT_DIR` from
     `ROOT`/`SYSROOT`/`ESYSROOT` at invocation time; probes the ABI-correct
     `libdir` instead of hardcoding `lib64`/`lib`; sets
     `PKG_CONFIG_SYSTEM_LIBRARY_PATH`/`_INCLUDE_PATH` +
     `PKG_CONFIG_FDO_SYSROOT_RULES` so pkgconf suppresses redundant `-I`/
     `-L` for paths the cross-compiler already searches by default;
     sanitizes an inherited `PKG_CONFIG_PATH`; sanity-checks the output for
     leaked host `-I`/`-L` paths — the exact bug class already documented
     for `net-libs/libtirpc`). `6df928e` replaced the symlink with
     `WRAPPER_TEMPLATE`, a close adaptation of that real script, with only
     the resolved backend path baked in per target — the one piece
     genuinely specific to this wrapper. Still no env.d/versioned-profile
     state needed (unlike `compiler`/`binutils`): the script itself carries
     all the state, read back by `show`/`list` via its own
     `REAL_PKG_CONFIG=` line. Wired into the existing gcc-activation
     `post_step` (`activate_toolchain`/`activate_native_toolchain`) so a
     plain `crossdev --setup`/`toolchain --setup` leaves a working wrapper
     behind with no extra manual step; idempotent (never clobbers a
     deliberate `em select pkgconf set` choice). **Verified live
     end-to-end**: the generated script correctly suppresses redundant
     `-I`/`-L` for a real riscv64 sysroot's `zlib.pc` (matching real
     crossdev's own behavior exactly, confirmed by reading the real
     `/usr/bin/cross-pkg-config` script on this host), and works invoked
     *standalone* (just `SYSROOT`/`ESYSROOT`/`CHOST` set, no `em`-managed
     phase environment needed) — plus a plain `crossdev --setup` re-run
     recreates the wrapper automatically after it was removed.
     **Second correction, same session (`8b080a9`)**: `6df928e` still
     wrote a full rendered copy of the script per target — real crossdev
     instead ships *one* `cross-pkg-config`, symlinked per target, deriving
     `CHOST` from `${0##*/}`. Matched that shape: `em-cross-pkg-config` is
     now written once per root, `<CTARGET>-pkg-config` is a plain symlink
     to it, and the backend choice is shared across every target under
     that root (a host-tool decision with no reason to vary per target) —
     `select pkgconf set` deliberately always rewrites the shared script
     (affecting every configured target's symlink), while auto-activation
     only creates the symlink + shared script if missing. Verified live:
     both manual `set` and `crossdev --setup`'s automatic activation
     produce the shared script + symlink correctly; new test confirms a
     second target reuses the first's backend choice instead of re-picking
     a default.

**Net**: task #17 (the BROOT/VDB conflation bug) stays ✅ closed — today's run
is the first real proof it holds under an actual from-scratch cross build,
not just `-p`/preflight. Full `sys-apps/systemd-utils` completion is now
blocked by the three independent findings above, tracked separately.

## Stage building (the active goal: a real stage3)

- 🟡 **Privilege / fakeroot for stage builds.** `sys-apps/util-linux`'s own
  Makefile `chown root:root .../bin/mount` fails unprivileged → blocks
  `sys-apps/portage` → no self-extending base. **v1 landed**: an unprivileged
  building invocation re-execs once under a fakeroost (ptrace+seccomp) umbrella
  session, so chown/setuid succeed and the merge records ownership; the three
  EPERM workarounds are now inert (fakeroost fakes getuid→0). **Validated**:
  `sys-apps/util-linux` merges unprivileged into `stage1-base` (the setuid-`mount`
  chown wall is cleared). ✅ Facet 2 — `fowners` resolves owner names to numeric
  uid:gid against the target passwd/group. ✅ `EM_PRIVILEGE=sudo` backend (real
  root, opt-in). ✅ `EM_PRIVILEGE=hakoniwa` umbrella sketch (userns mapped root,
  `hakoniwa` 1.7.1; not wall-tested yet). ✅ **Per-package `__worker` scoping
  (2026-07-01)**: fakeroost/sudo no longer umbrella the run — the un-wrapped
  parent runs `pretend..compile`, then a wrapped `em __worker` child runs
  install+qmerge(+binpkg) per package (Q6: the ptrace tax stays off the
  compile). Env crosses the process boundary via a variables-only `worker-env`
  dump (needed a brush `$'...'` parser fix, fork `6038e073`); qmerge is
  serialised across workers by an flock on `work_base/.merge.lock`; hakoniwa
  stays an umbrella; `em ebuild … install` keeps the umbrella (no worker seam).
  Validated: baselayout source build, `-b` producer and `-k` binpkg merge all
  through the worker. ✅ **Scoping confirmed live + fakeroost wrap fixed
  (2026-07-02, `f3201cb`)**: a uid/chown probe ebuild caught the worker wrap
  discarding `fakeroot()`'s returned command (silent degrade to `none`);
  full backend matrix now verified (uid/chown/gpkg ownership per phase).
  ✅ **pseudoroot backend (2026-07-02, `37e8d49` + `c6b0ae9`)**:
  `--privilege pseudoroot` = LD_PRELOAD fake root, worker-scoped like
  fakeroost, no ptrace tax; phase env passes `LD_PRELOAD`/`PSEUDOROOT_*`
  through exported. The two blocking pseudoroot bugs (supervise-marker env
  leak into the child + uid/gid default clobber) shipped fixed in the
  v0.2.0 release. **2026-07-03**: the util-linux gpkg sweep caught a third
  pseudoroot gap — the interposer missed the LFS `stat64` family, so
  bzip2 (ownership-preserving, binds `lstat64`) recorded the real build
  user on every compressed doc/man page (189/588 files); fixed in
  pseudoroot `f3997ea` (fakeroost verified immune — ptrace is
  symbol-agnostic). After that: 0/588 leaks, setuid mount/umount/su 0/0.
  Shipped as **pseudoroot v0.2.1**; workspace pins the tag (`5acb4ce`),
  path patch dropped, doc/man repro green from the plain git dep. Remaining: the
  binpkg/stage tar
  in-session (real `root:root` artifacts — next), fakeroot (system) backend.
  ✅ **`auto` now defaults to pseudoroot over fakeroost (2026-07-05,
  `42d001e`)** — a real riscv64 stage3 `--buildpkg` run hit a rare,
  non-reproducible-in-isolation fakeroost ptrace-supervisor crash
  (`fakeroost: syscall failed: ENOENT`) that silently killed ~1/3 of
  packages' install workers *after* qmerge had already succeeded; switched
  the priority order in `Backend::auto_backend()`. See
  [[stage-build-shakeout]] finding #25.
  **2026-07-03**: resumed the `stage-build-shakeout` @system run under pseudoroot
  — the util-linux wall is confirmed cleared. Found (a) a stale-VDB trap: any
  acct-group/acct-user package merged before a privilege backend existed lies
  about group/user creation (silent eclass no-op, not a failure) — needs
  re-merging, not a code fix; (b) ✅ **hang FIXED**: a `brush` scheduling
  deadlock — any read-side process substitution inside a command substitution
  (`old_groups=$(egetgroups …)` → `while read … done < <(…)` in
  `acct-user.eclass pkg_postinst`) strands the procsub body in a tokio worker's
  non-stealable LIFO slot while the parent blocks on a synchronous pipe read,
  so it never gets its first poll. Fixed with a `yield_now().await` after the
  procsub spawn (`setup_process_substitution`, `brush-core/src/interp.rs`);
  verified end-to-end (`@system` resumed clean, 50/50, 0 failures, no hangs).
  Patch sits **uncommitted** in the `~/Sources/brush` working tree
  (`for-portage-repo` branch) — needs Luca to review/commit/push + bump the
  `Cargo.toml` rev pin. [[stage-build-shakeout]] **Benchmark fakeroost vs hakoniwa
  vs sudo** — the 2026-06-27 stage3 smoke showed fakeroost (ptrace+seccomp, 2 ctx
  switches per `stat`/chown/…) much slower on the gcc bootstrap; if hakoniwa
  (userns, ~no per-syscall cost) lands near sudo it should become the default
  unprivileged backend. **2026-06-28 update**: fakeroost issue #7 fixed on PR #8
  (stat via a seccomp `USER_NOTIF` pool lifts the ceiling ~2.7×, and beats upstream
  `fakeroot` which goes backwards under load) — but a per-syscall tax remains, so
  the plan is to scope fake-root to `src_install`/archive only, not the compile.
  [[fakeroot-privilege-backends]] § Open Q6
  [[stage-build-shakeout]]
- 🟡 **`em stages`** — stage1 (`baselayout` + `packages.build`) → stage3
  (`--emptytree @system`). No stage2 (em builds a fresh toolchain, crossdev
  model). **`packages.build` ingestion + the CLI are done** (2026-07-16 check:
  `ProfileStack::packages_build`/`stage1_packages`, `portage-repo/src/repo/
  profile.rs`, wired into `em stages --stage1` — this line was stale since
  2026-06-26). **The full native pipeline now runs clean end-to-end**
  (2026-07-16, `em-native-0716` sandbox): `toolchain --setup` → `stages
  --stage1` (82 pkgs) → `--emptytree --with-bdeps @system` (140 more,
  including a full `gcc`/`binutils` self-rebuild) — 201 packages, zero real
  failures. See [[stage3-vs-real-comparison]]'s 2026-07-16 entry for the
  exact commands (needs `--autosolve-use` for `app-alternatives` REQUIRED_USE
  picks under `USE="-* build"`, and `--autounmask-write` + a second identical
  run for the `virtual/dev-manager`→busybox[mdev] advisory — both match real
  emerge's own behaviour). What's still genuinely open: building *with the
  ROOT's own `<chost>-gcc` + SYSROOT=ROOT* rather than the host's compiler
  (confirmed still missing both at the `em select` layer and one level deeper
  in the build shell — see [[select-toolchain]]'s 2026-07-16 addendum — this
  run used the host's gcc, matching catalyst's seed-compiler model by
  accident, not em's own intended crossdev-style design); the riscv64 *cross*
  stage3 target (`sys-apps/systemd-utils --cross`, "task #17") is untested by
  this run (native only). [[em-stages-and-binhosts]]
- ✅ **`USE="-*"` clear-all** — now honoured across the USE/USE_EXPAND
  incremental merge (profile→globals→conf→env layers) and the shell-state read,
  so catalyst's `USE="-* build"` collapses the closure as expected.
- ✅ **`ACCEPT_LICENSE`/`ACCEPT_KEYWORDS` `-*`** — clear-all now honoured
  (`AcceptLicense::from_tokens` clears allow_all+allowed+denied;
  `AcceptToken::ClearAll` resets the accept decision, global and per-package).
- 🟡 **Remaining `-*` gaps are feature work, not patches:**
  - ✅ `package.use` USE_EXPAND colon form (`L10N: -* en`,
    `PYTHON_TARGETS: -* python2_7`) — `expand_use_expand_colon` (use_env.rs) parses
    `KEY:` group headers against the live USE_EXPAND keys, expands values to
    interned `UseOverride`s (no String detour), and treats a `-*` inside a group as
    "clear the group's live values, then trailing values rebuild it".
  - `ACCEPT_KEYWORDS` `-arch` removal still dropped (additive ArchAccept model).
  - `ACCEPT_PROPERTIES`/`ACCEPT_RESTRICT`/`PORTAGE_CHECKSUM_FILTER` — the vars
    themselves are unimplemented (zero refs); their GLEP-23 `*`/`-*` is moot
    until the vars exist.
  - `use.mask`/`use.force` correctly take only per-flag `-` (no `-*`, portage(5)).
  [[em-root-characterization]]
- ✅ **Native toolchain activation via `em select` — wrapper fixed 2026-07-17.**
  `em toolchain --setup --root <dir>` already activated via the real
  `gcc-config`/`binutils-config` in postinst (ROOT-scoped correctly by em's own
  `ROOT`/`EROOT` env) — but for a plain `--root` offset the resulting
  `usr/bin/<chost>-gcc` was a genuinely **dangling** symlink (its target isn't
  re-rooted, so it resolves against the real host filesystem, not the offset —
  confirmed via `readlink -f` failing). `--prefix`/`--local` never had this
  problem (real gcc-config is EPREFIX-aware there). Gave `em toolchain --setup`'s
  native path a real `post_step` (`activate_native_toolchain`, `crossdev/mod.rs`
  — it was `|_| Ok(())`) that re-activates via `em`'s own EPREFIX-aware `select`
  machinery, which re-roots the wrapper correctly. Verified live: both gcc and
  binutils wrappers now resolve and run under `--root`; `--prefix` unaffected
  (idempotent re-activation, no regression). **Still open, separately**:
  nothing in the native (`chost == cbuild`) build path prefers this wrapper
  over the host's own `gcc` on `$PATH` — out of scope for this fix, tracked in
  [[select-toolchain]].
- ✅ **`em select` used `roots()` instead of `outer_roots()` everywhere —
  FIXED 2026-07-17 (`75067f1`).** Found live verifying the new `pkgconf`
  wrapper across `--root`/`--prefix`/`--local` (per Luca: "what is important
  is that `--local`/`--prefix`/`--root` work as intended"): a bare `--prefix
  DIR select pkgconf show --target T` reported "not set" even though
  `crossdev --setup` had *just* created the wrapper. Root cause, confirmed
  by debug-printing the resolved `Roots`: clap's derive macro applies a
  `select` subcommand's own `--target` value to *both* that local field
  and the global `Cli::target` (same long name "target", even though only
  the global one has the `-T` short alias) — so `em select <module> ...
  --target T` was silently also setting `Cli::target`, and `roots()`
  treats a non-`None` `Cli::target` as "substitute the sysroot in", which
  `select` never wants (it only means "which target's own config-root
  state", never "merge into this sysroot"). Affected every `select`
  subcommand with its own `--target` option — **`compiler`/`binutils`/
  `linker` had this bug too**, pre-existing since 2026-06-24, not something
  introduced this session; a "control" test against `compiler show`
  happened to report a plausible-looking answer only because this exact
  host has stray real system crossdev artifacts (`/etc/env.d/gcc/config-
  riscv64-unknown-linux-gnu`) that coincidentally matched. Fixed by
  switching every `select::*::run()`/helper from `globals.roots()` to
  `globals.outer_roots()` (which never consults `Cli::target` at all, so
  it's immune regardless of whether the flag-name collision itself gets
  addressed) — `env_d.rs`'s `run_list`/`run_show`/`run_set`, `select/
  mod.rs`'s `config_portage_dir`/`is_prefix_context`, `repos.rs`/`clang.rs`
  (no `--target` of their own, but fixed for the same underlying reason),
  and `pkgconf.rs`. Verified live across all three topologies: `--root`
  unaffected (still correctly requires `--config-root` for a bare
  invocation, matching real eselect), `--prefix` now resolves correctly
  without `--config-root` (the broken case), `--local` verified end-to-end
  including a real wrapper execution.
- ✅ **`em stages --stage1 --cross` install-order/preflight bugs — FIXED
  2026-07-03.** Confirmed with real portage (`qdepends`) that the apparent
  `util-linux` ↔ `python` cycle was never real: util-linux's `python? (
  ${PYTHON_DEPS} )` doesn't apply with `python` off. Root cause: Level-C
  `--autosolve-use` ceding (`cede_required_use`,
  `portage-cli/src/query/depgraph/repo.rs`) scanned the *whole*
  `REQUIRED_USE` tree for flags to cede whenever *any* clause was violated,
  instead of just the violated clause(s) — util-linux's independently-satisfied
  `python? (...)` got ceded as a side effect of its unrelated, genuinely-violated
  `su? ( pam )` clause, fabricating a phantom `util-linux -> python` DEPEND
  edge that corrupted install order for the whole cluster (which is also
  what produced the "USE-dep conditional-default syntax" symptom below —
  once ordering is fixed, those self-resolve). Fixed by scanning only
  `ru.unsatisfied(&enabled)`'s clauses. Verified: phantom edge gone, order
  correct, real (non-pretend) `em stages --stage1 --cross riscv64...` now
  passes `preflight::check` clean and starts building (gcc underway).
  [[stage-build-shakeout]] finding #15.
- 🔴 **Profile/USE vs the releng stage profile.** em `@system` matches 175/180 of
  the real arm64 stage3; the 5 em-only (nghttp2/3, ngtcp2, libusb) are the default
  profile enabling curl `http2/http3/quic` + libusb vs the lean releng profile.
  Resolve against the same profile for apples-to-apples. [[stage3-vs-real-comparison]]
- 🔵 cosmetic: glibc post-install `failed to redirect to <root>/etc/hosts` (no
  /etc/hosts in a fresh ROOT). [[em-root-characterization]]

## Merge / build robustness (found in the @system shakeout)

- ✅ **CBUILD=CHOST** (`50081f2`) — python configure "cross" on native `--root`.
- ✅ **fowners non-fatal unprivileged** (`efdeb37`) — pam/eselect.
- ✅ **Merge unlink-before-overwrite** (2026-06-28). Re-merging over an existing
  read-only file (`bash` → `usr/bin/bashbug`, mode 0555) used to `Permission
  denied`: `walk_image` did a bare `std::fs::copy`, which opens the dest
  `O_WRONLY|O_TRUNC` → EACCES. Now unlinks the dest first (portage's behaviour),
  so the copy creates a fresh file (needs only directory write perm). Validated
  e2e: re-merge over `-r-xr-xr-x` files succeeds. [[stage-build-shakeout]]

## Distfile fetcher [[distfile-fetch-reliability]]

- ✅ **GENTOO_MIRRORS from make.globals** (`e0bae58`) — mirror fallback existed but
  the list was empty (never read make.globals).
- ✅ **Mirror filename-hash layout** (`distfile-fetch-reliability` C.4) — modern
  hashed-layout mirrors (`distfiles/<blake2b>/...`) honoured; flat path kept as a
  legacy fallback.
- ✅ **sourceforge HTML body rejected** (C.5) — a 2xx with `Content-Type: text/html`
  is treated as a fetch failure and the next URL is tried.
- ✅ **Corrupt partial refetched** (C.3) — resume only a size-plausible partial; on
  any verify failure discard + one fresh non-Range download.
- ✅ **Success-after-fallback registered** (C.2) — the per-distfile URL loop
  early-returns `Ok(Downloaded)` on the first success.
- ✅ **Computed `SRC_URI` (facet A) — DONE** (`2965fa2`, 2026-06-15). Global-scope
  loop/array-join construction (bash's `${my_urls[*]}`, the `bash53-001..015`
  patch loop) is evaluated correctly: the fetch phase reads `SRC_URI` from the
  already-sourced live shell via `is_phase_sourced`, not by re-sourcing. The
  original bug was re-sourcing no-op'ing eclasses (their include guards fire on
  the second pass) and dropping global-scope effects — leaving SRC_URI stale/empty.
  Verified: `em ebuild bash-5.3_p15.ebuild fetch` computes the full SRC_URI
  (tarball + 15 patches). Empty SRC_URI remains a legitimate state (84 meta/virtual
  ebuilds have `SRC_URI=""`), so no fail-fast on empty.
- ✅ **`em select mirrors` — DONE.** `list`/`show`/`set` with `--country`/`--region`
  filters; mirror list from Gentoo's XML API (`portage_distfiles::MirrorList`),
  writes `GENTOO_MIRRORS` to make.conf. `select/mirrors.rs`.

## Binhosts (fast stage3/stage4) [[em-stages-and-binhosts]]

- ✅ Producer: **`em -b` GPKG writer — DONE** (2026-06-28). New **`portage-binpkg`**
  crate (published `0.1.0` on crates.io) with the GLEP 78 writer (`write_gpkg`):
  container = plain tar `<PF>/gpkg-1` → `metadata.tar.zst` → `image.tar.zst` →
  `Manifest`, image via `tar --xattrs` pax (caps/devnodes), metadata = the VDB dir,
  `DATA … SHA512 … BLAKE2B` Manifest. `-b/--buildpkg` wired after qmerge (in the
  privilege session). **Validated: host portage reads, Manifest-verifies, and
  decompresses em's gpkg.** VDB enrichment 16→30 fields (PF, CHOST/C*FLAGS, FEATURES,
  INHERITED, DEFINED_PHASES, repository, NEEDED/NEEDED.ELF.2/REQUIRES/PROVIDES via
  the `object` ELF scan, the `.ebuild`). Format spec in
  [[fakeroot-privilege-backends]].
  - *VDB field follow-ups (down-scoped after investigating portage source):*
    `REPO_REVISIONS` is **not** a per-package VDB field — it is the repo
    git-revision-at-build-time, needs sync-history infra em lacks (the global
    `/var/lib/portage/repo_revisions`, which `emaint revisions` purges) → deferred.
    `IUSE_EFFECTIVE` is real but needs profile USE_EXPAND/arch plumbing the merge
    path doesn't thread → follow-up, not blocking.
- ✅ **GPKG metadata reader (`read_metadata`) + `em maint binhost` `Packages`
  index — DONE** (2026-06-28). `read_metadata` extracts a container's inner
  `metadata.tar.<c>` and returns the flat VDB field map (skips
  `environment.bz2`/the copied `<PF>.ebuild`). `em maint binhost` walks `PKGDIR`
  for `*.gpkg.tar`, reads each, and writes the `Packages` index in portage's
  `binarytree` format (sorted header + sorted per-CPV entries, `DESC`/`REPO`
  translations, `BUILD_ID`, container `MD5`+`SHA1`+`SIZE`+`MTIME`). **Validated
  against host portage: `binarytree.populate()` parses em's `Packages`, indexes
  the cpv, resolves SLOT/DESC/REPO/USE, zero invalids.** Commits `1b46a62`
  `413364f`.
- ✅ **`-k`/`--usepkg` local binpkg reuse — DONE & validated e2e.** The validity
  check (version matches by cpv lookup; USE restricted to the package's IUSE must
  match the desired USE — portage's `_match_use` bug-#453400 rule, so a stale-USE
  binpkg is rebuilt) + `BinpkgIndex` (reads the `Packages` index, scans PKGDIR as
  fallback) + `merge_binpkg` (extracts the image post-clean, runs only `qmerge`).
  `portage_binpkg::extract_image` added. **Validated end-to-end**:
  `em -b sys-apps/gentoo-functions` (build) → `em -k` into a fresh root merges
  byte-identical payload (matching md5sums, populated CONTENTS, no compilation).
  Commits `434ab22` + `5c74a01` (the latter fixed run_inner's clean wiping the
  pre-extracted image). [[em-stages-and-binhosts]]
- ✅ **`-g`/`--getbinpkg` remote consumer — DONE & validated e2e.** Transport
  (`portage_distfiles::fetch_index` — `Packages.gz` then `Packages`, gzip) +
  `fetch_binpkg` (streamed download via `.partial` rename). `RemoteBinpkgIndex`
  (same `use_compatible` rule, resolves to a download URL). `portage_binhosts`
  reads `PORTAGE_BINHOST`. Merge loop: `-g` implies `-k` (local overrides
  remote), `-G` is binpkg-only (no source fallback). **Validated**: served
  `Packages`+gpkg over http, `em -g` merged byte-identical payload; `-G` with no
  matching binpkg refuses to build. Commit `311d0f1`.
  - ✅ **`binrepos.conf`** (modern format) — DONE (2026-07-15). `portage_binhosts`
    now reads `binrepos.conf` (global defaults, then
    `${PORTAGE_CONFIGROOT}/etc/portage/binrepos.conf`, either a file or a
    directory of `*.conf` files) and combines it with legacy `PORTAGE_BINHOST`
    in real portage's own priority order (`BinRepoConfigLoader`/`bintree.py`:
    ascending sort by `(priority, name)`, then reversed for consumption —
    verified against the real source, not assumed; for a plain
    `PORTAGE_BINHOST` list with no `binrepos.conf` the double-reversal cancels
    out to the original left-to-right order). New `BinRepoEntry` type
    (`name`/`sync_uri`/`frozen`/`verify_signature`); `frozen`/
    `verify_signature` are parsed and carried but not yet *enforced* (need the
    still-open local-index-cache and GPG-verify items below, respectively).
    Shared a generic single-level INI-section parser
    (`portage_repo::ini::{collect_conf_files, merge_sections}`) out of
    `ReposConf`'s own implementation rather than duplicating it. 8 new unit
    tests (the priority/reversal algorithm, dedup against an explicit
    section's `sync-uri`, missing-`sync-uri` skip, case-insensitive
    `frozen`/`verify-signature`, plus one real-file-on-disk integration test
    through the actual `portage_binhosts` entry point, not just the pure
    combining core). No `%(VAR)s` interpolation and no `[DEFAULT]`-section
    inheritance — same simplification `ReposConf` already makes for
    `repos.conf`, no configured value observed in practice needs either.
  - ✅ **`URI` header BASE_URI override** — DONE 2026-07-18.
    `RemoteBinpkgIndex::new` prefers the index header `URI` over the configured
    `sync-uri` (Portage `bintree.py`: `header.get("URI", base_url)`).
  - ✅ **Auto-reindex after `-b`/`-B`** — DONE 2026-07-18. `write_binpkg` calls
    `index_pkgdir` after each GPKG so `-k`/`-g` see new containers without a
    separate `em maint binhost`.
  - ✅ **`em stages` default `--buildpkg`** — DONE 2026-07-18.
    `merge_merge_flags_with(..., force_buildpkg: true)` on the stage1 path so
    each stage run seeds PKGDIR (catalyst/crossdev-stages model).
  - ✅ **CHOST gate on reuse** — DONE 2026-07-18. `find_reusable` rejects a
    binpkg whose `CHOST` differs from make.conf/env CHOST (either side empty
    skips the gate). Not full ABI/PROVIDES matching.
  - ✅ **Remote-index freshness** — DONE. `portage_distfiles::fetch_index` now
    takes an `if_modified_since: Option<&str>` and returns an `IndexFetch`
    enum (`NotModified` | `Fresh { text, last_modified }`), sending
    `If-Modified-Since` and recognizing HTTP 304 on both the `.gz` and plain
    `Packages` code paths. `portage-distfiles`'s `binhost_cache` module
    (`fetch_index_cached`, **relocated from `portage-cli` 2026-07-15** —
    it only needed `sync_uri`/`frozen`/`eroot`, not `&Cli`, so it now lives
    next to the `fetch_index` it wraps) implements real portage's exact
    decision tree from
    `bintree.py::_populate_remote_repo`: a local cache at
    `${EROOT}/var/cache/edb/binhost/<host>/<url-path>/Packages` carries
    `TIMESTAMP` (server generation time, echoed back as the next
    `If-Modified-Since`), `DOWNLOAD_TIMESTAMP` (our last fetch/revalidation),
    and `TTL` (freshness window). `frozen` or a live `TTL` skips the network
    entirely; otherwise a conditional GET either revalidates (304, cache
    kept, `DOWNLOAD_TIMESTAMP` bumped) or returns fresh content (cached,
    `TIMESTAMP` backfilled from the response's `Last-Modified` if the index
    itself didn't carry one). A fetch failure with a stale local cache falls
    back to it with a warning rather than failing the whole `--getbinpkg`
    run. `merge/mod.rs`'s `-g`/`-G` fetch loop now calls
    `fetch_index_cached` per configured binhost instead of the raw
    unconditional fetch. Covered by unit tests (header parse/write, cache
    path derivation) plus a hand-rolled TCP mock-server integration test
    (`portage-distfiles/tests/conditional_fetch.rs`) proving a real
    `If-Modified-Since` round trip surfaces as `NotModified`.
  - 🟡 **gpkg GPG signature verify** — `binpkg-request-signature` FEATURE / repo
    `verify-signature=true` (default-on in shipped config) drops remote XPAK and
    GPG-verifies gpkg at unpack. em accepts unsigned. Last (with signing).
  - ✅ **`-K`/`--usepkgonly` enforcement** — local-only binpkg mode, no source
    (`merge/mod.rs`: `enforce_no_source = usepkgonly || getbinpkgonly`). Symmetric
    to `-G`.
  - 🟡 **Build-env / multi-march binpkg identity.** **Partial (vibe 2026-07-19):**
    multi-instance index (`cpv → Vec`), sokgi `build_env_key`
    (CFLAGS/CXX/LD/RUST), gate in `find_reusable`, prune by
    `(cpv, chost, key)`, per-entry CHOST/CFLAGS from entry make.conf.
    **Still open:** per-entry/host PKGDIR dual index (S1/S4), max-BUILD_ID
    among matches, header parse harden, RUSTFLAGS in VDB, key strictness
    (full hash vs ISA-subset). Design + residual: [[binpkg-subtargets]].
- ✅ **`em maint binpkg` tooling** — DONE. `em maint binpkg {verify,list,prune}`,
  an em-only extension (no real `emaint` module covers this; its own `emaint
  binhost` only regenerates the index). `verify [--fix]` recomputes each
  indexed container's size/MD5/SHA1 and compares against the `Packages`
  index's recorded values (the same size-then-digest order as
  `_emerge/BinpkgVerifier.py`), reporting ok/corrupt/missing counts; without
  `--fix` a corrupt/missing binpkg fails the run (script-checkable), with
  `--fix` corrupt containers are quarantined (renamed `<file>.corrupt`) and
  the index is regenerated so missing/corrupt entries drop out. `list` prints
  a cpv/build-id/size/path table over the index. `prune [--dry-run]` collapses
  leftover multi-`BUILD_ID` containers for the same cpv down to the newest one
  (em's own reuse model keeps at most one instance per cpv — see the
  `binpkg-multi-instance` note above) and reindexes; this is *not* a full
  `eclean-pkg` port (no installed-set/age/size pruning — gentoolkit isn't
  installed on this host to verify parity against, and no confirmed gap
  depends on it). `verify`/`list`/`prune`'s core logic takes `pkgdir`/`chost`
  directly (not `&Cli`), so all three are covered by real-gpkg-container
  integration tests seeding containers via `portage_binpkg::write_gpkg`,
  corrupting/duplicating/removing them, and asserting the reported outcome —
  this caught a real bug pre-merge (`Utf8Path::with_extension` only replaces
  the last `.tar` of `.gpkg.tar`, silently producing
  `foo.gpkg.gpkg.tar.corrupt`; fixed to a plain string-append).
  **Relocated (2026-07-15):** this logic, plus the `Packages` index
  parser/writer and `BinpkgIndex`/`RemoteBinpkgIndex` USE-reuse matching that
  used to live in `portage-cli/src/binpkg.rs`/`maint/{binhost,binpkg}.rs`,
  moved into the standalone `portage-binpkg` crate (`index`/`scan`/`regen`/
  `maint` modules) — it has no `&Cli`/fork dependency, so it belongs with the
  GPKG reader/writer it already builds on rather than in the CLI crate.
  `verify`/`list_index`/`prune` now return structured reports instead of
  printing; `portage-cli/src/maint/binpkg.rs` is a thin formatter over them.
  `portage-cli/src/binpkg.rs` keeps only what genuinely needs `&Cli`
  (`PKGDIR` resolution, `binrepos.conf`/`PORTAGE_BINHOST`).
- ✅ **Library-relocation pass, round 2 (2026-07-15):** a full survey of
  `portage-cli/src` for logic with no real `&Cli` dependency found a much
  bigger candidate (`query/depgraph/*`, 8.5k lines, effectively zero `Cli`
  coupling — see the dedicated `portage-resolve` entry below for where it
  actually belongs, corrected from an initial too-hasty "portage-solver"
  guess) plus the build/merge engine
  (`ebuild.rs`/`merge/mod.rs`/`postprocess.rs`/`elfscan.rs`) as future,
  larger-effort moves. This round did the small, mechanical ones:
  `use_flags.rs`'s USE add/remove algorithm → `MakeConf::apply_use_changes`
  (`portage-repo`, alongside the `MakeConf` it edits); `package_env.rs`
  (whole file, pure) → `portage_repo::env_files_for`; `binhost_cache.rs` →
  `portage_distfiles::fetch_index_cached` (decoupled from the CLI-only
  `BinRepoEntry` type — takes `sync_uri`/`frozen` directly now). All test
  coverage moved with the logic (portage-repo +9, portage-distfiles +6,
  portage-cli −12 tests), plus a live smoke test of `em use --add/--remove`
  against the real release binary. `search.rs` was surveyed too but turned
  out to be presentation logic (terminal color/formatting via `anstream`/
  `style.rs`) tightly interleaved with its matching logic, not a clean
  mechanical move — deferred, would need a real pure/display split first.
  `elfscan.rs` (~210 lines, zero `Cli`) was also considered for
  `portage-vdb` since its output fields map onto `MergeSpec`'s by name —
  rejected: that's just a shared naming convention, not a real code
  dependency (`elfscan.rs` doesn't use `portage-vdb` at all), and it would
  saddle an otherwise-lean crate with the `object` crate's ELF/COFF/PE
  parsing for no functional reason. Left in `portage-cli` pending a fuller
  audit of what else might want `object`-based binary parsing (QA checks,
  a `revdep-rebuild`/`scanelf` workalike, …) so the crate boundary can be
  decided once, not piecemeal.
  Also found live, and **fixed same session**: `em use`/`em pkg
  {use,keywords,mask,env} add` all panicked on `--help` or any invocation in
  debug builds — clap's own `-a`/`--add` short flag collided with the
  global `-a`/`--ask` flag (pre-existing, confirmed present 10+ commits
  back). Root cause wasn't the short-letter collision itself but that
  `--ask` was `global = true` on `Cli` at all: unlike `--pretend`/`--root`/
  `--privilege` (genuinely meaningful everywhere), `--ask` only means
  anything to a merge-shaped command, so `global` inherited it into every
  config-editing subcommand's argument set whether or not that subcommand
  read it. Fix: moved `ask` into the existing `MergeFlags` mixin (already
  flattened into `Cli`/`CrossdevArgs`/`ToolchainArgs`/`StagesArgs`, with the
  established `merge_merge_flags` OR-merge precedence for "either position
  works") instead of a bare `global` field — `use`/`pkg *` no longer
  advertise or accept `--ask` at all, and the real consumers
  (`emerge.rs`'s bare-atom flow, `crossdev/config_plan.rs`'s config-write
  confirm) read the already-merged value. `--pretend` stayed global
  (genuinely broad, no collision found). Verified live: `em use --help`/
  `em pkg use --help` no longer panic in a debug build, `-a`/`--ask` still
  parses both before and after `crossdev`, full workspace test suite green.
- ✅ **`query/depgraph/*` → `portage-resolve` (staged migration, complete 2026-07-16)**
  — a second opinion (Fable, 2026-07-15) on the round-2 survey's too-hasty
  "belongs in `portage-solver`" guess found that's structurally wrong:
  `portage-solver`/`portage-atom-pubgrub` are both published crates with
  deliberately lean, publishable dependency sets, but the depgraph code
  needs `portage-repo` (unpublishable — brush git fork), so folding it into
  either would make a published crate unpublishable. Verdict: a **new,
  currently-unpublished-in-practice crate `portage-resolve`** sitting
  between `portage-atom-pubgrub`/`portage-repo`/`portage-vdb` and
  `portage-cli` — the resolution/policy compute layer (repo-fact adaptation,
  USE/keyword/mask policy, root-aware post-solve trimming, plan assembly),
  no clap/anstream dependency enforced at the boundary. **The crate now
  exists** (`portage-resolve`, placeholder `v0.0.1`, published to
  crates.io to reserve the name — empty, nothing moved yet).
  Per-file disposition (full detail in session transcript/Fable's report,
  not duplicated here): moves — `repo.rs`, `force_mask.rs`,
  `effective_use.rs`, `use_env.rs`, `installed.rs`, `conflicts.rs`,
  `subslot.rs`, `root_aware.rs`, `bdepend_trim.rs`, `depend_trim.rs`,
  `host_copies.rs`, `required_use.rs`, `download_size.rs`, `c7.rs`, most of
  `package_use.rs`, plus (outside the directory) `bdepend_avail.rs` and
  **`cli::Roots` itself** (moved, not mirrored — it's already portable, and
  duplicating its correctness-sensitive `satisfaction_root` policy table
  across two types would repeat this project's own "near-identical concepts
  drift" failure mode). Stays in `portage-cli`: `output.rs` (pure anstyle
  rendering, same precedent as `search.rs`), `autounmask.rs`, `package_use
  .rs`'s `report()`, and `mod.rs` itself for now (genuinely command-shaped:
  prints, writes config, sets exit codes — splitting it further is optional,
  highest-risk, deferred). Surprise finding: `overlay.rs` (204 lines)
  belongs in neither place — it's `portage-repo` territory, and the
  designated first migration slice. Staged order (7 stages, each
  independently green/revertible, ~6-7 sessions total):
  **stage 1 DONE (2026-07-15)** — `overlay.rs` moved to `portage-repo`
  verbatim (`overlay_entries`, made `pub`; its two helpers stay private to
  the new module). Sole caller (`repo.rs`'s `load_repos`) updated to
  `portage_repo::overlay_entries`. No unit tests existed for this file, so
  live-verified against a real overlay on this host instead of a synthetic
  repro (`/var/db/repos/crossdev`, a real crossdev overlay with no
  `metadata/md5-cache`, symlinked package dirs into `::gentoo`): `em -p
  cross-riscv64-unknown-linux-gnu/gcc` resolved correctly through the moved
  `master_cache_entry` symlink-resolution path.
  **stage 2 DONE (2026-07-16)** — `Roots` (struct + impl) moved from
  `portage-cli/src/cli.rs` into `portage-resolve/src/roots.rs` verbatim,
  `pub(crate)` methods bumped to `pub` (`config_root_explicit`,
  `is_overlay`, `is_self_contained_root`,
  `with_own_config_root_if_self_contained`, `satisfaction_root`). Since
  its fields are private and construction now crosses a crate boundary,
  added `with_*` builder methods (one per field, `mut self -> Self`,
  matching the pre-existing `with_own_config_root_if_self_contained`
  idiom) plus two new getters (`broot()`, `is_cross_arch()`) that didn't
  exist before because the impl itself used to reach the fields directly.
  The 3 `#[cfg(test)]`-gated test constructors (`for_test`,
  `for_test_root_with_broot`, `for_test_overlay`, used by 7 other files'
  own tests) became `#[doc(hidden)] pub` instead — `#[cfg(test)]` doesn't
  survive a crate boundary (it's only `true` while *that* crate itself is
  under test). `portage-cli`'s 4 real construction call sites
  (`roots`/`outer_roots`/`base_roots`/`broot`) rewritten from struct
  literals to builder chains; ~15 consumer files across `portage-cli`
  updated from `use crate::cli::Roots` to `use portage_resolve::Roots`
  (mechanical). **Consequence surfaced, not previously concrete**: this is
  the point `portage-resolve` actually gained a `portage-repo` dependency
  (for `Roots::repos_conf`'s `Result`/`ReposConf`), so `publish = false`
  is now set — the placeholder `v0.0.1` on crates.io was the last
  publishable version, exactly as Fable's plan flagged as the eventual
  outcome. Live-verified against the real binary across all four root
  topologies (bare, `--root`, `--target` sysroot substitution against a
  real pre-built `/usr/riscv64-unknown-linux-gnu`, `--prefix`, `--local`)
  — all resolve and plan correctly. Full workspace check/clippy -D
  warnings/fmt/test clean, no tests lost (`portage-cli`'s own lib: 196).
  **stage 3 DONE (2026-07-16)** — `bdepend_avail.rs` (BROOT/within-run
  `BDEPEND` availability checks, `Avail` + its free functions) moved
  verbatim into `portage-resolve`. `entry_satisfied` (previously
  `pub(crate)` for no reason any consumer needed — grepped, only used
  within the file itself) demoted to private; everything else that was
  actually consumed cross-module (`broot_vdb_packages`, `entry_satisfied`
  was the one exception) bumped from `pub(crate)` to `pub`. 5 consumer
  files (`preflight.rs`, `query/depgraph/{bdepend_trim,depend_trim,
  host_copies,installed}.rs`) updated to `use portage_resolve::...`.
  All 18 tests moved with the logic (verified via a `git stash`
  before/after comparison of the exact `portage-cli`-lib + `portage-resolve`
  totals, not just "still green" — 196 before == 178 + 18 after). Live-
  verified `--with-bdeps` across bare/`--root`/`--prefix` topologies
  against the real binary. Full workspace check/clippy -D warnings/fmt/test
  clean.
  **stage 4 DONE (2026-07-16)** — the big one: `repo.rs` (1676 lines —
  `RepoData`/`Adapter`/`AcceptKeywords`/`AcceptLicenses`/`load_repos`/
  `target_package`/`find_autounmask_candidates`/`cpns_for`/`find_cache`/
  `mask_matches`/`is_masked`/…), `force_mask.rs` (`ForceMask`), and
  `effective_use.rs` (`effective_use`/`EvaluatedDeps`/`apply_ceded`) all
  moved verbatim into `portage-resolve`. Nearly every `pub(super)` item
  bumped to plain `pub` (fields too, on `RepoData`/`Adapter`/`ForceMask`)
  since `mod.rs`/`output.rs`/`autounmask.rs` — which stay in `portage-cli`
  and aren't moving until a possible future stage — construct `Adapter`
  via a bare struct literal and read `RepoData`'s fields directly; no
  invariant was being protected by the old `pub(super)`, so this cost
  nothing. `AcceptKeywords::from_global` (a `#[cfg(test)]` helper also
  called from `portage-cli`'s own `c7.rs`/`host_copies.rs` tests) got the
  same `#[doc(hidden)] pub` treatment `Roots`'s test constructors did in
  stage 2, for the same reason.
  **Key simplification that avoided touching ~10 other files' call
  sites**: rather than flat-re-exporting individual names at the crate
  root (the stage 2/3 pattern), `portage-resolve` exposes these as real
  `pub mod repo;`/`force_mask`/`effective_use` module paths. `query/
  depgraph/mod.rs`'s old `mod repo; mod force_mask; mod effective_use;`
  became one line, `use portage_resolve::{effective_use, force_mask,
  repo};` — kept deliberately non-`pub`, matching the old bare `mod`
  privacy level, so every existing `super::repo::X`/`super::force_mask::
  X`/`super::super::repo::X` reference throughout the *other*
  not-yet-moved files (`output.rs`, `autounmask.rs`, `package_use.rs`,
  `download_size.rs`, `required_use.rs`, `c7.rs`, `host_copies.rs`,
  `bdepend_trim.rs`, `depend_trim.rs`, `use_env.rs`) kept working
  completely unchanged — Rust's privacy rule (private item visible to its
  defining module and all descendants) applies transparently through a
  `use`-bound module alias exactly like it did through a real `mod`.
  Confirmed via a real compile, not just reasoning: zero of those files
  needed touching. Test-count accounting done precisely (not just "still
  green"): 178 (`portage-cli`) + 18 (`portage-resolve`) before → 160 + 36
  after — 18 tests moved, none lost (counted `#[test]`/`#[tokio::test]`
  in the 3 moved files directly: 11 + 5 + 2 = 18, exact match). Added
  `tokio` (`macros`, `rt-multi-thread`) as a dev-dependency for
  `#[tokio::test]` in the moved `load_repos` tests, plus `gentoo-core`/
  `portage-metadata` as real dependencies. Fixed ~30 new `missing_docs`
  warnings (`portage-resolve` has `#![warn(missing_docs)]`, `portage-cli`
  doesn't — every newly-`pub` item needed a real doc comment, not a
  rubber-stamp one). Live-verified against the real binary: plain
  resolve, a USE-flag-rich package, `--autosolve-use` (Level-C ceding),
  cross-`*` alias injection (`load_repos`'s in-memory crossdev-symlink
  equivalent), `--with-bdeps` full multi-package resolution, and
  `--autounmask` reporting — all correct. Full workspace check/clippy -D
  warnings/fmt/test clean.
  **stage 5 DONE (2026-07-16)** — `use_env.rs` (profile/`make.conf`/
  `package.*` reading into `UseEnv`), `installed.rs` (VDB-backed
  target/host/sysroot views + `action_tag`), `conflicts.rs` (post-solve
  reverse-dep conflict detection), `subslot.rs` (`:=` slot-operator
  rebuild detection) all moved verbatim. Same visibility-bump treatment
  as stage 4 (nearly every `pub(super)` item and its fields → `pub`,
  since `mod.rs`/`output.rs` — staying in `portage-cli` — construct
  `ProposedPkg`/read `Conflict`/`VdbEntry`/`SubslotRebuild` fields
  directly); one over-broad `pub(super) fn load_dep_list` (only ever
  called within its own file) demoted to private, matching the
  `entry_satisfied` precedent from stage 3. Same module-path trick as
  stage 4 (`use portage_resolve::{conflicts, effective_use, installed,
  repo, subslot, use_env};` in `mod.rs`, replacing 6 `mod X;`
  declarations) — again zero of the other not-yet-moved files
  (`output.rs`, `autounmask.rs`, `package_use.rs`, `download_size.rs`,
  `required_use.rs`, `c7.rs`, `host_copies.rs`, `bdepend_trim.rs`,
  `depend_trim.rs`) needed touching, **except** one `force_mask` binding:
  `mod.rs` itself never names `force_mask::` directly (only holds a
  `ForceMask` *value* from `use_env`), so the bare `use` was flagged
  unused by `mod.rs`'s own lint even though `c7.rs`/`host_copies.rs`
  still reach it via `super::force_mask`/`super::super::force_mask` —
  kept the binding alive with a documented `#[allow(unused_imports)]`
  rather than removing it and breaking those two files. Needed one new
  real dependency (`anyhow`, for `use_env.rs`'s error handling) plus
  `tokio` `macros`/`rt-multi-thread` were already present from stage 4.
  Fixed 18 new `missing_docs` warnings. Test-count accounting: 160 + 36
  before → 145 + 51 after (15 moved: 6+3+0+6 `#[test]`/`#[tokio::test]`
  counted in the 4 files, exact match). Live-verified against the real
  binary: plain resolve (R tag), `--deep` upgrade resolution (U tag),
  and confirmed a real `package.use` entry on this host
  (`cross-riscv64-unknown-linux-gnu/gcc`) is actually applied in the
  resolved USE flags — not just defaulting to empty. Full workspace
  check/clippy -D warnings/fmt/test clean.
  **stage 6 DONE (2026-07-16)** — `root_aware.rs` (`CrossContext`
  detection + `PlanEntry`/`build_plan`/`display_root`), `bdepend_trim.rs`
  (within-run BROOT `BDEPEND` trim, `TrimCtx`), `depend_trim.rs`
  (sysroot `DEPEND` trim), `host_copies.rs` (Tier-1 native-offset host
  build-copies closure walk) all moved verbatim — the `Roots`-consumer
  group. Same visibility-bump rationale as stages 4/5 (nearly every
  `pub(super)` item and its fields → `pub`, since `mod.rs`/`output.rs`/
  `autounmask.rs`/`package_use.rs` — staying in `portage-cli` — construct
  `CrossContext`/`PlanEntry`/`TrimCtx` via bare struct literals and read
  fields directly). Same module-path trick as stages 4/5: `mod.rs`'s
  `use portage_resolve::{...}` line grew `bdepend_trim, depend_trim,
  host_copies, root_aware`, replacing 4 more `mod X;` declarations, with
  zero of the other not-yet-moved files needing touching. New wrinkle
  this stage (first hit in stage 5's `installed.rs`, but affecting all
  4 files here): each of these 4 files, in its OLD home, referenced
  `portage_resolve::Roots`/`portage_resolve::Avail` etc. (correct from
  `portage-cli`'s perspective) — once the file itself moves INTO
  `portage-resolve`, that must become `crate::Roots`/`crate::Avail`;
  fixed on each file with `sed -i 's/portage_resolve::/crate::/g'`
  immediately after the `pub(super)`→`pub` bump, before compiling. Fixed
  9 new `missing_docs` warnings (mostly `TrimCtx`'s remaining
  undocumented fields in `bdepend_trim.rs`, and `PlanEntry`'s 3 fields
  in `root_aware.rs`). Test-count accounting: 145 + 51 before → 135 + 61
  after (10 moved: 3+2+3+2 `#[test]` counted in the 4 files, exact
  match). Live-verified against the real binary: a native `--root`
  offset resolve pulling host build-copies (`net-misc/curl` to a fresh
  `--root`, exercising `host_copies.rs`'s closure walk), a `--target
  riscv64-unknown-linux-gnu` cross resolve (confirming `root_aware.rs`'s
  `Root-aware cross plan: CHOST=... CBUILD=... sysroot=... target=...`
  banner and correct merge destination), and `--with-bdeps` under both
  a `--root` offset (BROOT-satisfied `BDEPEND` edges dropped by
  `bdepend_trim.rs`) and combined with `--target` (both `depend_trim.rs`
  sysroot-`DEPEND` and `bdepend_trim.rs` BROOT trims firing together) —
  all correct. Full workspace check/clippy -D warnings/fmt/test clean.
  **stage 7 DONE (2026-07-16) — final stage** — `required_use.rs`
  (`REQUIRED_USE` violation check) and `download_size.rs` (per-package
  distfile-size computation) moved verbatim, visibility bumped to `pub`
  (both were already fully doc-commented, so zero new `missing_docs`
  warnings this stage — the first stage with none). `package_use.rs`
  **split**: `PackageUseEntry`/`PackageUseLine`/`build_entries`/
  `cosolve_use_deps`/`CosolveOutcome`/`write` (plus their private
  helpers — `ver_str`/`req_targets`/`build_adjacency`/`parse_root_cpns`/
  `build_comments`/`merge_content`) moved into
  `portage-resolve::package_use`; `report()` stays in `portage-cli`'s
  now-much-smaller `package_use.rs`, since it's the one function in the
  file coupled to `anstream`/`output::C_*` (portage-resolve depends on
  neither, deliberately — see the crate doc). Unlike every prior stage,
  `portage-cli`'s `package_use.rs` isn't deleted, just shrunk: it
  re-exports the moved items (`pub(super) use portage_resolve::
  package_use::{PackageUseEntry, build_entries, cosolve_use_deps,
  write};`) so every existing call site in `mod.rs`
  (`package_use::cosolve_use_deps(...)`, `package_use::build_entries(...)`,
  `package_use::write(...)`, `package_use::report(...)`) kept working
  completely unchanged — this is why `package_use` was kept as its own
  local `mod package_use;` in `mod.rs` rather than folded into the
  `use portage_resolve::{...}` aliasing line the other three modules
  joined (a name collision between a local `mod package_use;` and a
  `use portage_resolve::package_use;` in the same scope isn't allowed;
  re-exporting through the local file sidesteps that instead of touching
  every call site). `c7.rs` (`#[cfg(test)]`-only cross-package `[flag]`
  co-solve corner-case spec, CC1-CC7) moved into `portage-resolve` as a
  `#[cfg(test)] mod c7;` in `lib.rs` — it directly exercises
  `cosolve_use_deps`, so it could only move once `package_use.rs`'s
  split (above) put that function in `portage-resolve` too; its own
  `super::force_mask`/`super::repo`/`super::package_use` references
  became `crate::force_mask`/`crate::repo`/`crate::package_use`. Fixed
  the same self-referential-path pattern as stage 6 in all four moved
  files (`super::repo::RepoData` → `crate::repo::RepoData` etc. in
  `package_use.rs`'s `cosolve_use_deps`/`req_targets` signatures).
  Test-count accounting: 135 + 61 before → 128 + 68 after (7 moved, all
  from `c7.rs`'s 7 `#[test]` fns — `required_use.rs`/`download_size.rs`/
  `package_use.rs` carried no tests of their own, exact match). Live-
  verified against the real binary: `-pv` on an already-installed
  package (0 KiB, confirming `download_size.rs`'s already-fetched
  short-circuit) and on an uninstalled one (correct non-zero per-package
  sizes and total), and `-p --autosolve-use www-client/firefox` — a real,
  large co-solve producing a correct multi-entry "USE changes are
  necessary" block with full required-by dependency chains, exercising
  the moved `build_entries`/`cosolve_use_deps`/`report` path end-to-end;
  `required_use::find_violations` runs unconditionally on every one of
  these real resolves (returning clean each time, as expected) and
  didn't crash or misbehave in any of them. Full workspace check/clippy
  -D warnings/fmt/test clean.
  **This closes Fable's 7-stage `query::depgraph` → `portage-resolve`
  migration plan.** Deliberately left in `portage-cli` (per that plan,
  not revisited this arc): `mod.rs` itself, `output.rs`, `autounmask.rs`
  — the optional `mod.rs` compute/render split is deferred until a
  non-CLI consumer of the resolve layer actually appears.
- 🔴 `em stages` defaults to `--buildpkg` so each run feeds the next; per-arch.
- 🔴 Signing/verify (`BINPKG_GPG_*`) — last (lives in `portage-binpkg`).

## Other open (pre-existing, related)

- 🟡 **Root topology refactor** — replace the flat `Roots` bag with a
  `RootTopology` enum (`Single`/`Dual`/`Overlayed` + `CrossArch`) whose
  variant answers `satisfaction_root(dep_class)`. Retires the `host_roots`
  positional threading across 9 files and the `host_aliases` invariant
  violation (`208c818`). Four behaviour changes come with it: `--root` stops
  moving config (portage `ROOT=` parity), `--local` becomes standalone (not
  overlay) so it works on a foreign host, host-python symlinks move from
  `--local` to `--prefix` (overlay borrows host tools; standalone must own
  its python), `--prefix` sets EPREFIX=P. Design: [[root-topology]] (doc)
  + [[root-topology-refactor]] (tasks).
- ✅ **`--local` spuriously engaging dual-root solver machinery — FIXED
  2026-07-16.** `CrossContext::detect()` treated any non-`/` target as
  needing dual-root bookkeeping, which wrongly included `--local` (whose
  BROOT is the *same* prefix as the target, not a genuinely different
  filesystem). `host_copies`'s Tier-1 walk then ran against `--local`'s own
  empty BROOT VDB and fabricated a parallel `@Host` copy of nearly the
  whole closure — dozens of spurious warnings, duplicate plan entries,
  preflight rejecting the order. Fixed by keying `active` off
  `broot != merge_root` instead of `target != "/"` (Fable-reviewed; see
  [[dedup-availability-walks]]'s 2026-07-16 entry for the full trace and
  fix detail). `--root`/`--prefix`/cross unaffected (broot genuinely
  differs there) — spot-checked live, no regression.
- ✅ **`install_order`'s SCC tie-break sweeping non-cyclic hard deps — FIXED
  2026-07-16** (`97e5f1b`, Fable-reviewed). The `--local` from-scratch
  ordering gap above traced to a real bug in `order_cycle`
  (`portage-atom-pubgrub/src/graph.rs`): one Tarjan pass over combined
  hard+soft edges can't tell "genuinely in an irreducible hard cycle" from
  "merely pulled into a bigger SCC by an unrelated RDEPEND cycle elsewhere"
  — an incidental RDEPEND cycle folded 114 of 229 packages into one SCC,
  and the flat `indeg_hard`/`indeg_all` tie-break inside it was a
  *preference*, not a *gate* (`sys-libs/gdbm` emitted before its own direct
  BDEPEND `app-portage/elt-patches`). Fixed with a local hard-only Tarjan
  restricted to each component's members, gating the heuristic so a hard
  predecessor outside a member's own hard-group is always respected. This
  exposed one further genuine hard cycle beyond `elt-patches`<->`xz-utils`
  (matches #34's already-known `gawk↔bison↔gettext↔libxml2↔meson↔python`
  pattern above): an 11-node `gcc`/`glibc`/`libgcrypt`/`libxslt`/`po4a`/
  `util-linux`/`python`/`meson`/`libxml2`/`gettext`/`texinfo` bootstrap
  cycle, confirmed against the real `::gentoo` ebuilds edge-by-edge. A true
  cycle has no valid total order — some edge is unavoidably violated,
  exactly like the existing `elt-patches`/`xz-utils` case. Not an `em` bug:
  it's the real reason Gentoo bootstraps from staged tarballs rather than
  absolute zero. See [[dedup-availability-walks]] for the full trace.
- ✅ **Native `--prefix` toolchain bootstrap — first-ever clean run,
  2026-07-16** (`087fdfb`/`2721574`/`c69919b`). The user's proposed
  recipe for setting up `--local` ("build a stage1 via `--prefix`, then
  use it as the starting point") had never actually been tested to
  completion (`todo/em-stages-scenario-matrix.md`: "`--prefix`/`--local`
  stage1 has never been end-to-end tested"). Running it surfaced two real
  bugs, both now fixed:
  1. `em toolchain --setup`'s installed-view incorrectly shared the
     host's VDB under `--prefix` (`Roots::base() == None` → `VDB(base) ∪
     VDB(target)`), so `virtual/os-headers` resolved as already-satisfied
     by the host's real kernel headers and `sys-kernel/linux-headers`
     never got merged into the prefix — glibc's own `--with-headers`
     pointed at an empty dir and failed to configure. Fixed with a
     dedicated `Roots::installed_view_target_only` flag (deliberately
     *not* reusing/mutating `base` itself — doing that first broke
     `build_sysroot()`/`ESYSROOT` for the same steps, doubling gcc's
     `--with-build-sysroot` path).
  2. Once glibc built, gcc's own self-build failed compiling
     `libiberty/obstack.c` against the just-installed target glibc's
     ABI-mismatched `obstack.h` — the *same* class of bug already fixed
     for `--root` on 2026-07-03 (`setup.rs`'s `BASHRC_PREFIX`/
     `self_contained` doc comment), now hit for `--prefix` because that
     topology's own `BASHRC_PREFIX` still unconditionally injects
     `CPPFLAGS="-I<prefix>/usr/include ..."` for every package, including
     the bootstrap's own gcc. Fixed by reusing the same self-contained-
     bootstrap signal to skip the config-overlay bashrc for just these
     steps (`ebuild::RootContext::self_contained_bootstrap`).
  Live-verified end to end: `em --prefix <dir> toolchain --setup` now
  builds baselayout → binutils → linux-headers/os-headers → glibc → gcc
  cleanly into a fresh `--prefix`, and the resulting
  `<prefix>/usr/bin/<chost>-gcc` runs. Real toolchain.eclass evidence
  (`! is_crosscompile && ! use prefix-guest && [[ -n ${EPREFIX} ]]`)
  confirms native+EPREFIX+non-guest is genuinely single-stage in real
  Portage too — an earlier "give Native the same phasing as Cross"
  hypothesis was checked against the eclass and found wrong, not pursued.

  **Also resolves the 2026-07-12 `crossdev --setup --local` open
  question** (`todo/em-stages-scenario-matrix.md`'s "`em crossdev --setup`
  across all three root-mode variants" entry, `99bcd06`): that entry asked
  why bare `--root` toolchain bootstrap got 30+ packages merged before
  failing while `--local` failed at preflight before merging *anything*,
  despite `broot` looking identically configured. Re-ran via
  `regression-matrix.sh` (below) — `--local`'s `crossdev --setup` still
  fails, but the failure list is now legible: it's exactly the
  already-known genuine hard-cycle members (`meson`, `gettext`,
  `elt-patches`/`xz-utils`, `glibc[cet]`, `python`) plus their downstream
  consumers (`cmake`, `elfutils`, `e2fsprogs`, …) — i.e. `--local`'s own
  *base* toolchain never finished (blocked by the same unavoidable cycle,
  see the `install_order` SCC entry above), so the cross toolchain built
  on top of it is missing the same things. Not a distinct `--local`-vs-
  `--root` inconsistency after all — a direct, expected consequence of
  the hard cycle, once the SCC/dual-root fixes made the failure legible
  instead of an opaque wall of unrelated-looking `DEPEND` entries.

  **New**: `regression-matrix.sh` (repo root) automates this whole
  cross-topology matrix (native `toolchain --setup`, `stages --stage1
  -p`, `crossdev --setup`) as a live regression check — plain `-p`
  checks alone would **not** have caught either of today's two real bugs
  (both only manifested in a real build), so the toolchain-bootstrap leg
  runs for real by default. `--local`'s known-partial outcome is checked
  against its specific expected signature (gdbm still orders after
  elt-patches; only known cycle members remain unsatisfied), not treated
  as a plain pass/fail.
- 🔴 **Parser audit pass** — review the recent burst of parser work (incremental
  `-*`, package.use/license/accept_keywords, @set expansion, USE-dep eval, IUSE
  defaults, make.conf sourcing, md5-cache) for PMS/portage faithfulness.
  [[parser-audit]]
- 🔴 clang linker config (Option B, `gentoo-linker.cfg`). [[select-toolchain]]
- See also [[nonemptytree-bdeps-gap]], [[em-emptytree]], [[build-clean-env]],
  [[crossdev-target]], [[cross-support-self-review]] for older open threads.
