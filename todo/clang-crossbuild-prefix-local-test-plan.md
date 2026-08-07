# Test plan — cross-emerge `llvm-core/clang` for riscv64, under `--prefix` and `--local`

STATUS: 🟡 both scenarios reached a terminal (blocked) state for this pass,
2026-08-06, against `master` at `9b2e4c3` (includes the BuildClass/
`PackageArch` fix, commit `1971b7c`, which this plan is meant to
live-verify — no regression found in what did run). Neither scenario
reached "confirm `-b` does the right thing": Scenario A is blocked by a real
build-directory race (finding #4, reproduced deterministically, **and
confirmed pre-existing — reproduces identically on `em` built from `cd9e0df`,
the commit right before glm-5.2's BuildClass refactor track — not a
regression from that work**); Scenario B is blocked by `--local`'s own
pre-existing bootstrap hard cycle (finding #6). Both are recorded as
findings, not worked around, per direction.

## Findings summary (as of 2026-08-06, mid-run)

Confirmed working:

- **Scenario A step 2** — `--prefix`, riscv64 crossdev toolchain bootstrap:
  clean, all 6 stages, real `riscv64-unknown-linux-gnu-gcc --version` runs.
  No regression from the `PackageArch`/BuildClass fix (`1971b7c`) this whole
  plan exists to verify.

Broken, confirmed with root cause (details + exact repro in the Execution
log below):

1. **`em setup -p` isn't a preview — it writes for real** and registers
   `em active` state. `dispatch.rs`'s `Applet::Setup` dispatch passes no
   pretend flag at all; every write in its sibling `crossdev/mod.rs`
   consistently gates behind `!globals.pretend` (10+ sites) — an isolated
   gap, not a spread pattern.
2. **`em -p --target T crossdev --setup` can't preview a never-before-
   initialized target — it just fails** (`no ebuilds in ::gentoo or
   overlays`), because the alias `repos.conf` entry is correctly *not*
   written under `-p`, but the next step's package-plan resolution still
   needs it to exist on disk. Only bites a first-time target; re-running
   `-p` after a real init works fine.
3. **The known dual-plan-entry bug** (`llvm-core/llvm` listed twice,
   2026-08-05 finding) **also hits `llvm-core/clang` itself, now confirmed
   with `--target` set too**, not just plain `--prefix`.
4. **Escalation of #3 — under real parallelism (`--jobs 80`) it's a genuine
   build-directory race, not just a wasted plan slot, and it blocks the
   goal package entirely.** Proved directly: the build.log for one of the 3
   packages that failed shows every phase logged **twice** in the same
   file — the host-arch and target-arch copies of the same package ran
   concurrently into one shared `WORKDIR` and collided. Root cause:
   `default_work_base()` keys the work directory only on outer-prefix +
   `category/pf`, with no distinction for which root a merge installs into.
   **Fix plan:** [[workdir-dual-root]] (per-target builddirs + lock/schedule
   like Portage; multi-`em` coordination later).
   Consequence: `llvm-core/clang` itself never got a single
   `Emerging`/`Completed` line — the run stopped at 66/136, blocked
   transitively by the 3 collided packages. **Reproduced deterministically**
   on a second, fully fresh sandbox with the identical command (no
   `--keep-going`): byte-for-byte the same 3 packages, same errors, same
   66/136 stopping point — not timing-sensitive noise, a reliable outcome of
   this dependency graph under `--jobs 80`. This is Scenario A's terminal
   state for this pass; the buildpkg-verification checklist could not run
   since the goal package never built.
5. **Process mistake, corrected**: retried with `--keep-going`, against this
   project's own standing convention — it cascaded into an unrelated
   `merged-usr vs split-usr` die on further packages instead of surfacing
   one clear thing. Killed it, threw the sandbox away rather than hand-patch
   it, rebuilt clean, retrying without `--keep-going`.
6. **`--local`: neither `crossdev --setup` nor the direct native `toolchain
   --setup` can bootstrap a fresh `--local` — both fail at the identical
   preflight check.** `crossdev --setup --local` fails exactly as it did on
   2026-07-12 (`sys-devel/gcc needs glibc[cet]`, `glibc needs python`,
   meson/gettext/elt-patches missing). Tested directly (not assumed): `em
   --local ... toolchain --setup` — the plain native bootstrap, no crossdev
   involved — hits the **identical** failure. Confirms this isn't a
   crossdev-specific gap: it's `--local`'s own genuine, already-documented
   11-node native-bootstrap hard cycle (`install-order-scc-tiebreak-fix`
   memory), hit at the earliest possible point. **Per direction, no
   workaround was attempted** — a fresh `--local` simply cannot bootstrap
   anything, native or cross, without either the `--prefix`-first workaround
   or an already-populated prefix to start from. This is Scenario B's
   terminal state for this pass.

## Execution log

- **Sandboxes**: fresh `em-clang-prefix`/`em-clang-local` (`sandbox setup
  --arch aarch64`), release `em` deployed to both, `em --help` sanity check
  passes in both.
- **Real bug found: `em setup -p` is not a preview — it writes for real and
  registers `em active` state anyway.** `em --prefix /root/xp setup -p`
  created `/root/xp/{etc,usr,var}` and wrote `~/.local/state/em/active*`
  registering `xp` as an active prefix, identical to a non-`-p` run.
  Root cause: `dispatch.rs`'s `Applet::Setup => setup::run(&globals.roots())`
  passes no pretend flag at all, and `setup::run`/`setup::bootstrap`
  (`portage-cli/src/setup.rs:212-222`) take only `&Roots` — there is no
  gate to check. Every write path in `crossdev/mod.rs` (its close sibling —
  `crossdev --setup` also implies `init_target`/`bootstrap`) consistently
  guards real writes behind `!globals.pretend` (10+ call sites); `setup.rs`'s
  `Applet::Setup` dispatch has no equivalent gate — an isolated gap, not a
  spread pattern. Not fixed here (out of scope for this pass); flagging with
  exact location for follow-up. **Consequence for this run**: proceeding
  from the now-real `/root/xp` rather than re-doing the "preview" step, since
  the side effect already happened.
- **Real bug found: `em -p --target T crossdev --setup` cannot preview the
  package plan for a target that has never been initialized before** — it
  fails outright instead of showing what would happen. `em -p --prefix /root/xp
  --target riscv64-unknown-linux-gnu crossdev --setup` printed the
  `>>> config changes:` preview correctly, then failed resolving step 1/6:
  `cross-riscv64-unknown-linux-gnu/binutils: no ebuilds in ::gentoo or
  overlays`. Root cause: `crossdev/mod.rs::setup`'s own comment confirms
  `init_target` is deliberately `-p`-aware and only *previews* the alias
  `repos.conf` entry under `-p` rather than writing it (correct, matches
  `--init-target`'s own documented pretend behavior) — but the very next
  step, `run_staged`'s package-plan resolution, still reads the real on-disk
  `repos.conf` to resolve `cross-<tuple>/binutils` etc., and since the alias
  was never actually written, that category genuinely doesn't exist yet to
  resolve against. Only reproduces for a target with **no prior
  `--init-target`/`--setup` run** — once the alias exists on disk from an
  earlier real run, `-p` previews fine (matches `docs/crossdev.md`'s
  documented "safe to re-run" example, which is implicitly the
  already-initialized case). Not fixed here; flagging with location.
  **Consequence for this run**: went straight to the real (non-`-p`)
  `crossdev --setup` for this first-time target, since `-p` cannot be used
  to preview it.
- **Scenario A, step 2 — full success.** `em --prefix /root/xp --target
  riscv64-unknown-linux-gnu crossdev --setup --jobs 8` completed the full
  6-step bootstrap (binutils → linux-headers → gcc-stage1 → libc → glibc →
  gcc-stage2), `EXIT=0`, `>>> cross toolchain riscv64-unknown-linux-gnu
  ready in /root/xp/usr/riscv64-unknown-linux-gnu`. Verified for real:
  `/root/xp/usr/bin/riscv64-unknown-linux-gnu-gcc --version` runs, reports
  `16.1.1_p20260718`. No regression vs. the 2026-08-04 informal run.
- **Scenario A, step 3, `-p` pass: much larger closure than the 2026-08-04
  informal run (~70+ packages vs. 18-20), and the known dual-plan-entry bug
  recurs, now confirmed under `--target` too.** `em -p --prefix /root/xp
  --target riscv64-unknown-linux-gnu -b llvm-core/clang` pulls in what's
  effectively a target `@system` (glibc, gnupg, python-3.14, perl, curl,
  rsync, sys-apps/portage, ...) plus the full LLVM/clang runtime stack —
  larger than the earlier informal run, not investigated further (plausibly
  a newer snapshot / different starting closure, not necessarily a
  regression; no prior `-p` baseline exists to diff against for this exact
  scenario). `llvm-core/clang-22.1.8` (and several `llvm-runtimes/*-config`
  packages) appear twice in the plan, byte-identical, once `to /root/xp/`
  (host) and once `to /root/xp/usr/riscv64-unknown-linux-gnu/` (target) —
  this is `prefix-clang-test-2026-08-05`'s already-documented "dual BDEPEND
  visibility" plan-duplication bug (previously found for `llvm-core/llvm`
  under plain `--prefix`, no `--target`); now confirmed to also occur for
  `llvm-core/clang` itself with `--target` set. Not a new bug — noting the
  broader reproduction scope. Whether it actually double-builds (previously:
  no, the second entry gets skipped via the VDB-presence check) is checked
  in the real run below.
- **Scenario B, step 0 — still blocked, unchanged from 2026-07-12.** `em
  --local /root/xl setup` then `em --local /root/xl --target
  riscv64-unknown-linux-gnu crossdev --setup --jobs 8` (real, not `-p` — the
  known first-init `-p` gap applies here too) built a large plan (~90
  packages, host-side BDEPEND tools for the toolchain bootstrap) then failed
  at the same preflight check, same shape as the 2026-07-12 finding:
  `sys-devel/gcc needs sys-libs/glibc[cet(-)?]`, `sys-libs/glibc needs || (
  python:3.14 python:3.13 python:3.12 )`, plus meson/gettext/elt-patches
  entries for several BDEPEND-class tools. Nothing landed since 2026-07-12
  fixed this — `--local`'s empty-VDB preflight gap is orthogonal to the
  `cede_required_use`/USE-fold work that did land in between. **Applying the
  documented workaround** (`native-prefix-toolchain-bootstrap-fix` memory):
  build a native toolchain via `--prefix` into the *same* directory
  `--local` already points at (`/root/xl`), so `--local`'s own VDB has a
  real compiler/meson/gettext/python to satisfy preflight against, before
  retrying `crossdev --setup --local`.
- **Scenario A, step 3, real run — new, more severe finding: the dual-plan-
  entry bug is a genuine build-directory race under real parallelism, not
  just a harmless wasted plan slot.** `em --prefix /root/xp --target
  riscv64-unknown-linux-gnu -b llvm-core/clang --jobs 80` reached 66/136
  packages then stopped: `llvm-runtimes/clang-rtlib-config-22`,
  `llvm-core/clang-linker-config-22`, and `llvm-runtimes/clang-stdlib-config-22`
  all failed identically (`die: newins: failed to install
  .../temp/.gentoo-*.cfg.new-src`) — and **`llvm-core/clang-22.1.8` itself
  never got a single `Emerging`/`Installing`/`Completed` line at all**
  (grepped the full log) — the actual goal package never built, blocked
  transitively by these three.

  Root cause, confirmed directly (not inferred): `cat
  /root/xp/var/tmp/portage/llvm-core/clang-linker-config-22/build.log` shows
  **every phase logged twice** (`pkg_pretend` ×2, `pkg_setup` ×2, ...,
  `src_install` ×2) in the exact same file — unambiguous proof the host-arch
  and target-arch plan entries (the byte-identical duplicate `-p` already
  showed, `prefix-clang-test-2026-08-05`'s known finding) actually ran
  **concurrently in the same work directory** this time, not "second entry
  skipped via VDB-presence check" as previously documented. One `src_install`
  won the race and presumably succeeded; the other's `newins` then failed
  because the source temp file the first one already consumed/moved was
  gone. Mechanism: `default_work_base(prefix)` (`portage-cli/src/ebuild.rs:213`)
  keys the work dir only on the **outer** prefix + `<category>/<pf>` — with
  no distinction for *which* root (host EROOT vs. target sysroot) a given
  merge is installing into, two plan entries for the same category/pf
  literally share one `WORKDIR`/`build.log`. Previously invisible because
  either the packages involved took long enough to naturally serialize under
  lower parallelism, or the "does not double-build" claim was only checked
  at a lower `--jobs` count — **this is the first time this bug's been run
  under real high parallelism (`--jobs 80`, at the user's suggestion, 128
  cores available) and it changed outcome from cosmetic to build-blocking.**
  Not fixed here (out of scope for this pass) — flagging with the exact
  mechanism and location, since "wasted plan slot" undersells it: any dual-
  role package pair with a short enough build time to overlap under real
  parallelism can hit this, and it blocks anything depending on it.
- **Mistake, corrected by Luca: retried with `--keep-going`, which is
  against this project's own standing convention (never pass `--keep-going`
  to `em stages`/toolchain-class runs) and made things worse, not better.**
  Pushing forward past the 3 raced failures didn't recover — it cascaded
  into a new, unrelated-looking `die: ERROR: 23.0 merged-usr profile, but
  disk is split-usr` on multiple subsequent packages, muddying the sysroot's
  state instead of surfacing one clear thing to investigate. Killed the run
  (`sudo kill -TERM`, clean exit, no orphaned processes) rather than let it
  keep going. Per the hard sandbox rule, not attempting to hand-patch or
  reason about exactly what's now inconsistent in `em-clang-prefix` —
  destroying and recreating it fresh, redoing the (already-proven-working)
  `crossdev --setup` step, then retrying the clang build without
  `--keep-going` so a real failure stops cleanly for inspection instead of
  cascading.
- **Scenario B, direction from Luca: do not attempt the `--prefix`-first
  workaround for the step-0 preflight failure. Document what works, what
  doesn't, and why — first.** A workaround build (`em --prefix /root/xl
  toolchain --setup --autounmask-write --jobs 80`, per
  `native-prefix-toolchain-bootstrap-fix`) had already been started and was
  in progress (7 of 8 packages, compiling `sys-devel/gcc`) when this
  direction came in — killed cleanly (`sudo kill -TERM` on the actual `em`
  process, confirmed gone, no orphans) rather than let it finish. **Scenario
  B's result for this pass is therefore the step-0 finding above as its
  terminal state**: `crossdev --setup` under a fresh `--local` fails at
  preflight, unchanged since 2026-07-12, root cause not yet re-confirmed
  against current code (only re-confirmed that the *symptom* is unchanged).
  The `--prefix`-first workaround remains a documented, untested-in-this-pass
  option for a future session, not something to reach for reflexively when a
  `--local` scenario fails.
- **Scenario B, direct test (per Luca's follow-up question — had skipped
  this): does `em --local ... toolchain --setup` (the native bootstrap,
  invoked directly, not via `crossdev`) fail the same way?** The `/root/xl`
  used for the killed workaround above was contaminated (18 packages already
  merged via `--prefix` before it was killed — confirmed via `var/db/pkg`)
  so this needed a genuinely fresh sandbox, not the same one. Destroyed and
  rebuilt `em-clang-local` clean, re-registered `--local /root/xl` (skeleton
  only), then ran `em --local /root/xl toolchain --setup --autounmask-write
  --jobs 80` directly. **Answer: yes, identical failure, same preflight
  check, same package set** (`sys-devel/gcc needs sys-libs/glibc[cet(-)?]`,
  `sys-libs/glibc needs || ( python:3.14 python:3.13 python:3.12 )`,
  `app-arch/xz-utils needs app-portage/elt-patches`, several
  `>=dev-build/meson-1.2.3` entries). This confirms `crossdev --setup
  --local`'s failure isn't a crossdev-specific gap layered on top of
  something else — it's `--local`'s own genuine, already-documented 11-node
  native-bootstrap hard cycle (`install-order-scc-tiebreak-fix` memory;
  `regression-matrix.sh` classifies this exact outcome `KNOWN-PARTIAL`, not
  `FAIL`) hit at the earliest possible point, before `crossdev` is even
  involved. Confirms Scenario B's terminal state precisely: **a fresh
  `--local` cannot bootstrap anything — native or cross — without either the
  `--prefix`-first workaround or an already-populated prefix to start from.**
- **Scenario A, step 3 — the build-directory race (finding #4) is
  deterministic, not a rare timing fluke, and is this pass's terminal state
  for `-b llvm-core/clang --jobs 80` under `--prefix`.** Retried on a fully
  fresh `em-clang-prefix` sandbox (destroyed/recreated, `crossdev --setup`
  redone clean, `EXIT=0`) with the identical command, no `--keep-going` this
  time. Result: **byte-for-byte identical failure** — same 3 packages
  (`llvm-runtimes/clang-rtlib-config-22`, `llvm-core/clang-linker-config-22`,
  `llvm-runtimes/clang-stdlib-config-22`), same exact `newins` errors, same
  stopping point (66 of 136, `63 ok, 0 already installed, 3 failed`). This
  confirms the race isn't timing-sensitive noise — it's a reliable outcome
  of this dependency graph under `--jobs 80`, reproducible from a clean
  state. Following the same "document, don't route around it" approach
  applied to Scenario B: **not** attempting a different `--jobs` value or
  any other workaround to dodge it in this pass. `llvm-core/clang` itself
  still never built, so the buildpkg-verification checklist below could not
  be run — this is Scenario A's terminal state for this pass, not a partial
  result to keep chasing with more retries.
- **Regression check, per Luca's question: does the build-directory race
  predate glm-5.2's BuildClass refactor track, or was it introduced by it?**
  Built `em` from `cd9e0df` (the commit immediately before Track A1,
  `b977fdb`, first BuildClass commit — a `git worktree` at that rev, kept
  separate from the working tree) and ran the identical scenario end-to-end
  against a third fresh sandbox (`em-clang-prerefactor`): topology setup,
  `crossdev --setup --jobs 80` (clean, `EXIT=0`, same 6 stages), then `-b
  llvm-core/clang --jobs 80`. **Answer: it already existed. `--prefix`
  cross-building clang did not fully work before the refactor either.**
  The same class of dual-role work-dir collision hit 2 of the 3 packages
  this run (`llvm-runtimes/clang-rtlib-config-22`,
  `llvm-core/clang-linker-config-22`; `llvm-runtimes/clang-stdlib-config-22`
  avoided the *install* collision this particular run — race timing isn't
  identical run-to-run even though it's reliably reproducible within one
  binary/codebase) and `llvm-core/clang` itself again never got a single
  `Emerging`/`Installing`/`Completed` line — confirmed via the same grep
  that found nothing on the post-refactor runs. **This confirms the race is
  a pre-existing bug, not a regression introduced by the BuildClass/
  `PackageArch` refactor** — the refactor didn't cause it and (so far)
  hasn't fixed it either.

  **Bonus finding, pre-refactor run only (not yet checked against current
  `master`): a third, distinct real bug.** `clang-stdlib-config-22`'s
  *host*-side instance (`to /root/xp/`, not the sysroot) merged successfully
  this run, but its `--buildpkg` binpkg-write step then failed: `tar:
  /root/xp/var/tmp/portage/llvm-runtimes/clang-stdlib-config-22/image/root/xp:
  Cannot open: No such file or directory` — reported as a non-fatal warning
  (the package still counts as merged), but the `.gpkg` binpkg was never
  written. The `image/root/xp` path looks like the same host-vs-target path-
  doubling class of bug as the earlier ESYSROOT/CBUILD findings
  ([[riscv64-clang-crossbuild-cbuild-esysroot-fixed]]), now hitting binpkg
  image-path computation for a host-arch dual-role package instead. Not
  investigated further or confirmed against current `master` this pass —
  flagging for a future session, since it directly affects `-b` correctness
  for exactly the scenario this whole plan is about.

## Goal

Confirm `em` can, for our usual cross target (`riscv64-unknown-linux-gnu`,
GCC model), under **both** `--prefix` and `--local`:

1. stand up the topology itself,
2. bootstrap a cross toolchain inside it (`crossdev --setup`),
3. **seed the target @system via `stages --stage1`** (`packages.build` —
   required: setup only lands `cross-<T>/*`; ordinary packages need real
   Cpns like `sys-libs/glibc` — see [[libcrypt-never-scheduled]],
   `docs/crossdev.md` 2026-08-07),
4. cross-build `llvm-core/clang` through that toolchain (the actual
   "cross-emerge of clang" goal), and
5. do all of that with `-b`/`--buildpkg` and have the resulting binpkg be
   correct and reusable — not just "the merge succeeded".

`--prefix` was already run through steps 2-3 once, informally, on
2026-08-04 (`riscv64-clang-crossbuild-cbuild-esysroot-fixed` memory, fixes in
commit `ad59ed0`) and is now also written up as a worked example in
`docs/crossdev.md`. This plan turns that into a repeatable checklist instead
of a one-off session result, and — the actual new ground — runs the same
shape through `--local`, which has never been tested for this scenario.
`-b` correctness (step 4) has also never been specifically checked for
clang; existing binpkg-identity coverage (`test-scripts/
test-crossdev-binpkg-sandbox.sh`) only exercises `sys-libs/zlib`.

## Target

`riscv64-unknown-linux-gnu`, **GCC** model (the project's standard test
tuple — matches `CROSS_TARGET` in `test-scripts/regression-matrix.sh`). Not
`-L`/LLVM crossdev mode: that mode rejects glibc targets outright, and this
tuple is glibc. So clang is built as an ordinary **target-arch** package
through the GCC cross toolchain (`docs/crossdev.md`'s "two package classes"
— `llvm-core/clang` installs into the sysroot, it is not the host-side
cross-compiler itself), exactly like the 2026-08-04 run.

## Scenario matrix

| # | Scenario | Topology flag | Known state going in |
|---|---|---|---|
| A | `--prefix` overlay | `--prefix DIR` | Proven once, informally, 2026-08-04 (commit `ad59ed0`) |
| B | `--local` standalone prefix | `--local [DIR]` | **Blocked on native bootstrap.** Empty-VDB hard cycle is expected; seed via `package.provided` (plan: [[local-bootstrap-provided]], [`docs/local-bootstrap.md`](../docs/local-bootstrap.md)). Crossdev under `--local` fails the same way until that lands. |

## 0. Sandbox setup (crossdev-stages)

One fresh sandbox per scenario — never reuse or hand-patch an existing one
(hard rule, `crossdev-stages-sandbox` memory: a reused/broken sandbox has
cost a full bad session before; symptoms look like `em` bugs but aren't).

```sh
cd ~/Sources/crossdev-stages
cargo build --release
./target/release/crossdev-stages sandbox setup --arch aarch64 --name em-clang-prefix
./target/release/crossdev-stages sandbox setup --arch aarch64 --name em-clang-local
./target/release/crossdev-stages sandbox prepare --name em-clang-prefix --bare
./target/release/crossdev-stages sandbox prepare --name em-clang-local --bare
```

**Correction, live-verified 2026-08-06 — `sandbox setup` alone is NOT
enough anymore; the `crossdev-stages-sandbox` memory saying the stage3
"already ships a full ... `::gentoo` tree" is stale/wrong for the current
tool.** A fresh `sandbox setup`-only sandbox has an *empty*
`var/db/repos/gentoo` (`em`/real `emerge` both fail immediately: `not a
valid repository`). `sandbox prepare --name NAME --bare` ("configure portage
and sync the tree, but do not install host packages") is what actually
populates it — takes well under a minute, gpg-verifies the snapshot, no host
package installs. Use `--bare`, not a bare `sandbox prepare` (which also
installs the full board/image host-dependency list — still unneeded for
driving our own `em` binary).

**Do not** work around the empty-repo problem by manually
`sudo mount --bind`-ing this workspace's own `portage-repo/gentoo` tree in
(the recipe `test-scripts/README.md` still documents, paired there with
`sudo chroot`) — tried first, and it breaks `sandbox run` outright
(`mount(...) => EINVAL`, `sandbox run` does its own internal mount-namespace
setup that a pre-existing manual bind mount on the sandbox root collides
with). `sandbox prepare --bare` is the correct, current, `sandbox
run`-compatible way to get a real tree; recovered by unmounting the manual
binds, `sudo rm -rf`-ing both sandbox directories, and recreating fresh.

Deploy the release `em` binary into each:

```sh
cd ~/Sources/portage-cli
cargo build --release -p portage-cli
for name in em-clang-prefix em-clang-local; do
  cp target/release/em ~/.cache/crossdev-stages/sandboxes/$name/usr/local/bin/em
done
```

Drive everything from here with
`crossdev-stages sandbox run --name <name> "<cmd>"` — never `sudo chroot`
directly into the sandbox (same hard rule; `sandbox run` needs no manual
`proc`/`dev`/`sys` mount management and doesn't leave orphaned mounts behind
on a killed build).

## How to retrieve logs

Several distinct log sources exist here — don't conflate them, and don't dump
full logs into your own output when a targeted grep will do (per
`docs/testing.md`'s own rule: check the actual `build.log`, don't assume).

- **The `em` invocation's own output** (resolve/plan/merge-summary — not a
  single package's compiler output). Always redirect it to a file and check
  an explicit exit marker, exactly like `test-scripts/regression-matrix.sh`
  does, rather than trusting a truncated terminal scrollback:

  ```sh
  crossdev-stages sandbox run --name em-clang-prefix -- \
    "em --prefix /root/xp --target riscv64-unknown-linux-gnu crossdev --setup --jobs N \
       > /root/crossdev-setup.log 2>&1; echo EXIT=\$? >> /root/crossdev-setup.log"
  crossdev-stages sandbox run --name em-clang-prefix -- \
    "grep -o 'EXIT=[0-9]*' /root/crossdev-setup.log | tail -1"
  ```

- **On a merge failure, `em`'s own summary names the exact `build.log` to
  read** (`merge/mod.rs`: each failed package gets a `log: <path>` line once
  that file exists) — start there instead of guessing a path:

  ```sh
  crossdev-stages sandbox run --name em-clang-prefix -- \
    "grep -A2 'failed to merge' /root/crossdev-setup.log"
  ```

  If a path isn't printed (e.g. the failure was before any build started —
  preflight, resolve, fetch), there is no `build.log` for it; look at the
  invocation log itself instead.

- **Per-package `build.log` location**, if you need to construct the path
  yourself rather than reading it off the failure summary: `<outer
  prefix-or-local-path>/var/tmp/portage/<category>/<pf>/build.log`
  (`default_work_base`) — the **outer** `--prefix`/`--local` path, not the
  `--target` sysroot, even when `--target` is set (build work trees stay
  anchored to the outer EROOT; only the *installed result* lands in the
  sysroot — see `docs/crossdev.md`'s "Worked example" gotcha note).

- **`elog` messages** (the `einfo`/`ewarn`/`eerror`/`eqawarn` summaries a
  phase files, as opposed to raw compiler output) are retrieved with
  `em read`, not by finding the file by hand:

  ```sh
  crossdev-stages sandbox run --name em-clang-prefix -- \
    "em --prefix /root/xp --target riscv64-unknown-linux-gnu read llvm-core/clang"
  crossdev-stages sandbox run --name em-clang-local -- \
    "em --local /root/xl --target riscv64-unknown-linux-gnu read llvm-core/clang"
  ```

  **Pass the same `--prefix`/`--local`/`--target` flags you built with** —
  `em read` resolves its log directory from `BROOT`, which differs by
  scenario (`--prefix`'s BROOT is the host `/`; `--local`'s BROOT is the
  prefix itself; `--target` never moves it). Using the wrong flag
  combination silently looks in the wrong directory and reports "no elog
  messages" instead of erroring — don't mistake that for "nothing was
  logged". `-n0` shows all filed packages instead of the default 10; `-l`
  lists filenames only, without printing message bodies.

- **`crossdev-stages`'s own operational logs** (sandbox setup/prepare
  progress — a different tool, not `em`) live under
  `~/.cache/crossdev-stages/logs/<name>`. Only relevant if `sandbox setup`
  itself misbehaves, not for anything `em`-side.

- **Retrieval mechanics**: always go through `crossdev-stages sandbox run
  --name <name> "<cmd>"` (`cat`/`grep`/`tail`/`em read`), never assume you
  can read the sandbox's rootfs directly from the host — commands run as
  real root inside the sandbox, so files a real build/merge creates are
  root-owned on the host filesystem too. For a large `build.log`, grep for
  `>>> Failed`/`die:`/`error:`/`ERROR:` first and only `tail`/read the
  surrounding context once you know where to look, rather than retrieving
  the whole file.

## Scenario A: `--prefix`

**Step 1 — stand up the topology alone (cheap sanity check):**

```sh
em --prefix /root/xp setup -p
em --prefix /root/xp setup
```

**Step 2 — bootstrap the cross toolchain.** No separate `em --target T
setup` pre-step: `crossdev --setup`'s own `init_target` already bootstraps
the outer root itself. (A prior version of this recipe ran `em --target T
--prefix DIR setup` first — with `--target` set that follows the sysroot
substitution and writes the *sysroot's* make.conf instead of the outer
prefix's, and because config writes are `FillGapsOnly`, `crossdev --setup`'s
own later correct write then gets silently skipped since a file already
exists at that path. Confirmed as a real bug this exact recipe hit
2026-07-17 — don't reintroduce it.)

```sh
em -p --prefix /root/xp --target riscv64-unknown-linux-gnu crossdev --setup
em    --prefix /root/xp --target riscv64-unknown-linux-gnu crossdev --setup --jobs N
```

Verify the cross compiler is real and runs:

```sh
/root/xp/usr/bin/riscv64-unknown-linux-gnu-gcc --version
```

**Step 3 — cross-build clang, with `-b`:**

```sh
em -p --prefix /root/xp --target riscv64-unknown-linux-gnu -b llvm-core/clang --jobs N
em    --prefix /root/xp --target riscv64-unknown-linux-gnu -b llvm-core/clang --jobs N
```

**Step 4 — buildpkg verification:** see the shared checklist below.

## Scenario B: `--local`

**Step 0 — recheck whether this is still blocked before doing anything
else.** A 2026-07-12 run found `em --target riscv64-unknown-linux-gnu
--local crossdev --setup` failing at **preflight**, before merging
anything — flagged then as "real gap, not yet root-caused" and never
followed up. A lot has landed since (`cede_required_use` fix, USE-fold
redesign, the `--target`/outer-root `init_target` fix above). Don't assume
either "still broken" or "silently fixed" — run it first and read the
actual preflight output:

```sh
em -p --local --target riscv64-unknown-linux-gnu crossdev --setup
```

- If this now resolves cleanly: proceed directly with steps 1-3 below,
  no workaround needed — and note this as a real, worth-recording fix
  (cross-reference which recent change plausibly explains it).
- If it still fails at preflight: that failure signature *is* the first
  real finding to record (with the actual `needs:` lines, not a guess),
  before falling back to the workaround in step 1.

**Step 1 — topology setup, with the known-cycle workaround if step 0 shows
it's still needed.** `--local`'s own **native** toolchain bootstrap hits a
genuine 11-node hard cycle (gdbm↔elt-patches↔meson↔gettext↔glibc[cet]↔python
— `install-order-scc-tiebreak-fix`/`regression-matrix-script` memories) and
cannot complete from nothing. If crossdev's toolchain bootstrap under
`--local` turns out to depend on that native base existing first (unclear —
that's exactly what step 0 checks), the documented workaround
(`native-prefix-toolchain-bootstrap-fix` memory) is: build a native
toolchain via `--prefix` into some `DIR` first (borrows the host's tools to
break the cycle), then point `--local` at that same, now-populated `DIR` as
its `EPREFIX` so it has something of its own to build from — rather than
starting `--local` from a genuinely empty prefix.

```sh
# only if step 0 shows crossdev --setup --local is still blocked:
em --prefix /root/xl toolchain --setup --autounmask-write --jobs N
em --local /root/xl setup -p
em --local /root/xl setup
```

**Step 2 — bootstrap the cross toolchain** (same shape as Scenario A step 2,
`--local` in place of `--prefix`):

```sh
em -p --local /root/xl --target riscv64-unknown-linux-gnu crossdev --setup
em    --local /root/xl --target riscv64-unknown-linux-gnu crossdev --setup --jobs N
/root/xl/usr/bin/riscv64-unknown-linux-gnu-gcc --version
```

**Step 3 — cross-build clang, with `-b`:**

```sh
em -p --local /root/xl --target riscv64-unknown-linux-gnu -b llvm-core/clang --jobs N
em    --local /root/xl --target riscv64-unknown-linux-gnu -b llvm-core/clang --jobs N
```

**Step 4 — buildpkg verification:** see the shared checklist below.

## Buildpkg verification checklist (run for both scenarios)

Don't stop at "the merge exited 0" — `-p`'s binary/ebuild tag is a
documented simplification that doesn't reflect the real per-key reuse gate
(`test-binpkg-identity-sandbox.sh`'s own noted gotcha), so check the real
mechanisms directly:

- [ ] A binpkg for `llvm-core/clang` exists in the target sysroot's PKGDIR.
- [ ] `em maint binpkg list` / `em maint binpkg fingerprint` (both scenarios,
      after the clang build) show the recorded **CHOST as the target's**
      (`riscv64-unknown-linux-gnu`), not the build host's — same check
      `test-crossdev-binpkg-sandbox.sh` already does for `sys-libs/zlib`,
      now against clang specifically (bigger/slower, worth confirming the
      identity model holds for a toolchain-heavy package too).
- [ ] `em maint binpkg fingerprint --host` still separately reports the
      real build-host CHOST correctly (not swapped/confused with the
      target's).
- [ ] The installed clang binary is genuinely target-arch:
      `file <sysroot>/usr/bin/clang` (or wherever `llvm-core/clang` lands
      inside `<sysroot>`) reports an ELF for RISC-V, not the build host's
      arch.
- [ ] Real reuse, not just the `-p` tag: re-run with
      `-e -k llvm-core/clang` (`-e`/`--emptytree` forces a fresh plan
      decision, `-k`/`--usepkg` allows binpkg reuse) and confirm via the
      build log / timing that it actually reuses the just-built binpkg
      instead of recompiling clang.
- [ ] A rebuild under a genuinely different `build_env_key` (e.g. a
      different `-march` via `CFLAGS`) does **not** wrongly reuse the first
      binpkg — the same asymmetric-key check `test-binpkg-identity-sandbox.sh`
      does for zlib, worth doing once for clang since it's the first
      cross-toolchain-class package this check has been run against.

## Known prior blockers to watch for (don't re-diagnose from scratch)

- `--local`'s native-toolchain hard-cycle partial failure (see Scenario B
  step 1) — expected, already understood, not a new bug if it recurs in
  that specific shape.
- `cede_required_use`'s installed-cpv early-return gap under `--prefix`
  (fixed 2026-08-01, commit `58d335b` — should **not** recur; if it does,
  that's a real regression).
- The `--target`-with-`--prefix`/`--local` `FillGapsOnly` config-clobber bug
  described in Scenario A step 2 — fixed, watch for it resurfacing if the
  recipe accidentally reintroduces the extra `em --target T setup` pre-step.
- `preflight.rs`'s BDEPEND check not recognizing PATH-found host tools under
  `--local` (2026-07-12 finding, `stages --stage1` context — may or may not
  apply to `crossdev --setup`'s own preflight; step 0 above is what
  actually resolves whether it's relevant here).

## Order of execution

1. Scenario A (`--prefix`) first — re-derives already-proven ground, so a
   regression here is the highest-priority finding (something broke what
   used to work).
2. Scenario B step 0 alone (the preflight recheck) — cheap, and its result
   decides whether the rest of B needs the workaround.
3. Scenario B steps 1-4 — new coverage, slowest (a from-scratch cross
   toolchain bootstrap plus a full clang build, possibly preceded by a
   native `--prefix` toolchain build if the workaround is needed).

## Out of scope for this pass

- Actually executing the produced riscv64 clang binary (needs qemu-user;
  not requested here — "use the new toolchain to build something" is
  satisfied by cross-building clang itself, the goal package).
- Fixing anything found — this is a validation pass. Findings get recorded
  here (following `em-stages-scenario-matrix.md`'s numbered-finding
  convention) and triaged afterward.
