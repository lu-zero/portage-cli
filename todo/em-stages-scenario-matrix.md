# `em stages --stage1` scenario matrix — full re-validation across root modes

STATUS: in progress — executing. Using one dedicated sandbox per scenario
(not shared) since `crossdev-stages sandbox setup` is fast/cheap per-name.

**Update 2026-08-01: the `cede_required_use`/`installed_cpvs.contains(cpv)`
bug documented throughout this file (awk-4 REQUIRED_USE finding, first
raised below and re-confirmed multiple times further down) is FIXED,
commit `58d335b`.** Every "fix direction: drop the `installed_cpvs` early
return" note in this doc is **superseded** — that naive fix was verified
wrong (it reintroduces a real prior bug, commit `b919014`'s systemd-utils/
jinja2 protection). The actual fix adds a second set,
`Adapter::rebuilding_cpvs` (installed cpvs this run rebuilds anyway — a
non-selective explicit root target or a `-N`/`-U` USE-drift rebuild,
computed from the plan's own already-installed-filter logic), and guards
ceding on `installed_cpvs.contains(cpv) && !rebuilding_cpvs.contains(cpv)`.
See [[cede-required-use-repro-not-fixed]] for the full story. Every
reference to "the known `cede_required_use` bug" or "drop the
`installed_cpvs` early return" below is historical diagnosis, not current
status — don't re-open this without checking that memory/commit first.

## Execution log

- **`crossdev-stages sandbox setup --dry-run` is not actually a dry-run**:
  running it against a new `--name` unpacked the real 1.8G stage3 anyway
  (confirmed: `sandbox list` showed it `state=unpacked` immediately after,
  `du -sh` showed 1.8G). Not our code (crossdev-stages is a sibling repo,
  not fixed here per [[dont-commit-to-sibling-repos]]) — just noting so a
  future `--dry-run` isn't trusted at face value for this tool.
- `em-stage1-root` sandbox created (aarch64, 1.8G unpacked). All four
  sandboxes created: `em-stage1-root`, `em-stage1-prefix`, `em-stage1-local`,
  `em-stage1-cross` — each `crossdev-stages sandbox setup --arch aarch64
  --name <name>` (few seconds each). Bind-mounted this workspace's own
  `portage-repo/gentoo` tree (already has a real md5-cache) at
  `<sandbox>/var/db/repos/gentoo`, plus `/proc`, `/dev` (rbind), `/sys`,
  `/etc/resolv.conf` (copied — chroot doesn't get one by default, breaks
  distfile fetch DNS), and the host's `/var/cache/distfiles` bind-mounted at
  the same path (cache hits avoid needing network for most distfiles).
- `em --help` and a `toolchain --setup -p` resolve both work cleanly inside
  the bare chroot (`em-stage1-root`, scenario 1): 53 packages, no
  REQUIRED_USE violation, no autounmask needed.
- **Scenario 1 (native `--root`) — full success.** `em toolchain --setup
  --root /root/stage1-root --jobs 1 --keep-going` completed: "35 package(s)
  merged into /root/stage1-root", ">>> native toolchain ready", 0 real
  failures (grep for `failed to merge`/`die:`/`>>> Failed` — none). Verified
  the built compiler is genuinely self-contained: `gcc --version` must be
  run via a **nested** `chroot` into `/root/stage1-root` itself (`sudo
  chroot "$SB/root/stage1-root" /usr/bin/...-gcc --version`), not by
  executing a path inside it from the outer sandbox chroot — the target's
  gcc-config symlinks are absolute (`/usr/aarch64-.../gcc-bin/15/...`) and
  only resolve correctly once that offset *is* `/`. Running it the naive
  way (single chroot, full inner path) silently resolves to the sandbox's
  own host gcc instead (a testing-methodology trap, not an `em` bug — worth
  remembering for any future verification of a `--root`-built toolchain).
  Nested-chroot check confirms `gcc-15.2.1_p20260214`, matching the merge
  log exactly.
- **Finding (not a bug): `--prefix`/`--local` toolchain --setup is a weak
  test here.** Per `docs/root-model.md`'s scenario table, `--prefix P` keeps
  `base_root = /` (host) as the "already installed" view and only overlays
  the delta into `P` — `planner installed = host ∪ VDB(P)`. Since these
  sandboxes are real stage3s (already have a working baselayout/binutils/
  glibc/gcc at their own `/`), `toolchain --setup --prefix <dir>` correctly
  sees the toolchain as already satisfied by the base and only shows
  `[ebuild R]` (reinstall, since the atoms are the explicit targets) with no
  dependency closure — there's no bootstrap to observe, because there's
  nothing missing for the overlay to fill in. This is the planner working
  exactly as documented, not a gap. **Consequence for this matrix**:
  scenarios 2/3 (`--prefix`/`--local`) are more meaningfully tested via
  `em stages --stage1` directly (the full `packages.build` list, `USE="-*
  build"`, which can differ from what the stage3 already has installed)
  rather than `toolchain --setup` first — skip straight to the stage1 step
  for these two scenarios and treat any `toolchain --setup` output as a
  planner-correctness sanity check, not a real bootstrap exercise.

- **Real finding: `em stages --stage1 --prefix <dir> -p --autosolve-use`
  reports a genuine REQUIRED_USE violation for `app-alternatives/awk-4`**
  (`^^ ( gawk busybox mawk nawk )`, all four showing disabled) that
  `--autosolve-use` should have resolved but didn't. Root-caused, not yet
  fixed (out of scope for this validation pass per its own rules):

  - `app-alternatives.eclass`'s `_app-alternatives_set_globals` sets
    `IUSE="+gawk busybox mawk nawk"` (only the *first* alternative gets a
    `+` default — "yep, that's a cheap hack", the eclass's own comment) and
    `REQUIRED_USE="^^ ( gawk busybox mawk nawk )"`. Under normal USE, gawk's
    `+` default alone satisfies `^^`. Under `stages --stage1`'s `USE="-*
    build"`, this week's wildcard-reset fix *correctly* suppresses that `+`
    default too (matching real portage — the whole point of that fix), so
    nothing satisfies the constraint anymore. This part is working as
    intended.
  - The gap: `Adapter::cede_required_use` (`repo.rs:600`) has
    `if self.installed_cpvs.contains(cpv) { return; }` as its *first* check
    — before even looking at whether the current desired USE actually
    satisfies REQUIRED_USE. Under `--prefix`, `installed_cpvs` is
    `target_installed_cpvs` seeded from the **host** VDB (`mod.rs:302-324`,
    matching `docs/root-model.md`'s "planner installed = host ∪ VDB(P)" —
    correct for that purpose). Since the sandbox's stage3 base already has
    `app-alternatives/awk-4` installed, this early return fires and Level-C
    never gets a chance to cede/re-decide its flags for the *new* build
    under `-* build`, even though the constraint demonstrably doesn't hold
    for the desired USE this build is about to use.
  - The function's own doc comment says it "skips ceding entirely when the
    constraint already holds" — that's exactly what the later
    `unsatisfied.is_empty()` check (already present, a few lines down)
    covers correctly. The `installed_cpvs.contains(cpv)` early return looks
    like a separate, overly-broad guard (probably intended to avoid
    re-deciding USE for settled/unrelated already-installed packages) that
    happens to also suppress the one case where re-deciding is exactly what
    stage1's use-override rebuild needs. Suspect fix direction: drop the
    `installed_cpvs` early return and rely on the `unsatisfied.is_empty()`
    check alone (which already re-derives from the *current* `cfg`, so a
    genuinely-settled package still short-circuits correctly) — needs its
    own careful pass (why was the separate check added at all? check
    blame/history) before touching it, not a same-session fix.
  - Scope check still needed: does this affect scenario 1 (`--root`, empty
    target, nothing "already installed")? Should not — `installed_cpvs`
    there is empty for a fresh bootstrap. Likely `--prefix`/`--local`-
    specific (and possibly a real, un-tested `--root --update` /
    `--newuse` reinstall-over-existing-VDB case too — worth a follow-up
    check once this matrix is done).

- **Second real finding, scenario 3 (`--local`): `stages --stage1` exits 1
  on a genuine preflight BDEPEND failure**, distinct from the `--prefix`
  finding above (and confirms it by contrast — see below). Log:
  `error: pre-flight dependency check failed`, listing `app-portage/elt-patches`
  needing `app-arch/xz-utils`, `app-arch/zstd` needing `>=dev-build/meson-1.2.3`,
  `sys-libs/glibc` needing `>=sys-devel/gcc-6.2` — all **genuinely present**
  in the sandbox (`which gcc meson xz` and `qlist -Iv` inside the chroot
  confirm `sys-devel/gcc-15.3.0`, `dev-build/meson-1.11.1`,
  `app-arch/xz-utils-5.8.3` are installed and on `PATH`).

  Root cause: `Cli::base_roots()`'s `--local` branch (`cli.rs:567-581`) sets
  `broot: Some(prefix.clone())` — **`--local`'s BROOT is the prefix itself,
  not the host**, by design (the branch's own comment: "standalone
  Gentoo-Prefix, own BROOT... during bootstrap the host compiler is reached
  via PATH, never via a symlink masquerading as a prefix-owned file"). That
  comment describes the *build-execution*-time behavior (a spawned
  build correctly finds `gcc`/`meson`/`xz` via the chroot's inherited
  `PATH` env, no VDB entry needed) — but `preflight.rs`'s BDEPEND check
  (`portage-cli/src/preflight.rs`, `roots.satisfaction_root(DepClass::Bdepend)`)
  has no equivalent "found via PATH, no VDB tracking needed" concept: it
  only ever checks VDB entries at the BROOT it's given, which for `--local`
  is the prefix's own (still-empty, this being a from-scratch bootstrap)
  VDB. So preflight fails a check that the actual build wouldn't have — a
  real gap between what preflight validates and what `--local`'s own design
  comment says should work.

  **Confirms the `--prefix` finding by contrast**: `--prefix`'s BROOT is
  `/` (`cli.rs:592`, the host) — same chroot, same real gcc/meson/xz — and
  its resolve got past preflight to the actual REQUIRED_USE stage, meaning
  preflight's BDEPEND check *does* work correctly when BROOT is a real VDB
  with these tools registered. The `--local` case is the one that needs
  either (a) a documented, deliberate exception in `preflight.rs` for
  `--local`'s PATH-based BDEPEND model, or (b) BROOT weaving in the host's
  VDB for `--local` too (contradicting its own "own BROOT" design intent —
  would need to reconcile with why `--local` was built to *not* do that
  overlay in the first place, likely relocatability: a `--local` prefix is
  meant to be usable standalone/moved, so depending on host VDB state would
  be a regression). Needs its own design pass, not a quick fix here.

  **Positive confirmation of the `--prefix` diagnosis**: this run's own
  `--autosolve-use` output shows it *correctly* ceding all seven
  `app-alternatives/*` `^^` constraints (awk, bzip2, gzip, lex, ninja, tar,
  yacc — all reported as "configured off" → autosolved to the first listed
  alternative) — because under `--local` nothing is "already installed"
  (fresh prefix), so `cede_required_use`'s `installed_cpvs.contains(cpv)`
  early return never fires here. This is exactly the contrast predicted by
  the `--prefix` root-cause analysis above, now empirically confirmed:
  same wildcard-reset-suppressed defaults, same `^^` constraints, but
  autosolve-use works here and doesn't there, and the only difference is
  whether the cpv is already in `installed_cpvs`.

- **CRITICAL BUG FOUND AND FIXED: `--autosolve-use` stack overflow on a
  populated root.** `em stages --stage1 --root /root/stage1-root -p
  --autosolve-use` (scenario 1, native `--root`, real installed toolchain)
  crashed with `fatal runtime error: stack overflow, aborting`. Without
  `--autosolve-use` the same command resolved cleanly (exit 0, reporting the
  7 expected `^^` REQUIRED_USE violations) — isolating the bug precisely to
  Level-C autosolve combined with a real, non-empty installed base.

  Minimal repro found by bisecting the atom list down from the full
  packages.build set: `USE="-* build" em --root <dir> -p --autosolve-use
  app-alternatives/tar` alone crashes when `app-arch/libarchive` (one of
  tar's `^^ ( gnu libarchive )` alternatives) is already installed in
  `<dir>`; the identical command against a fresh/empty root does not crash.
  Reproduced in-process via a new unit test
  (`required_use_exactly_one_with_installed_alternative_does_not_overflow`,
  `portage-atom-pubgrub/src/provider/tests.rs`) — a minimal `^^ ( w x )`
  shape with `w`'s own dependency pre-installed, no chroot/real-repo needed.

  Root cause (found via `perf record --call-graph dwarf` on a `profiling`-
  profile build, then confirmed instantly with `gdb bt` against the debug
  unit test once minimized): `PortageDependencyProvider::branch_installed_ver`/
  `branch_best_installed` (`provider/mod.rs`) — a `choose_version`
  tie-break heuristic ("prefer the alternative reaching a newer installed
  version") — recursed into **any** virtual package reachable from a
  version's merged deps, with **no cycle guard**. Safe for its intended
  target (`Choice`/`SlotChoice` `||`/`:*` provider trees, which are acyclic
  DAGs down to real packages), but `UseDecision` nodes (Level-C `REQUIRED_USE`
  encoding) reference each other **symmetrically** for a `^^` group's
  mutual-exclusion pairs — an inherent 2-cycle this function was never
  designed to enter. It's gated behind `newest_installed_choice_branch`'s
  "at least one candidate branch reaches an installed package" check, which
  is exactly why it only fired once `libarchive` was genuinely installed.

  Fixed (`portage-atom-pubgrub/src/provider/mod.rs`): restricted the
  recursion to `Choice`/`SlotChoice` only (matching the same distinction
  `host_satisfied_on_broot_inner` already makes for a different function),
  plus a `BRANCH_DEPTH_LIMIT = 16` recursion budget threaded through both
  functions as defense-in-depth, since the heuristic's own docs say "one
  level" but the implementation was actually unbounded. Verified: the unit
  test passes, the full workspace suite passes (1210 tests), clippy/fmt
  clean, and the original real-world repro (`em stages --stage1 --root
  /root/stage1-root -p --autosolve-use`) now resolves cleanly — 78
  packages, exit 0, `tar` correctly autosolved to `libarchive`, no crash.

- **Scenario 1 (native `--root`) — real `stages --stage1` build, post-fix.**
  71 of 77 packages merged; 6 failed. Checked each build.log directly
  (not assumed) before categorizing, per the correction on this session's
  earlier premature cross-build triage:
  - `sys-devel/m4-1.4.20`, `sys-apps/coreutils-9.9-r1`, `app-editors/nano-8.7`:
    all three fail with the identical gnulib error (`./stdlib.h:807:20:
    error: expected identifier or '(' before '_Generic'` in `bsearch`'s
    declaration) — a real gnulib/glibc C23 qualifier-generic-function
    incompatibility (upstream fix: Gentoo bug 969219, "Port to C23
    qualifier-generic fns like strchr"). **Verified, not assumed**: found
    the actual fix already sitting in this repo tree at
    `sys-apps/coreutils/files/coreutils-9.9-glibc-2.43-c23.patch`, but it is
    **not wired into the ebuild's `PATCHES` array** (`coreutils-9.9-r1.ebuild`
    `src_prepare`, only lists two unrelated patches plus a separately-fetched
    `MY_PATCH` bundle that predates this Nov-2025 cherry-pick). `m4` has no
    such patch file in this tree at all. This is a real gap in the checked-out
    ebuild tree snapshot's patch set (would hit real `emerge` identically,
    since it's a source-level compile error against this glibc version, not
    anything `em`-specific) — not an `em` bug.
  - `sys-apps/portage-3.0.77-r3`: meson configure fails with `<PythonExternalProgram
    'python3'> is not a valid python or it is missing distutils` — the
    well-known Python 3.12+ removal of the stdlib `distutils` module; this
    ebuild's meson setup isn't distutils-free yet (or needs a shim). Also a
    real, independently-known compatibility gap, not `em`-specific.
  - `dev-lang/python-3.13.11` (×2, once per consumer needing it): "one or
    more distfiles could not be fetched" — not investigated further (likely
    a genuine cache-miss + this specific patch-level release not mirrored,
    same class as the distfile-reliability gaps already tracked in
    `todo/distfile-fetch-reliability.md`), but flagged here rather than
    assumed, since this session's own earlier mistake was asserting
    "legitimate" without checking.
  - **None of these are new findings requiring an `em` fix.** The stack
    overflow above is the real, `em`-specific bug from this run.

- **New real bug found via direct real-`emerge` comparison, and fixed:
  `package.use` survived `USE=-*` wildcard-reset in `em`, but real portage
  makes it entirely inert.** Prompted directly by "did you compare with the
  plan emerge produced?" — ran real `emerge -pv` against the same fresh
  `/root/stage1-testing` root under `ACCEPT_KEYWORDS="~arm64"` and
  `USE="-* build"` with a `package.use` entry `sys-devel/m4 nls`: real
  emerge showed `-nls` (the override had **zero effect**); without the `-*
  build` override, the same entry correctly forced `nls` on in both real
  emerge and `em`. Cross-checked the other direction too:
  `sys-apps/baselayout -build` in `package.use` also had zero effect under
  `-* build` (real emerge still showed `build` enabled) — confirming
  `package.use` is entirely bypassed once a `-*` wildcard reset is in
  effect, not merely unable to *revive* something, in either direction.

  `em`'s `apply_package_use` (`portage-solver/src/use_config.rs`) applied
  `package.use` overrides unconditionally, with no awareness of
  `wildcard_reset` — a gap in this week's earlier `USE=-*` fix, since
  `package.use` is a separate code path from the IUSE-default fallback
  that fix already covered. Fixed: `apply_package_use` now returns
  `Cow::Borrowed` (package.use entirely skipped) whenever `base.wildcard_reset()`
  is set, matching real portage's "package.use is just another layer the
  `-*` wildcard wipes" semantics. Added
  `apply_package_use_inert_under_wildcard_reset` (`portage-solver/src/use_config.rs`).
  Verified: full workspace suite passes (1211 tests), clippy/fmt clean, and
  both the `m4 nls` and `baselayout -build` live cases now match real
  emerge exactly after rebuilding.

- **New discrepancy found during regression spot-check, not yet root-caused
  (separate from the package.use fix above — confirmed unrelated).** After
  the package.use fix, re-ran `em --root /root/stage1-testing -vp
  sys-devel/gcc` (plain, no `USE=-*`, no `~arm64`-specific config quirk
  beyond the profile already in use) against real `emerge -vp` on the same
  root: real emerge shows only `[R] sys-devel/gcc` (already installed,
  matching version, nothing else needed); `em` additionally pulls in
  `sys-libs/libxcrypt-4.4.38-r1` and `virtual/libcrypt-2-r1` as new
  packages. Both tools show `gcc` with `sanitize` enabled (which is what
  conditionally pulls `virtual/libcrypt` into `DEPEND` per
  `toolchain.eclass:419`, `DEPEND+=" sanitize? ( virtual/libcrypt )"`), so
  the flag isn't the differentiator.
  - **Confirmed not caused by today's package.use fix**: this test sets no
    `USE=-*`, so `apply_package_use`'s new `base.wildcard_reset()` check
    evaluates to `false` and short-circuits to the exact pre-fix code path
    — the fix is a structural no-op here.
  - **Confirmed real emerge doesn't need it even when forced to
    re-evaluate**: `emerge -pv --newuse sys-devel/gcc` (forces full USE/dep
    re-check) still shows nothing for gcc/libxcrypt/libcrypt.
  - Not yet root-caused. Hypothesis to check next: `compute_dependencies`'s
    "already installed at matching version → skip DEPEND" branch
    (`portage-atom-pubgrub/src/provider/solve.rs` ~line 305) may not be
    firing for this gcc for some reason specific to this root (possibly
    slot-related, since gcc is `:15` slotted) — worth checking whether
    `self.installed.get(package)` actually finds an entry for this exact
    `(cpn, slot)` pairing here, or whether real emerge's own "not
    re-evaluating DEPEND for an installed, unchanged package" logic is
    doing something em's port of the same rule doesn't quite replicate for
    slotted DEPEND with a conditional (`sanitize?`) virtual.
  - **Flagging, not fixing now** — this session has already covered a lot
    of ground; treat as the next thing to pick up.

- **`cede_required_use` bug scope broadened: also hits `--root` in an
  upgrade/resume scenario, not just `--prefix`.** Testing `ACCEPT_KEYWORDS="~arm64"`
  against the already-populated `/root/stage1-root` (134 packages installed
  from the earlier successful real build, including `sys-apps/gawk-5.3.2`
  and `app-alternatives/awk-4`) re-triggered the exact same
  `installed_cpvs.contains(cpv)` early-return bug: `gawk` needed upgrading
  to `5.4.0-r2` under the newly-accepted testing keywords, but
  `app-alternatives/awk-4` being already-installed skipped Level-C
  reconsideration, re-exposing its `^^ ( gawk busybox mawk nawk )` violation
  with `--autosolve-use` unable to fix it (same shape as the `--prefix`
  finding, just reached via "re-run stage1 against a partially-built root"
  instead of "base = host"). **Not a `--prefix`-specific bug** — any
  scenario where the target already has the package installed can hit it.
  Redone on a fresh `/root/stage1-testing` root instead to get an
  uncontaminated read on `~arm64`.

- **Re-verified after the fix**: redeployed the fixed binary to all four
  sandboxes. Scenario 2 (`--prefix`) and scenario 3 (`--local`) still show
  exactly their previously-diagnosed, separate bugs (the `cede_required_use`
  early-return gap; the preflight-vs-PATH-tools gap) unchanged — confirms
  all three findings this session are independent, and the stack-overflow
  fix introduced no regression in either.

- **Scenario 4 (cross riscv64) — full success.** `em --target
  riscv64-unknown-linux-gnu crossdev --setup --jobs 1 --keep-going`
  completed cleanly end-to-end (binutils → os-headers → gcc-stage1 → libc →
  gcc-stage2, 0 failures): `>>> cross toolchain riscv64-unknown-linux-gnu
  ready in //usr/riscv64-unknown-linux-gnu`. Verified the compiler actually
  works (`riscv64-unknown-linux-gnu-gcc --version` runs). Followed by
  `em --target riscv64-unknown-linux-gnu stages --stage1 -p --autosolve-use`:
  103 packages, exit 0, all seven `app-alternatives/*` `^^` constraints
  correctly autosolved (no unsatisfied-REQUIRED_USE report) — **further
  confirms the `--prefix` root-cause diagnosis above**: cross's target
  sysroot VDB is fresh/empty (nothing "already installed" there, same
  reasoning as `--local`), so `cede_required_use`'s early return doesn't
  fire and Level-C works as designed. Real `stages --stage1` build kicked
  off.

- **`docs/root-model.md` is stale w.r.t. `--local`**: it documents `--root`/
  `--prefix` in detail (the scenario table, BDEPEND/RDEPEND satisfaction
  roots) but has no section for `--local` at all — its BROOT model
  (`broot = prefix`, distinct from both `--root` and `--prefix`) isn't
  captured anywhere in that doc, which is why the preflight gap above took
  direct code reading to find rather than being a documented, deliberate
  divergence. **Follow-up**: add a `--local` row/section to
  `docs/root-model.md` once the preflight-vs-PATH reconciliation above is
  actually decided (documenting the gap before deciding its resolution
  would just need re-editing).

- **The `apply_package_use_inert_under_wildcard_reset` fix above (the
  `wildcard_reset`-under-Cow::Borrowed one) was itself falsified, then
  replaced structurally, not patched again.** Live-testing with
  `ACCEPT_KEYWORDS="~arm64"` found real emerge lets `package.use` survive a
  `-*` set in `make.conf`, but **not** one set via the process environment
  — a distinction a single `wildcard_reset: bool` cannot express (it was
  already inert either way once `wildcard_reset` was true). Reading
  portage's own `config.py`/`_config/UseManager.py`/`make.globals`
  confirmed the real model: USE resolves via one ordered fold over 8 fixed
  layers (`env.d < repo < features < pkginternal < defaults < conf < pkg <
  env`), and `-*` just clears whatever the fold accumulated from lower
  layers so far — `package.use` (`pkg`) sits between `conf` and `env`, so a
  `conf`-level `-*` clears everything below `pkg` but `pkg` itself still
  applies, while an `env`-level `-*` clears `pkg` too.

  Fixed structurally instead of adding a second, narrower flag:
  `portage_solver::resolve_effective_use(iuse_defaults, pre_env, cpv, slot,
  package_use, env_use)` is now the single canonical per-package USE fold,
  replacing `apply_package_use`/`fold_iuse_defaults`/`wildcard_reset`
  everywhere in the workspace (`portage-repo`'s `ResolvedUse` now exposes
  `pre_env`/`env_use` instead of a collapsed enabled/disabled set). Also
  found and fixed during the migration: `force_mask.rs`'s `effective()` had
  excluded global `use.force` on a since-invalidated assumption that it was
  already baked into the base config; it now applies unconditionally like
  `use.mask`, matching real portage's `use.force`/`use.mask` being
  unconditional post-fold filters. Full detail + design rationale:
  `use-config-duplicate-fallback-logic` memory. Verified: full workspace
  suite (1212 tests), clippy, fmt all clean.

  **Live re-verification, done on a freshly-`sandbox setup` sandbox
  (`em-test-1`) — no manual mounts, no `sudo`, just `sandbox run`:**

  ```sh
  cd ../crossdev-stages
  cargo run -- sandbox setup --arch aarch64 --name em-test-1   # if not already unpacked
  cargo run -- sandbox prepare --name em-test-1
  cp ../portage-cli/target/release/em ~/.cache/crossdev-stages/sandboxes/em-test-1/usr/local/bin/em

  # set up the package.use test case
  cargo run -- sandbox run --name em-test-1 -- \
    "mkdir -p /etc/portage/package.use && printf 'sys-devel/m4 nls\n' > /etc/portage/package.use/zz-test"

  # (a) no override
  cargo run -- sandbox run --name em-test-1 -- "emerge -pv sys-devel/m4 2>&1 | grep m4-"
  cargo run -- sandbox run --name em-test-1 -- "em -pv sys-devel/m4 2>&1 | grep m4-"

  # (b) USE="-* build" in make.conf
  cargo run -- sandbox run --name em-test-1 -- \
    "sed -i '/^USE=/d' /etc/portage/make.conf; echo 'USE=\"-* build\"' >> /etc/portage/make.conf; emerge -pv sys-devel/m4 2>&1 | grep m4-"
  cargo run -- sandbox run --name em-test-1 -- "em -pv sys-devel/m4 2>&1 | grep m4-"

  # (c) USE="-* build" via env
  cargo run -- sandbox run --name em-test-1 -- "USE='-* build' emerge -pv sys-devel/m4 2>&1 | grep m4-"
  cargo run -- sandbox run --name em-test-1 -- "USE='-* build' em -pv sys-devel/m4 2>&1 | grep m4-"

  # (d) USE="build" via env (no -*)
  cargo run -- sandbox run --name em-test-1 -- "USE='build' emerge -pv sys-devel/m4 2>&1 | grep m4-"
  cargo run -- sandbox run --name em-test-1 -- "USE='build' em -pv sys-devel/m4 2>&1 | grep m4-"

  # cleanup
  cargo run -- sandbox run --name em-test-1 -- \
    "rm -f /etc/portage/package.use/zz-test && sed -i '/^USE=/d' /etc/portage/make.conf"
  ```

  Results: all four conditions match real emerge exactly —
  (a) `nls` on, (b) `nls` on (survives make.conf `-*`), (c) `-nls`
  (env `-*` wipes it), (d) `nls` on (plain env addition, no `-*`, doesn't
  suppress package.use).

  Also re-confirmed the `--autosolve-use` stack-overflow fix (the finding
  above this one) live, on the same sandbox, against its pre-existing
  installed `app-alternatives/tar` + `app-arch/libarchive`:

  ```sh
  cargo run -- sandbox run --name em-test-1 -- "em -p --autosolve-use app-alternatives/tar 2>&1"
  ```

  Resolves cleanly (reports the unsatisfied `^^ ( gnu libarchive )`
  advisory since neither is enabled by default; no crash) — this is the
  exact installed-alternative shape that used to stack-overflow.

  **Note on methodology**: don't hand-patch an existing sandbox (manual
  `sudo mount --bind`, `chown`, etc.) when it misbehaves — destroy and
  `sandbox setup` a fresh one instead. See `crossdev-stages-sandbox`
  memory for the incident that prompted this rule.

- **Re-ran `em stages --stage1 -p --autosolve-use` on all three root-mode
  scenarios (`em-stage1-matrix`, a fresh `sandbox setup`-only sandbox — no
  `sandbox prepare`, see the corrected "Substrate" section below) to
  confirm today's fix introduced no regressions.** `sudo chroot` was
  briefly attempted here too (per the now-corrected "Substrate"/
  "Per-scenario commands" sections below) and caught mid-attempt before
  real work happened — see `crossdev-stages-sandbox` memory. All three
  driven via `sandbox run` only, no manual mounts/chroot:

  ```sh
  cargo run -- sandbox run --name em-stage1-matrix -- \
    "em stages --stage1 --root /root/stage1-root -p --autosolve-use"
  cargo run -- sandbox run --name em-stage1-matrix -- \
    "em stages --stage1 --prefix /root/stage1-prefix -p --autosolve-use"
  cargo run -- sandbox run --name em-stage1-matrix -- \
    "em stages --stage1 --local -p --autosolve-use"
  ```

  Results — no crashes, and both non-clean outcomes match this file's own
  prior, already-documented findings exactly (same packages, same gaps):
  - Scenario 1 (`--root`): **exit 0**, clean — every `^^`/`||` REQUIRED_USE
    constraint correctly autosolved (tar, lex, yacc, gzip, bzip2, awk,
    `sys-apps/portage`'s python target).
  - Scenario 2 (`--prefix`): **exit 0** — one un-ceded `app-alternatives/awk`
    `^^` constraint, which is the already-documented `cede_required_use`
    early-return gap (finding above, "installed_cpvs.contains(cpv)"), not new.
  - Scenario 3 (`--local`): **exit 1** — the already-documented
    preflight-vs-PATH-tools gap (`app-portage/elt-patches needs
    app-arch/xz-utils`, finding above), not new.

  Conclusion: the `resolve_effective_use` USE-fold redesign and the
  `--autosolve-use` stack-overflow fix are both clean across all three
  root-mode scenarios, with zero new regressions. The real (non-`-p`)
  `stages --stage1` build for scenario 1 hit an unrelated, separate
  failure in `em toolchain --setup`'s bootstrap (glibc configure failing
  for missing kernel headers despite a `virtual/os-headers` step in the
  plan) — flagged but explicitly **not** investigated further this pass
  per direction ("test `stages --stage1`, not the toolchain applet");
  worth its own look at `sys-kernel/linux-headers` actually landing in
  the ROOT via `virtual/os-headers`'s RDEPEND, next time toolchain
  bootstrap itself is in scope.

## Why now

This week landed a chain of fixes that all bear directly on whether
`em stages --stage1` actually works end-to-end: the `--root` config-parity
fix, the `is_cross_arch` false-positive fix, the Choice/SlotChoice
host-satisfaction gap, the `initial_depend` target-VDB-weave regression, the
`USE=-*` wildcard-reset correctness fix (which should remove the
`curl[quic]`→`ngtcp2[gnutls]` autounmask blocker that previously stalled
`stages --stage1` — see `todo/useconfig-clone-elimination.md`'s "Original
proposal" section and this week's commits `b9d4fbb`/`44f4195`/`03fd4ec`), and
the `ForceMask`/`UseConfig` perf cleanup (`d4091a3`). None of these were
re-verified against a full `stages --stage1` run after landing — only
against `-p`/`-vp` resolves and `em toolchain --setup` in isolation. This is
the "actually try it end-to-end" pass.

Native `--root` stage1 was last confirmed clean on 2026-06-26/2026-07-03
(`todo/stage-build-shakeout.md` findings #20/#37), but on stale CLI syntax
(`--cross` flag, since replaced by `--target`) and before this week's fixes.
`--prefix`/`--local` stage1 has **never been end-to-end tested** — only
individual package builds under those modes.

## Reconciling the two scenario framings

Two ways of slicing "what to test" were raised; they overlap rather than
multiply:

- **By root mode**: `--root`, `--prefix`, `--local` (`docs/root-model.md`).
- **By bootstrap topology**: native, cross, self-contained.

"Self-contained" isn't a fourth thing to test separately — it's simply how
`em toolchain --setup` → `em stages --stage1` already works under `--root`:
BROOT is always the host's (or, here, the sandbox chroot's) own
already-working toolchain, and the target starts genuinely empty. There is
no "seeded from an existing toolchain" variant in the current design to
contrast it with. So the real, distinct, CLI-supported matrix is **four
scenarios**, covering both framings without redundant combinations (cross
is inherently a `--root`/`--target` sysroot in this project's model — it
was never meant to compose with `--prefix`/`--local`):

| # | Scenario | Flags | Toolchain step | Stage1 step |
|---|---|---|---|---|
| 1 | Native, `--root` | `--root <dir>` | `em toolchain --setup --root <dir>` | `em stages --stage1 --root <dir>` |
| 2 | `--prefix` overlay | `--prefix <dir>` | `em toolchain --setup --prefix <dir>` | `em stages --stage1 --prefix <dir>` |
| 3 | `--local` Gentoo Prefix | `--local <dir>` | `em toolchain --setup --local <dir>` | `em stages --stage1 --local <dir>` |
| 4 | Cross (crossdev), riscv64 | `--target riscv64-unknown-linux-gnu` | `em --target <tuple> crossdev --setup` | `em --target <tuple> stages --stage1` |

Scenario 4 reuses the riscv64 tuple from prior sessions
(`todo/stage-build-shakeout.md`) for direct comparison against the
2026-07-05 baseline, rather than introducing a new target variable.

## Substrate: crossdev-stages sandbox, not a `/tmp` scratch dir

**Correction (2026-07-12): this section originally recommended `sudo
chroot`-ing directly into the sandbox rootfs. Don't do that — use
`crossdev-stages sandbox run --name NAME "<cmd>"` for every command below
instead, including the slow real builds. It works fine (confirmed
repeatedly), needs no manual `proc`/`dev`/`sys` mount management, and
running commands as real root on the host via raw `sudo chroot` is exactly
the pattern that caused a bad session (see [[crossdev-stages-sandbox]]'s
hard rule). The commands further down are left in their original
`sudo chroot` form for the historical record of what was actually run
before this correction — translate each to `sandbox run --name <name> --
"<same command, unprefixed>"` before executing.**

Per [[crossdev-stages-sandbox]] / [[chroot-test-em-method]]: drive `em`
**inside** a clean stage3 sandbox as real root (via `sandbox run`, not a
manual `sudo chroot`), rather than pointing a host-run `em` at `--root
</tmp/...>` — the latter has repeatedly hit relative-symlink and permission
issues unrelated to the thing under test, and this session's own
`/tmp/stage1-foo` has accumulated cross-session state that makes it
unsuitable for a from-scratch re-validation anyway.

**Note**: `crossdev-stages` itself drives *real* `emerge`/`{tuple}-emerge`
(confirmed by reading `crossdev-stages/src/portage.rs` — `cross_emerge`
shells out to the tuple's real crossdev-installed emerge wrapper). Its own
`target setup`/`target stage1`/`sandbox crossdev` commands are **not** used
here — only `sandbox setup` to get a clean rootfs, then our own `em` binary
is copied in and driven directly.

**Correction (2026-07-12): don't run `sandbox prepare`.** It installs a big
host-dependency list meant for the full board/image pipeline (`sys-devel/crossdev`,
`dev-lang/rust` + a `cargo install`, `sys-kernel/dracut`, `sys-fs/genimage`,
u-boot tools, etc.) via `emerge-webrsync` + `emerge`, none of which `em
toolchain --setup`/`em stages --stage1` need — that's testing our own
pre-built `em` binary against the repo tree, not building a cross toolchain
or a board image. It's also slow (full tree sync + a real Rust build).
The stage3 tarball `sandbox setup` unpacks already ships a full, current,
md5-cache-populated `::gentoo` tree (confirmed: `/var/db/repos/gentoo` has
~180 categories and a real `metadata/md5-cache`, dated to the stage3's own
build snapshot) — `emerge --version`/`em --version` both work immediately
after `sandbox setup`, no sync needed. `ACCEPT_KEYWORDS="~arm64"` is also
already set by `sandbox setup`'s own `make.conf` writer.

```sh
cd ~/Sources/crossdev-stages
cargo run -- sandbox setup --arch aarch64 --name em-stage1-matrix
```

Sandbox rootfs: `~/.cache/crossdev-stages/sandboxes/em-stage1-matrix/`.
Two existing sandboxes (`aarch64-20260618T101350Z`, `em-item6-9-v2`) predate
this week's fixes and prior test runs may have left state in them — start
fresh rather than reusing either, so a clean run isn't second-guessed by
leftover VDB/config state.

```sh
cargo build --release -p portage-cli   # from portage-cli/
cp target/release/em ~/.cache/crossdev-stages/sandboxes/em-stage1-matrix/usr/local/bin/em
```

Scenarios 1–3 can share **one** sandbox (each uses its own target
subdirectory: e.g. `/root/stage1-root`, `/root/stage1-prefix`, `~/.gentoo`
for `--local`'s default) since they don't interact — `sandbox run` executes
as real root inside the sandbox already, which covers `--root` directly and
is also sufficient privilege for `--prefix`/`--local` (unprivileged modes
still work fine as root; `--privilege` auto-detects).
Scenario 4 (cross) needs its own sandbox or at least its own subtree since
it also writes a crossdev overlay + sysroot config into the chroot's `/`.

## Per-scenario commands

Run each with `--autosolve-use` (Level C REQUIRED_USE auto-satisfaction —
needed for the known util-linux `su?(pam)` case from prior sessions) and
without `--autounmask-write` initially, so a still-needed unmask/USE-write
shows up as a reportable gap rather than silently mutating the sandbox's
config — the `USE=-*` wildcard-reset fix this week should mean the
previously-blocking `curl[quic]`→`ngtcp2[gnutls]` case no longer appears at
all; if it still does, that's the first thing to re-diagnose.

```sh
cd ~/Sources/crossdev-stages

# Scenario 1 — native --root
cargo run -- sandbox run --name em-stage1-matrix -- \
  "em toolchain --setup --root /root/stage1-root"
cargo run -- sandbox run --name em-stage1-matrix -- \
  "em stages --stage1 --root /root/stage1-root --autosolve-use --buildpkg"

# Scenario 2 — --prefix overlay
cargo run -- sandbox run --name em-stage1-matrix -- \
  "em toolchain --setup --prefix /root/stage1-prefix"
cargo run -- sandbox run --name em-stage1-matrix -- \
  "em stages --stage1 --prefix /root/stage1-prefix --autosolve-use --buildpkg"

# Scenario 3 — --local (defaults to ~/.gentoo inside the sandbox)
cargo run -- sandbox run --name em-stage1-matrix -- \
  "em toolchain --setup --local"
cargo run -- sandbox run --name em-stage1-matrix -- \
  "em stages --stage1 --local --autosolve-use --buildpkg"

# Scenario 4 — cross, riscv64 (separate sandbox/subtree)
cargo run -- sandbox run --name em-stage1-cross -- \
  "em --target riscv64-unknown-linux-gnu crossdev --setup"
cargo run -- sandbox run --name em-stage1-cross -- \
  "em --target riscv64-unknown-linux-gnu stages --stage1 --autosolve-use --buildpkg"
```

Start with a **`-p`/pretend pass of `stages --stage1`** for every scenario
before the real build (fast, catches resolution regressions — REQUIRED_USE
violations, unexpected package counts, phantom packages like the
`virtual/libintl`/`virtual/libiconv` case this week — without burning
build time). Only proceed to the real (non-`-p`) run once the pretend
output looks sane.

## Success criteria

- `-p` pass: resolves without a REQUIRED_USE violation report (or, if one
  appears, it's a *known* one — util-linux's `su?(pam)`, handled by
  `--autosolve-use`) and without needing `--autounmask-write`.
- Real run: `em toolchain --setup` completes with a working compiler
  (`<root>/usr/bin/<chost>-gcc --version` runs); `em stages --stage1`
  reports 0 failures, or failures that are already-known/understood
  (cross-reference against `todo/stage-build-shakeout.md`'s findings before
  treating anything as new).
- Package count in the ballpark of prior runs for the same scenario where
  one exists (native --root: ~53 stage1 packages per `packages.build`,
  historically; cross riscv64: matches the 2026-07-05 baseline) — a wildly
  different count (much larger *or* smaller) is itself a signal, not just
  failures.
- No scenario should need a manual VDB patch to complete (the
  `app-alternatives/gpg` IUSE-for-VDB bug, finding #36, is still an open,
  deferred bug — if it recurs, that's expected and already tracked, not new).

## Known prior blockers to watch for (don't re-diagnose from scratch)

From `todo/stage-build-shakeout.md`:
- **`--jobs N` (N>1) races on a shared workdir** (finding #35): a chown/build
  race that can produce spurious "failed to merge" reports for packages that
  actually installed fine (verify via the VDB directly before treating a
  reported failure as real). Prefer `--jobs 1` for the first clean-baseline
  pass per scenario; only reintroduce parallelism once each scenario has one
  known-good serial run to compare against.
- **`fowners` chown-to-foreign-user under non-root** (open, facet 1) —
  shouldn't apply here since these runs are as real root inside the chroot,
  but worth confirming it doesn't resurface for `--prefix`/`--local` (which
  are normally unprivileged; running them as root inside the chroot changes
  this compared to their normal real-world use — note in results whether
  this masks anything that would matter for a genuinely unprivileged run).
- **`app-alternatives/gpg` VDB IUSE gap** (finding #36) — deferred, not
  fixed; expect it if gpg/gpgme is in the closure.
- **MAKEOPTS parallelism** — `todo/useconfig-clone-elimination.md`'s
  predecessor plan mentions this as a still-open blocker for the full
  cross-toolchain-under-`--prefix` live run; check whether it's actually hit
  here or was scenario-specific to a different combination.

## Order of execution

1. Scenario 1 (native `--root`) — cheapest to validate against a known-good
   prior baseline; if this regresses, stop and fix before touching the
   others (something this week's fixes broke would be the highest-priority
   finding).
2. Scenario 2 (`--prefix`) and Scenario 3 (`--local`) — new coverage, can run
   in either order or in parallel (separate subtrees, same sandbox).
3. Scenario 4 (cross, riscv64) — slowest (fresh cross toolchain build), most
   likely to hit a still-open issue; do last so scenarios 1–3's results
   aren't blocked on it.

## Out of scope for this pass

- Building all the way to a full `@system`/stage3 — this matrix is about
  `stages --stage1` specifically (the `toolchain → stage1` pipeline), not
  the later stages.
- Fixing anything found — this is a validation pass; findings get recorded
  here (or in `todo/stage-build-shakeout.md`, following its existing
  numbered-finding convention) and triaged afterward, not fixed inline
  mid-sweep unless trivial.

## 2026-07-12 session retrospective — a real finding, reached badly

**The actual finding, confirmed and worth fixing:** a native (self-hosting,
`CHOST==CBUILD`) `--root` stage1 build can find and *execute* a binary that
was just merged into the still-being-bootstrapped target root, when it
should only ever use the host/seed's own tools for anything build-time
(BDEPEND-class). Reproduced live: `dev-util/pkgconf`/`virtual/pkgconfig`
merge correctly into `/root/stage1` (a real RDEPEND, correctly satisfied
there — stage2/3 building *inside* this root later will need it present).
Later in the *same* stage1 batch, `app-portage/portage-utils`'s `configure`
resolves `pkg-config` to `/root/stage1/usr/bin/aarch64-unknown-linux-gnu-pkg-config`
and executes it — it's the right architecture (native, not cross), so it
doesn't fail-fast with "exec format error" the way a real cross build's
foreign-arch mismatch would; it runs, and only then dies unable to load its
own `libpkgconf.so.8`, because nothing ever chroots or sets
`LD_LIBRARY_PATH` to make that root's libraries visible to a process that
was never actually rooted there.

Root cause, traced to a specific line of reasoning, not a guess:
`docs/root-model.md`'s own rule sets `SYSROOT = ROOT` for a plain `--root`
(correct — DEPEND headers/libraries for a native self-hosted build
genuinely live in ROOT, `portage-repo/src/build/shell.rs:1628-1640`).
Autoconf's own cross-aware tool search (`AC_PATH_TOOL`/`PKG_PROG_PKG_CONFIG`)
independently uses that *same* `SYSROOT` value as an anchor for finding
`${host_alias}-pkg-config`, treating it as "where do cross tools for this
target live" — a second, different job the variable was never meant to do.
For a genuine cross build (foreign arch) that heuristic is harmless: the
binary it finds can't execute at all, so configure falls through to the
working plain `pkg-config` cleanly. Native bootstrap is the one case where
`CHOST` names the *same* architecture the build is already running on, so
the wrong binary is capable of starting — and fails much later, in a
confusing, unrelated-looking way. Two legitimately different concerns
(DEPEND anchor vs. an accidental tool-search anchor) sharing one variable,
and only native self-hosting stage1 exposes the conflict. Not fixed this
session — needs a real design pass on whether `SYSROOT` should differ from
`ROOT` specifically during stage1/toolchain bootstrap, or whether target-root
binaries need to be kept off any tool-search path outright regardless of
`SYSROOT`, before landing a change this central.

**How today actually got there, since the user asked this be recorded
honestly rather than polished into a clean bug report:**

- Landed two real, correct, independently-verified fixes first
  (`BOOTSTRAP_USE` re-add after stage1's `-*`, commit `9c63a2e`; per-package
  `ld.so.cache` refresh, commit `476491b`) — both are solid, both are
  committed, neither is in question.
- Then spent a long stretch chasing the `portage-utils`/`pkg-config`
  failure through **three wrong hypotheses in a row**, each stated with
  more confidence than it had earned:
  1. "The RDEPEND edge to `dev-util/pkgconf` is never walked at all" —
     falsified immediately by checking the VDB (it *was* merged).
  2. "It's an `ld.so.cache` timing gap, same class as the fix just landed" —
     plausible, matched the symptom shape, so it got built and shipped
     as a real fix (it's still correct and worth having) — but then
     **directly disproved** by an isolated single-package test that showed
     the cache *did* have the right entry, immediately, and the symptom
     persisted anyway.
  3. Reacted to that disproof by running `/root/stage1/usr/bin/...`
     directly from the sandbox's own shell as a "test" — which tests
     nothing, since a bare path exec doesn't chroot and was never going to
     resolve the way an in-build invocation does. The user had to point
     this out directly ("why the fuck are you running a binary from the
     stage1??") before it was recognized as a meaningless check.
  4. Only after the user asked pointedly whether it was really a
     `BDEPEND` and pushed back on "not new"/"already documented" being
     treated as synonymous with "acceptable" did the actual mechanism
     (SYSROOT's dual, conflicting role) get found — and even that took a
     direct nudge ("let me guess your bashrc adds stage1 to the path")
     before the right code path got read.
- Separately, and earlier in the same session: repeated the *exact*
  `sudo chroot`-instead-of-`sandbox run` mistake that had already been
  called out and turned into a hard memory rule minutes earlier, catching
  it only because the user interrupted before real work happened; ran
  `sandbox prepare`'s full (rust/crossdev/dracut/...) install when a
  `--bare` sync was all that was needed, until the user pointed out the
  new `--bare` flag existed; and re-ran an identical failing command
  verbatim without checking whether a manual `package.use`/VDB edit had
  even taken effect first ("what the fuck are you doing" — deserved).
- The throughline across all of it, named directly by the user
  ("YET AGAIN you conflate a mode with another"): this session kept
  mixing up which of several similar-but-distinct things applied to a
  given problem — `config_root` vs. `merge_root` for where config lives,
  `RDEPEND` vs. `BDEPEND` for what a virtual's dependency really means,
  and finally `SYSROOT`-as-DEPEND-anchor vs. `SYSROOT`-as-tool-search-anchor
  for the actual bug. Each individual mix-up got resolved once pointed at
  directly, but none of them were caught *before* being pointed out —
  worth treating "which of these near-identical concepts actually applies
  here" as a question to answer explicitly before acting, not after being
  corrected.

## `em crossdev --setup` across all three root-mode variants (2026-07-12, after the USE_EXPAND fix)

Retested `em crossdev --setup` (cross target `riscv64-unknown-linux-gnu`)
across the three root-mode combinations, on a fresh `em-crossdev-test`
sandbox (`sandbox setup` + `sandbox prepare --bare`), after landing the
USE_EXPAND fix (commit `d602de1`) — this is what originally motivated that
fix: the plain `--target` case regressed to a `dev-libs/libiconv`/glibc
file collision before the fix, now confirmed clean.

- **Plain `--target riscv64-unknown-linux-gnu` (native, no EPREFIX):
  full success**, matching the pre-regression baseline exactly (51
  packages, byte-identical `-pv` plan to a binary built from `d9f1f90`).
  `>>> cross toolchain riscv64-unknown-linux-gnu ready in //usr/riscv64-unknown-linux-gnu`.
- **`--prefix /var/tmp/px` (overlay, BROOT = host): full success.**
  `em crossdev --target riscv64-unknown-linux-gnu --prefix /var/tmp/px --setup`
  completed end-to-end: `>>> cross toolchain riscv64-unknown-linux-gnu
  ready in /var/tmp/px/usr/riscv64-unknown-linux-gnu`. Needed `em setup
  --prefix /var/tmp/px` run first (just the directory/config skeleton;
  `--prefix` borrows the host's own toolchain, so nothing further is
  needed before `crossdev --setup`).
- **`--local` (standalone Gentoo-Prefix, own BROOT): fails at preflight,
  before merging anything — real gap, not yet root-caused.** Needed `em
  setup --local` (skeleton) *and*, per the `local-eprefix-mode` convention,
  `em toolchain --setup --local` (build the prefix's own native base
  toolchain — the prefix has no host-shared BROOT to borrow from). The
  toolchain bootstrap itself fails immediately:
  ```
  error: pre-flight dependency check failed — ...
    sys-devel/gcc-16.1.1_p20260613 needs: >=dev-libs/gmp-4.3.2:0=, sys-devel/gettext, sys-libs/glibc[cet(-)?]
    sys-libs/glibc-2.43-r2 needs: || ( dev-lang/python:3.14 dev-lang/python:3.13 dev-lang/python:3.12 )
    ... (a dozen more DEPEND-class entries: gettext, meson, gmp, python, m4, ...)
  ```
  **Initially misdiagnosed** (corrected mid-session after a direct
  challenge) as the already-documented `--local` preflight-vs-PATH-tools
  gap from earlier findings in this file — that was wrong, reached by
  pattern-matching the *words* "pre-flight dependency check failed"
  without checking whether the *content* matched. The earlier documented
  gap is specifically about real host tools (`xz-utils`, `meson`) being on
  `PATH` but invisible to a VDB-only check; this list is dominated by
  plain `DEPEND`-class items (`gettext`, `gmp`, `python`) that `--root-deps=rdeps`
  is supposed to exempt from this exact preflight check entirely —
  `toolchain()` (`crossdev/mod.rs`) forces `root_deps = true`
  unconditionally for `toolchain --setup`, same as bare `--root` gets.
  **The open question**: `Roots::root_set()`/`broot()` configure `--local`
  and bare `--root` *identically* (both set `broot` to their own offset —
  "own BROOT", self-contained model) — yet bare `--root`'s `toolchain
  --setup` got 30+ packages successfully merged before hitting an
  unrelated failure (see the earlier `virtual/os-headers`/kernel-headers
  finding, out of scope per that finding's own note), while `--local`'s
  fails at preflight before merging *anything*. Why the same `broot`
  configuration produces such different outcomes — whether
  `--root-deps=rdeps`'s exemption isn't actually reaching the `--local`
  preflight check the same way it does for `--root`, or something else
  entirely — is not yet traced. Needs its own investigation, ideally
  starting from `preflight.rs`'s actual `DepClass::Depend` check alongside
  `merge_flags.root_deps`'s consumption, comparing the two modes directly
  rather than assuming they're equivalent because their `Roots` values
  look the same on paper.

**Also found, CLI usability**: `em --target T --local crossdev --setup`
(global flags before the subcommand) mis-parses — clap's optional-value
`--local [<DIR>]` greedily consumes `crossdev` as its directory argument,
producing a confusing `unexpected argument '--setup'` error nowhere near
the real cause. Putting the subcommand first works correctly: `em
crossdev --target T --local --setup`. Worth a small usage-doc note or a
clap fix (e.g. requiring `=` for `--local`'s optional value) so this
doesn't cost someone else the same confusion — not investigated further
this session.

## `--local` bootstrap failures: two distinct bugs found, one fixed, one open

Follow-up on the `--local` preflight-explosion finding above. Confirmed
`em setup --local` itself works cleanly in isolation (fresh sandbox: exit
0, produces exactly `etc/portage/{bashrc,make.conf}` + the bare skeleton,
idempotent). Everything below is what happens *after* that, still on a
fresh, properly-`sandbox prepare --bare`d sandbox.

**Bug 1 — solver pulls a hugely inflated closure under `--local`,
regardless of which command drives it.** Both `em toolchain --setup
--local` (step `[2/5] binutils`) and `em crossdev -T
aarch64-unknown-linux-gnu --local --setup` (a same-arch "cross" target,
step `[1/6] binutils`) fail at their first real build step with the
identical-shaped pre-flight explosion: not just binutils' own direct
`gettext`/`m4` need, but `gcc`/`glibc`/`gmp`/`mpfr`/`mpc`/`meson`/python,
`sys-apps/portage`'s whole python-dependency chain (`mypy`, `requests`,
`gemato`, `maturin`), and several USE/multilib-ABI-conditioned deps
(`glibc[cet(-)?]`, `glibc[-crypt(-)]`, `libxml2[abi_x86_32...]`). The
aarch64 crossdev run additionally pulls in `dev-vcs/git`/`gnutls`/`nettle`/
`libidn2`/`libpsl` (crossdev's own extra deps) on top. **Ruled out**: this
is not applet-specific (both `toolchain` and `crossdev` hit it identically)
and not about the package list (same shape both times) — it's something
about `--local`'s own `Roots`/solver-satisfaction configuration, since the
exact same `binutils` step succeeds cleanly under bare `--root` even
though `Roots::root_set()`/`broot()` configure `--local` and `--root`
*identically* on paper (both "own BROOT", their own offset — confirmed by
reading `cli.rs` directly, not assumed). Next step: compare the solver's
`InstalledPolicy`/host-satisfaction logic's actual behavior between the
two modes directly (not just their `Roots` values), since that's the
layer that decides whether a `DEPEND` item gets treated as "the host
already has an equivalent" vs. "must be built fresh into this plan" —
`preflight.rs` itself has no `root_deps`-awareness at all (checked: zero
references), so whatever's inflating the closure happens upstream of
preflight, during solving.

**Bug 2 — RESOLVED, was a test-methodology artifact, not a `--local` bug.**
Originally recorded as: `em crossdev -T riscv64-unknown-linux-gnu --local
--setup` fails *before* reaching preflight at all, `error: no ebuilds found
for 'cross-riscv64-unknown-linux-gnu/binutils' (searched ::gentoo and
overlays)`, right after printing `>>> cross target riscv64-unknown-linux-gnu
ready` as if the overlay/alias setup had succeeded — contrasted with the
same-arch aarch64 case, whose overlay resolved fine.

Traced to the real cause: the riscv64 run above reused the *same* `--local`
prefix the aarch64 run had already `--setup` on. `em crossdev`'s alias
repos.conf entry used one fixed name (`crossdev.conf`, section
`[crossdev]`) regardless of target, and `--setup` refreshes config via
`RefreshPolicy::FillGapsOnly` (presence-only — "the file exists" is enough,
by design, so hand edits made between an `--init-target` and a `--setup`
survive). So the aarch64 alias file was already present, `--setup` saw
"already there" and left it untouched, and the riscv64 target silently got
no alias at all. Confirmed by re-running with `--init-target`
(`RefreshPolicy::Sync`, content-compared) instead of `--setup` on the same
prefix: the alias refreshed correctly and the riscv64 overlay resolved.
Nothing about `--local`'s repo/config view was actually broken — `--root`/
`--prefix` "worked" for riscv64 in this session's earlier pass only because
those runs happened to use a fresh prefix per target, never hitting the
same collision.

**Fixed** (commit `89a151a`): each cross target now gets its own alias
file/section, `crossdev.<tuple>.conf` / `[crossdev.<tuple>]`, via a new
`overlay_name(target)` helper (`crossdev/mod.rs`) threaded through
`ConfigEntry::Alias`'s new `name` field (`config_plan.rs`). Multiple
targets now coexist on one prefix regardless of `--setup` vs
`--init-target`, closing off this failure mode structurally rather than
just documenting the collision. Live-reverified: `crossdev.aarch64-unknown-linux-gnu.conf`
and `crossdev.riscv64-unknown-linux-gnu.conf` both present after
`--init-target`ing each in turn on one `--local` prefix, both
`cross-*-unknown-linux-gnu/binutils` atoms resolve with zero "no ebuilds
found" (both still go on to hit Bug 1 below, which is unrelated and
expected).

Both targets, once alias resolution is out of the way, hit Bug 1 (below)
identically — that one's still open.

## `em stages --stage1` real (non-`-p`) re-validation, 2026-07-12, fresh sandboxes with today's binary

Rebuilt release binary (includes the multi-alias fix, unrelated to this),
fresh `crossdev-stages sandbox setup` + `sandbox prepare --bare` per
scenario (`em-stage1-live`, `em-stage1-prefix-live`), `--autosolve-use
--jobs 4`, no `--keep-going` (per standing rule — so each run stops at its
first real failure rather than surveying every package in one pass).

**Native `--root`: root-caused the previously-abstract "SYSROOT dual role"
finding down to a concrete, fixable gap.** 50 of 89 packages merged, then
died on `app-portage/portage-utils`'s `econf`: `.../aarch64-unknown-linux-gnu-pkg-config:
error while loading shared libraries: libpkgconf.so.8: cannot open shared
object file`. `dev-util/pkgconf` (providing that exact library) had merged
successfully 35+ packages earlier — this is the identical symptom
`476491b`'s per-package `ld.so.cache` refresh was meant to fix, recurring
after that fix already landed, which is the tell: refreshing
`/root/stage1-testing/etc/ld.so.cache` can't matter here, because `em`
never chroots into `ROOT` for build execution (confirmed: zero `chroot`
calls anywhere in `portage-cli`/`portage-repo` outside doc-comments) — a
target-root binary that `configure`'s `AC_PATH_TOOL`/`PKG_CHECK_MODULES`
finds via `$ESYSROOT` (`portage-repo/src/build/commands/econf.rs:54`) and
executes directly resolves its shared-library deps through the *calling*
process's own namespace (this sandbox's own `/etc/ld.so.cache`), not the
target root's.

Root cause, precisely: `setup.rs`'s three bashrc recipes
(`self_contained`/`is_overlay`/else, lines 190-199) — `--prefix`
(`BASHRC_PREFIX`) and `--local` (`BASHRC_LOCAL`) both `export
LD_LIBRARY_PATH="${_ov}/usr/${_libdir}..."` for exactly this reason (see
`BASHRC_LOCAL`'s own comment, lines 58-64: "tools whose rpath the host
loader still doesn't search — needs the prefix libdir on the runtime
search path"). Bare `--root` (`self_contained`) gets an **empty** bashrc —
deliberately, per the comment at lines 174-189, but that comment's
rationale is specifically about *not* injecting `CPPFLAGS`/`LDFLAGS` (compile/link-time
search paths, which the SYSROOT/CHOST toolchain wiring already handles
correctly and which injecting would actively break, per the 2026-07-03
`obstack.h` incident). It says nothing about *runtime* shared-library
resolution for a directly-executed tool binary, which is a narrower,
separate need `--root` still has and doesn't get — the two other modes
have it as an accident of already needing an EPREFIX-keyed bashrc for
other reasons, not because someone reasoned about this case for them
specifically either.

**Fix direction** (not yet implemented): bare `--root` needs its own
minimal bashrc exporting `LD_LIBRARY_PATH` for its own `usr/$(get_libdir)`
— just the runtime search-path line, none of `BASHRC_PREFIX`/`BASHRC_LOCAL`'s
`CPPFLAGS`/`PKG_CONFIG_*`/`CMAKE_PREFIX_PATH` machinery (those exist to
bridge two *different* trees; `--root` only has the one).

**`--prefix`: reconfirmed the already-documented `cede_required_use`
early-return bug, now with a harder failure mode than previously seen.**
15 of 36 packages merged, then `app-alternatives/awk-4` **died at
`src_install`** (not just an unsatisfied-REQUIRED_USE report): `die: No
selected alternative found (REQUIRED_USE ignored?!)` — the eclass's own
`get_alternative()` runtime guard firing because every alternative flag is
really off. Same root cause as the earlier-documented finding
(`Adapter::cede_required_use`'s `installed_cpvs.contains(cpv)` early return
skipping Level-C reconsideration because the host already has
`app-alternatives/awk` installed) — this run just demonstrates it's not
merely a cosmetic "unsatisfied REQUIRED_USE reported" gap, it's a hard
build-stopping failure for any real `--prefix` stage1 attempt. Still open,
same fix direction as previously recorded (drop the `installed_cpvs`
early return, rely on the already-correct `unsatisfied.is_empty()` check).

**Net effect**: neither native `--root` nor `--prefix` currently completes
a real `stages --stage1` build end-to-end. `--root` gets further (50/89)
before hitting the LD_LIBRARY_PATH gap above; `--prefix` stops much sooner
(15/36) on the known `cede_required_use` bug. `--local` isn't a meaningful
third data point yet — it can't even get past its own `toolchain --setup`
prerequisite (Bug 1, above). Cross (native-target, e.g. riscv64) previously
completed a full real build in this file's earlier entries and doesn't hit
either of these (its target sysroot VDB is fresh, so `cede_required_use`
doesn't misfire, and it's a genuine cross build so the direct-tool-execution
gap doesn't apply the same way — worth double-checking that assumption
before relying on it, not done this pass).

## The native `--root` LD_LIBRARY_PATH framing above was wrong — fixed the real bug instead

The write-up above proposed adding an `LD_LIBRARY_PATH` bashrc export for
self-contained `--root`, treating the missing-shared-lib symptom as the
bug. Direct challenge ("even for --root stages should not set the PATH to
include the stage1 this is a different bug") led to finding the *actual*
mechanism: `portage-repo/src/build/shell.rs`'s `run_phase` unconditionally
prepended `<root_str>usr/bin` onto `PATH` for **any** self-contained
`--root` build (not stage1-specific — `em stages --stage1` is just a thin
wrapper computing `use_override` and handing off to the exact same generic
`emerge_atoms` path any `em --root <dir> <atoms>` call uses, confirmed by
reading `crossdev::run_staged`). That's why `configure` found and directly
executed the ROOT-installed `pkg-config` in the first place — a plain
`$PATH` hit, not an `ESYSROOT`/`AC_PATH_TOOL` cross-search as first
claimed.

That PATH-prepend was itself the bug, not something needing a companion
`LD_LIBRARY_PATH` fix: it was written for one narrow, real, previously
live-verified case (`sys-libs/glibc`'s `get_kheader_version()` needing to
find `${CTARGET}-cpp` during a genuine cross-toolchain bootstrap,
`stage-build-shakeout.md`'s 10th finding, 2026-07-03) but generalized "by
analogy" to *also* fire for any plain native self-contained build with no
cross involved at all — a case that was never actually live-tested until
this session's real (non-`-p`) re-run found it actively harmful.

**Fixed** (`portage-repo/src/build/shell.rs`): narrowed the condition from
`(build_config_root.is_none() || cross_host_tool_tuple.is_some())` to just
`cross_host_tool_tuple.is_some()` — the PATH-prepend now only fires when
building `cross-<T>/{binutils,gcc,gdb,clang-crossdev-wrappers}` themselves,
never for a plain self-contained bootstrap with no cross target at all.

**Risk found and checked before trusting it**: `cross_host_tool_tuple`'s
`pn` filter doesn't include `glibc` — so the *original* motivating case
(cross `libc`'s `get_kheader_version`) gets no PATH-prepend either way
under the new condition, raising a real concern that the narrowing had
just reopened the 2026-07-03 bug. Checked empirically rather than assumed:
re-ran a full `em --target riscv64-unknown-linux-gnu crossdev --setup`
from scratch with the fixed binary. Result: `* Checking linux-headers
version (7.1.0 >= 3.2.0) ...` — the correct value, not `0.0.0` — and the
full 6-step bootstrap completed (`>>> cross toolchain
riscv64-unknown-linux-gnu ready`). Not regressed.

Why it isn't regressed: `tc-getCPP`'s fallback (`toolchain-funcs.eclass`)
is `"${CC:-gcc} -E"` — when no `${CTARGET}-cpp`/`${CTARGET}-gcc` is found
on `PATH`, it falls back to the plain host `gcc -E`. A C preprocessor
reading `linux/version.h` for a `#define` macro is architecture-agnostic —
the host's own `cpp`, given the explicit `-I "${ESYSROOT}$(alt_headers)"`
the ebuild already passes, reads the right value regardless of which
architecture's `cpp` binary actually runs. So the original fix's own
motivating case turns out not to have needed the ROOT-installed binary
specifically either — the same "any correctly-invoked tool works if given
the right sysroot/include path, no need to execute a guest binary" logic
that fixed the `pkg-config` case applies here too, just via the eclass's
own fallback rather than anything `em` needed to add.

Re-verified the native `--root` case too with the fixed binary
(`em-stage1-live` sandbox, real `em stages --stage1 --root
/root/stage1-testing --autosolve-use --jobs 4`): `app-portage/portage-utils`
now merges cleanly (`registered (counter=69)`); the run got to 24/54
merged before a new, unrelated failure — `dev-lang/perl-5.42.2`: `phase
unpack failed: shell error: src_unpack: die: unpack failed` — not
investigated further, looks like the same class of distfile-reliability
gap already tracked in `todo/distfile-fetch-reliability.md`, not an `em`
regression from this fix.

Full workspace suite (1217 tests), clippy, and fmt all clean with the
narrowed condition. The remaining `cross_host_tool_tuple`-gated
PATH-prepend (for `binutils`/`gcc`/`gdb` finding *each other's* already-
merged output during the toolchain's own bootstrap) is left as is — it's
demonstrated necessary (the whole crossdev bootstrap needs it structurally,
since a genuinely foreign-arch tool has no host fallback the way `cpp`
does) and is out of scope to redesign further this pass, though the same
"pass the right flag instead of executing a guest binary" philosophy may
apply there too (e.g. `-B`/`--with-as`/`--with-ld` instead of a live PATH

## Two more real fixes (2026-07-12): tar ownership under a constrained sandbox, and `--autosolve-use` wiped by its own `-*`

Re-running the real (non-`-p`) native `--root` `stages --stage1` build after
the `LD_LIBRARY_PATH`/PATH-prepend fix above surfaced two further genuine
bugs, both now fixed and committed (`b880d84`, `5b00c74`):

**`dev-lang/perl` (and by extension anything with a similarly-shaped
release tarball) died `unpack failed`.** Root cause: nothing to do with
distfile fetch/corruption (the archive verified clean with `tar tJf`) — GNU
`tar` defaults to `--same-owner` when the calling process is real root, and
perl's official tarball embeds file ownership `uid/gid 197609` (and `544`
for the top dir), values outside this sandbox's user-namespace uid map
(confirmed via `/proc/self/uid_map`: only uid `0` and `1-65536` are
mapped), so the `chown()` call fails `EINVAL`. `em` runs its whole build as
real root here (`privilege.rs`'s own rule: "Already root ⇒ no wrapping"),
so it never got the `--no-same-owner` protection a normal
`FEATURES=userpriv` Gentoo build gets for free (tar defaults to
`--no-same-owner` when *not* literal root). Fixed: `unpack.rs` now passes
`--no-same-owner` unconditionally on every tar extraction — WORKDIR
ownership doesn't matter for the build per PMS; only install/qmerge
(already correctly privilege-scoped) needs real ownership. Caught and fixed
a real mistake in the first attempt too: putting `--no-same-owner` *before*
the old-style combined mode letters (`xjf` etc.) makes GNU tar stop parsing
them as short options entirely and die "You must specify one of
'-Acdtrux'" — verified the corrected (trailing) argument order directly in
the sandbox before trusting it.

**`--autosolve-use` never actually worked under `USE="-* build"` — its own
core use case.** `app-alternatives/lex` died `No selected alternative
found (REQUIRED_USE ignored?!)` on a completely fresh `--root`, even though
`--autosolve-use` correctly *reported* it would cede `+reflex`. Root cause:
the ceded decision was folded into a synthetic `package.use` entry — but
this session's own earlier `USE=-*` layer-fold redesign correctly makes
`package.use` lose to an *env-level* `-*`, and `em stages --stage1`'s own
`use_override` operates at exactly that layer. So the ceded flag was wiped
out by the very `-*` that made ceding necessary in the first place — this
bug meant `--autosolve-use` could never have worked for a real `USE="-*
build"` stage1/catalyst-style build, on any root mode, the whole time.
Also explains the earlier-flagged "display shows pre-autosolve flags"
cosmetic bug (same root cause). Fixed: ceded flags now apply as a final,
unconditional override (`effective_use::apply_ceded`), the same standing as
`use.force`/`use.mask`, threaded through the merge plan, the REQUIRED_USE
check, the download-size estimate, and the `-p` display. Two new unit
tests (`effective_use.rs`); live-verified `app-alternatives/lex` (and,
in the full re-run below, `app-alternatives/awk` too) now actually merge
under `USE="-* build"`.

**Net result of this pass's two fixes**: native `--root` `stages --stage1`
went from stalling at 24-50 of ~89 packages to **147 of ~148 merging
cleanly**, reaching `sys-devel/gcc` itself.

**New, different, real finding at that point — out of scope for this
pass**: `gcc-16`'s own `libgcc` build fails `fatal error: stdio.h: No such
file or directory`. Confirmed `sys-libs/glibc` is not merged into the
target root at all (no `var/db/pkg/sys-libs/glibc*` entry there).

**Correction, checked directly after an initial wrong claim**: first said
`packages.build` doesn't reference glibc at all — wrong, and only reached
by grepping for the literal string "glibc" and missing the indirection.
`profiles/default/linux/packages.build` (part of the active stack) lists
`virtual/libc`, whose own `RDEPEND` is `elibc_glibc? ( sys-libs/glibc:2.2
)`. It *is* in the plan: `[ebuild R] virtual/libc-1-r1` registered
(`counter=101`) in this run. The real mechanism: `virtual/libc`'s RDEPEND
was satisfied by checking the **host's** VDB (`broot`, which genuinely has
`sys-libs/glibc-2.43-r2` installed at its own real `/`, confirmed via
`qlist -Iv`) rather than by merging a real `sys-libs/glibc` into
`/root/stage1-testing` — so `virtual/libc` shows as a same-version no-op
reinstall with nothing underneath it in the target root at all.

That's the same underlying question as before, just precisely mislocated
at first: real catalyst's stage1 assumes a pre-existing "seed" toolchain
(baked into the stage0-produced seed tarball it starts from) — a plain
`--root` build's RDEPEND-satisfied-by-broot logic is *correct* for real
portage's own `ROOT=X emerge` semantics (confirmed empirically earlier
this session: a host-satisfied virtual doesn't need its own copy in an
ordinary offset install) and is *correct* for every other package in this
run. It only breaks for `gcc`'s own bootstrap specifically, because
`econf` correctly passes `--with-sysroot=<ROOT>` for a genuine from-scratch
build (deliberately isolating it from the host's own headers, unlike
ordinary packages, which fall back to the host's default header search
when nothing chroots) — so it fails exactly where it should, given the
target truly has no libc.

**Resolved, differently than the "needs a design decision" framing above
suggested.** Prompted by "so stages should layer the reset + build at the
right level to make it work fine": the deeper issue wasn't "needs a seed
toolchain" — the design decision. `virtual/libc`'s `elibc_glibc? (
sys-libs/glibc:2.2 )` RDEPEND wasn't firing because `USE_EXPAND_IMPLICIT`
tokens (`ELIBC`/`KERNEL`, folded as `elibc_glibc`/`kernel_linux` at the
profile's "defaults" layer) get wiped by stage1's own `-*`, same as
`BOOTSTRAP_USE`'s own flags — verified directly against real portage's
`config.py` `regenerate()`. Fixed in commit `663e38b`: `bootstrap_use()`
(`crossdev/mod.rs`) now also reads `ELIBC`/`KERNEL` from the profile chain
and appends `elibc_${ELIBC}`/`kernel_${KERNEL}`, the same re-add
`BOOTSTRAP_USE` itself already needed. Live-verified: the packages.build
step's USE now reads `"...zstd elibc_glibc kernel_linux"`, and
`sys-libs/glibc-2.43-r2` now appears as a real `[ebuild N]` merge target.

Same commit also fixed a related, independent bug found along the way:
`em stages --stage1`'s `USE="-* build ..."` was applied via a real
`std::env::set_var("USE", ...)` (the process-env layer, above
`package.use`) instead of catalyst's actual placement (`make.conf`, the
conf layer, below `package.use`) — meaning any real `package.use` entry
got silently wiped during a stage1 run. Replaced with a proper conf-layer
fold (`ConfSource`/`use_flags_with_override` in `portage_repo::build::profile`,
threaded through `DepgraphOpts::extra_use_override`). New unit test
(`use_flags_with_override_lands_in_pre_env_not_env_use`) proves the
override lands in `pre_env`, not `env_use`.

**New, separate, unrelated bug found while verifying the above**:
`package.use` isn't applied *at all* for a bare `--root`, independent of
either fix above — confirmed on a fresh root with zero stage1 involvement
(`em --root <dir> -pv app-alternatives/tar` with a `package.use` entry
disabling `gnu`/enabling `libarchive` still showed `gnu` enabled,
`libarchive` disabled — the untouched IUSE defaults, not the configured
entry). Not investigated further this pass — flagging for its own pass,
since it's a real, orthogonal correctness gap.

Also noticed, non-blocking, not chased: two `error: the following required
arguments were not provided: <ATOM>` (`has_version` called with no atom,
during `dev-lang/python`'s build) and two `error: declare: cannot mutate
readonly variable` (during `sys-devel/gcc`'s Gentoo-patch application) —
neither stopped its package from registering successfully, so flagged here
for visibility rather than investigated.

## Post-fix crossdev regression sweep (2026-07-15), plus two quirks investigated to a real conclusion

After all the fixes above, re-ran `em crossdev --setup` (riscv64 target) on
fresh sandboxes in two more root modes to confirm nothing regressed:

- **Bare invocation (no `--root`, real host `/`)**: full 6-step bootstrap
  completes, `>>> cross toolchain riscv64-unknown-linux-gnu ready`,
  `linux-headers version (7.1.0 >= 3.2.0)` correct both `glibc` merges.
- **`--prefix /var/tmp/xp`**: same, full success, `ready in
  /var/tmp/xp/usr/riscv64-unknown-linux-gnu`.

Both hit two cosmetic, non-fatal messages, investigated to a definitive,
real (not `em`-specific) root cause rather than dismissed on sight:

- **`aarch64-unknown-linux-gnu-gcc: error: unrecognized command-line
  option '-mno-relax'`**: `glibc-2.43/nptl/Makefile:537`'s own
  `dir=$(CC) $(CFLAGS) $(CPPFLAGS) -print-multi-directory` — for this one
  recursive `install-headers` sub-make, `$(CC)` resolved to the host
  compiler while `$(CFLAGS)`/`$(CPPFLAGS)` stayed correctly RISC-V-typed.
  Narrow, isolated to this one multidir-name computation in the
  headers-only pass; doesn't affect real compilation elsewhere in the
  same log (confirmed by sampling occurrences after the `all` target
  starts — those are glibc's own, entirely correct `BUILD_CC`
  autoconf probing, not the same issue recurring). Non-blocking, not
  chased further — not clearly attributable to `em` vs. upstream
  GNU-make variable propagation without a real-`emerge` comparison.
- **`failed to redirect to <EROOT>/etc/hosts` / `sed: can't read
  <ED>/etc/host.conf`**: fully root-caused in `sys-libs/glibc`'s own
  ebuild, not `em`. `src_install` has an explicit `is_crosscompile &&
  ... return 0` before the section that installs `/etc/host.conf`/
  `nsswitch.conf`/`gai.conf` (`alt_prefix()`/`is_crosscompile()`,
  `glibc-2.43-r2.ebuild:275,1646-1659` — real portage's own cross-vs-native
  contract, "CHOST = CTARGET → install into /; CHOST != CTARGET → install
  into /usr/CTARGET/"), so a cross build never installs those files at
  all. `pkg_postinst` isn't equally gated: it unconditionally `sed`s
  `${ED}/etc/host.conf` (predictably absent, hence the message) and reads
  `${EROOT}/etc/hosts` (a real read, not a write — succeeds silently
  against a real host's own file, fails against a genuinely fresh
  `--root`/`--prefix` that's never had `baselayout` merged into it, since
  `crossdev --setup` alone never merges it). Confirmed real crossdev
  would hit the identical behavior if pointed at a fresh `ROOT=` the same
  way (it never sets `ROOT=` itself, so in normal use its `EROOT` is
  always the real, already-populated host, which is why this never
  surfaces there) — not an `em` bug, not a real file collision with the
  host's own native glibc, purely cosmetic. No action needed.
