# For Sonnet — live verification handoff

**Current pin (do this next):** `master` at **`f8ac293`**  
(“fix: honour bashrc die; seed baselayout for all toolchain plans”)

Older pins below are historical. Build `em` from **`f8ac293`** (or tip if
only docs landed after) before each campaign; note the SHA in Results.

Unit tests already green. Unit tests do **not** catch silent host/target env
or workdir races under real parallelism.

---

## NEXT HOMEWORK — `f8ac293` (Sonnet)

**Goal:** re-run clang Scenario A after baselayout + bashrc-die fixes; confirm
#4/#5, and either clear or pin #3 (libxcrypt never scheduled). Record + stop
on the first new hard failure.

### Rules (same as before)

1. **Fresh sandbox only** — do not reuse `em-a46027b-verify` or any half-failed tree.
2. **No `--keep-going`.**
3. Record findings here under **Results**; do not implement large design fixes
   in the same pass unless a one-line regression of `f8ac293`/`a46027b`.
4. pam **is expected** in the clang graph (not a bug):
   `clang → python → util-linux[pam] → sys-libs/pam` under
   `default/linux` `USE=pam`. Do not spend time “removing” pam from the plan.

### Commands (paths illustrative)

```sh
# build em at f8ac293
git -C /path/to/portage-cli rev-parse --short=8 HEAD   # expect f8ac293 or later docs-only

P=/root/xp   # fresh bare sandbox root
em --prefix "$P" setup
em --prefix "$P" --target riscv64-unknown-linux-gnu crossdev --setup --jobs 8
# Expect EXIT=0, real riscv64-unknown-linux-gnu-gcc
# Spot-check: baselayout ran *before* libc in the staged plan/log

# layout check after crossdev --setup (bug #4)
# merged-usr: bin should be a symlink into usr (or equivalent profile layout),
# not two independent real directories with different content.
ls -la "$P/usr/riscv64-unknown-linux-gnu/bin" \
       "$P/usr/riscv64-unknown-linux-gnu/usr/bin" | head

em --prefix "$P" --target riscv64-unknown-linux-gnu \
  -b llvm-core/clang --jobs 80
```

### Pass / fail checklist

| Check | Pass if |
|-------|---------|
| **#4 baselayout** | `crossdev --setup` plan/log has baselayout before libc; after setup, sysroot is **not** genuine split-usr (`bin` vs `usr/bin` both real, divergent content) |
| **#5 bashrc die** | Zero silent “Completed after `die: … merged-usr … split-usr`” — if split-usr still exists, those packages must **fail**, not complete |
| **#1/#2 still fixed** | No sed ACL race; no empty-ED `tar: Cannot open` on virtuals; virtuals still get `.gpkg` under `-b` |
| **#3 libxcrypt** | Either (a) `sys-libs/libxcrypt` gets `Emerging` **before** pam configure, and pam finds libcrypt, **or** (b) still missing — then capture: plan line for libxcrypt, whether any `Emerging`, VDB path, and whether virtual/libcrypt completed earlier |
| **Progress** | Note N/M; whether `llvm-core/clang` is reached |
| **Workdir** | Still no doubled phases / dual WORKDIR for same CPV host+target |

### If #3 still fails — collect this (do not fix yet)

```sh
# from the failed sandbox, after EXIT=1:
rg -n 'libxcrypt|libcrypt|sys-libs/pam' "$LOG" | head -80
ls "$P/usr/riscv64-unknown-linux-gnu/var/db/pkg/sys-libs/" 2>/dev/null
ls "$P/usr/riscv64-unknown-linux-gnu/usr/include/crypt.h" 2>/dev/null
# From the initial -p or plan dump of the same run: was libxcrypt [ebuild N]?
```

Hypotheses for Grok (you only need evidence):

1. silent VDB skip (host weave / already-installed false positive)
2. RDEPEND edge virtual/libcrypt → libxcrypt missing at schedule time (USE `prefix-guest` / `elibc_glibc`)
3. plan listed it but never dequeued (blocker / stop_new)

### Optional if P0 green and time left

- P1 package.env letter spot-check (GCC crossdev --setup host vs target env files)
- Pretend purity smoke (already green on a46027b; quick re-check only if easy)

### Out of scope this pass

- Implementing package.provided / --local bootstrap
- Reintroducing BuildClass
- Designing multi-em plan registry

---

## Context (what Grok landed — cumulative)

| Commit | What |
|--------|------|
| `56435d4` | Per-root workdirs; setup/crossdev `-p` honour |
| `480daff` | Crossdev `-p` in-memory aliases |
| `fad35a3` | Drop BuildClass |
| `a46027b` | RDEPEND in `build_blockers` (sed/acl); empty-ED `--buildpkg` |
| `f8ac293` | bashrc `die` propagates; baselayout for **all** `toolchain_plan` |

Plans / matrices:

- [[drop-buildclass]] Step 5 live table  
- [[workdir-dual-root]] landed; keep regression-watching Scenario A  
- [[local-bootstrap-provided]] open (not this handoff)  
- [[clang-crossbuild-prefix-local-test-plan]]  

Matrix: [`docs/bash-crossdev-matrix.md`](../docs/bash-crossdev-matrix.md)

---

## Rules

1. Fresh sandboxes only (`crossdev-stages` / project sandbox recipe). Never
   reuse a half-failed tree.
2. **No `--keep-going`** on staged/toolchain/clang high-jobs runs.
3. Prefer **record + stop** on new bugs; do not invent workarounds unless asked.
4. Build `em` from this tree’s tip before each campaign; note the SHA in the log.
5. Append results to this file under **Results** (and/or a dated subsection in
   the clang test plan). Do not silently “fix” design in the same pass unless
   the bug is a one-line obvious regression of the commits above.

---

## Priority queue (historical — first handoff)

### P0 — Re-verify workdir dual-root fix (clang Scenario A)

**Why:** Previously blocked at ~66/136 under `--jobs 80` with dual WORKDIR
race. Code now keys workdir by merge root.

```sh
# Fresh prefix sandbox; paths illustrative — match project sandbox helpers
em --prefix "$P" setup
em --prefix "$P" --target riscv64-unknown-linux-gnu crossdev --setup --jobs 8
# Expect EXIT=0, real riscv64-unknown-linux-gnu-gcc

em --prefix "$P" --target riscv64-unknown-linux-gnu \
  -b llvm-core/clang --jobs 80
```

**Pass if:**

- No phase doubled in one `build.log` for dual-role packages  
- Workdirs for host vs sysroot merges differ under `$P/var/tmp/portage/`  
  (look for `host/` vs `…usr-riscv…` style root-keys)  
- `llvm-core/clang` eventually builds (or fails for a **new**, well-documented
  reason — not shared WORKDIR `newins`)

**Fail / record if:** still double-phase logs, same path under
`var/tmp/portage` for host+target same CPV, or clang never starts for the
old reason.

Also note: pre-refactor bonus binpkg path-doubling on host dual-role
`clang-stdlib-config` (`image/root/xp`) — check if still present under
current tip when `-b` runs.

### P1 — Drop BuildClass / package.env live (drop-buildclass Step 5)

On a **fresh** prefix (or reuse only if crossdev --setup just succeeded):

#### 1. GCC linux-gnu toolchain

```sh
em --prefix "$P" --target riscv64-unknown-linux-gnu crossdev --setup --jobs 8
```

Spot-check after (or mid-run for one package) that package.env under the
**outer** `etc/portage` is letter-faithful:

| Package class | Expect in `env/cross-*/…` |
|---------------|---------------------------|
| binutils/gcc | host ABI + `TARGET_ABI` |
| linux-headers / glibc | target ABI, **no** `TARGET_ABI` |
| newlib (if bare-metal run) | same as glibc (target) |

Also: no dependency on `EM_BUILD_CLASS` being set for correct inject
(shell should use package.env sniff).

#### 2. Bare-metal elf/newlib (short)

```sh
em --prefix "$P2" --target riscv64-unknown-elf crossdev --setup --jobs 8
```

Expect newlib **target** env; not host-codegen PATH/ESYSROOT host-tool specials
(wrong-as-host is the old failure class).

#### 3. LLVM `-L` musl (if time)

```sh
em --prefix "$P3" --target aarch64-unknown-linux-musl -L crossdev --setup --jobs 8
```

Expect:

- `clang-crossdev-wrappers` host env + HostCodegen as needed  
- llvm-runtimes **host** env (`TARGET_ABI` present), not K\|L target env  
- Still installs into sysroot via ebuild/`is_crosspkg`

#### 4. Pretend purity (quick)

```sh
# Fresh empty dir — must NOT create skeleton / register active
em -p --prefix "$EMPTY/never" setup
test ! -d "$EMPTY/never/etc/portage"

# First-time crossdev -p — must NOT require prior init; must not write
# package.env / make.conf (alias may be in-memory only)
em -p --prefix "$EMPTY2" --target riscv64-unknown-linux-gnu crossdev --setup
# Expect: config changes preview + real plan for cross-*/binutils (or clear
# step plans), no full layout under $EMPTY2 except possibly nothing
```

### P2 — Optional / only if P0–P1 green

- Hand-seed `package.provided` under `--local` per
  [`docs/local-bootstrap.md`](../docs/local-bootstrap.md) and try
  `toolchain --setup` (not automated yet).  
- Do **not** treat failure as a regression of fad35a3.

---

## How to report

Append below under **Results**. For each item: SHA, command, EXIT, one-line
verdict, paths to logs, and any new bug (file:line if known).

```text
### Results — YYYY-MM-DD (Sonnet)

**em SHA:** …

#### P0 workdir / clang Scenario A
- …

#### P1 package.env / BuildClass drop
- GCC linux-gnu: …
- bare-metal: …
- LLVM -L: …
- pretend: …

#### New bugs
- …
```

---

## Out of scope for this handoff

- Implementing `package.provided` automation  
- Reintroducing BuildClass  
- Multi-`em` plan registry (future in workdir todo)  
- Fixing dual plan *entries* (isolation should make them safe; dedupe later)  

---

## Results

### Results — 2026-08-07 (Sonnet)

**em SHA:** `f250e62` (tip; code is `fad35a3`, `f250e62` is docs-only)

Stopped after P0 + the pretend-purity half of P1.4 per direction ("enough
bugs to stop here and report") — P1.1 (GCC linux-gnu env spot-check) was
launched (`crossdev --setup` for a fresh `--prefix`) but not inspected;
P1.2/P1.3 (bare-metal, LLVM `-L` musl) not attempted; P2 not attempted.

#### P0 workdir / clang Scenario A

**The workdir dual-root race is fixed. Confirmed via three independent
signals, not just exit code:**

1. New root-keyed workdir paths are real and in use: cross-toolchain host
   packages build under `var/tmp/portage/root-xp/cross-<tuple>/<pf>/`;
   ordinary target packages build under
   `var/tmp/portage/root-xp-usr-riscv64-unknown-linux-gnu/<cat>/<pf>/` — host
   and target instances of the same CPV now get genuinely different paths.
2. `em --prefix "$P" --target riscv64-unknown-linux-gnu crossdev --setup
   --jobs 8` → `EXIT=0`, all 6 stages, real
   `riscv64-unknown-linux-gnu-gcc --version` runs (`16.1.1_p20260718`).
3. `em --prefix "$P" --target riscv64-unknown-linux-gnu -b llvm-core/clang
   --jobs 80` — **no doubled phases anywhere in any build.log**, and none of
   the previously-racing packages (`llvm-runtimes/clang-rtlib-config-22`,
   `llvm-core/clang-linker-config-22`, `llvm-runtimes/clang-stdlib-config-22`)
   failed this time; both `llvm-core/llvm-common` and `llvm-core/clang-common`
   merged cleanly. Progress reached 76/136 (previously stalled at 66/136
   every time, deterministically).

**But two other, independent real bugs now surface** (previously masked —
never reached — by the workdir race):

**New bug #1 — dependency/scheduling race, not a workdir issue.**
`sys-apps/sed-4.10-r1`'s `econf` fails: `checking for sys/acl.h... no` →
`configure: error: ACLs enabled but support not detected`. But
`sys-apps/acl-2.4.0-r2` **did** merge into the same sysroot in this same run
(confirmed: `/root/xp/usr/riscv64-unknown-linux-gnu/usr/include/sys/acl.h`
exists on disk, and the VDB has `acl-2.4.0-r2` installed) — the header is
there, just apparently not yet at the moment sed's `configure` ran. Under
`--jobs 80` this reads as a genuine dependency-ordering/scheduling gap: sed
should not be able to start `configure` before its `acl` dependency is fully
installed into the target sysroot, but it did. Not root-caused further (no
file:line yet — would need to check whether `sys-apps/acl` is actually
encoded as a real DEPEND edge for `sed[acl]` in the resolved plan, or
whether the scheduler's readiness check doesn't cover this class of
same-sysroot ordering). This is what stopped the run (`EXIT=1`, `1 of 136
package(s) failed to merge`) — `llvm-core/clang` itself was **not yet
reached** when it stopped (only 76/136 done), so whether clang would
complete past this point is still open.

**New bug #2 — `--buildpkg` fails systematically for near-empty-image
packages under `--prefix --target`, not just one isolated case.** Every
`virtual/*` package installed into the sysroot in this run failed its
`--buildpkg` step (12 occurrences: `virtual/libintl`, `virtual/libiconv`,
`virtual/acl`, `virtual/libcrypt`, `virtual/os-headers`, four
`virtual/perl-*`, `virtual/zlib`), plus one non-virtual symlink-only package
(`llvm-core/llvm-toolchain-symlinks-22`) — always the same error shape:
`tar: .../<cat>/<pf>/image/root/xp: Cannot open: No such file or directory`
→ `--buildpkg failed for <pkg>: ... tar failed with exit code 2`. Reported
as a non-fatal warning (the merge itself still succeeds, package still
counts as installed), but no `.gpkg` is ever written for any of these.
Pattern strongly suggests these are all packages whose merge image is empty
or near-empty (virtuals typically install no real files of their own;
`*-toolchain-symlinks` installs only symlinks) — the `image` dir for one
checked (`virtual/libintl-0-r2`) no longer exists (already cleaned up
post-merge, so emptiness at tar-time couldn't be directly re-confirmed, but
no package with real installed content hit this in the same run).

**This is not a new regression from `56435d4`/`480daff`/`fad35a3`** — it
already reproduced, once, on `em` built from `cd9e0df` (pre-BuildClass-
refactor) for `llvm-runtimes/clang-stdlib-config-22`'s host-side instance,
in a prior session (see the clang test plan's Execution log). What's new
here: confirmed on current tip, confirmed **not** limited to host-arch
dual-role packages (this run's occurrences are almost all target-sysroot
installs), and confirmed systemic (12+ packages in one run, not a one-off) —
directly relevant to "-b does the right thing", the actual goal of the
underlying clang test plan, since it means `-b` silently produces no binpkg
for an entire class of packages whenever they're part of the plan.

---

### Results — 2026-08-07 (Sonnet), confirming `a46027b`

**em SHA:** `a46027b` ("fix: high-jobs virtual RDEPEND race; empty-ED
--buildpkg tar"). Fresh sandbox (`em-a46027b-verify`, `sandbox prepare
--bare`), same P0 scenario: `em --prefix /root/xp setup` →
`em --prefix /root/xp --target riscv64-unknown-linux-gnu crossdev --setup
--jobs 8` (EXIT=0, real gcc) → `em --prefix /root/xp --target
riscv64-unknown-linux-gnu -b llvm-core/clang --jobs 80`.

**Both bugs #1 and #2 are fixed, confirmed directly, not just by absence of
the old symptom:**

- **Bug #1 (sed/acl RDEPEND scheduling race): fixed.** `sys-apps/sed-4.10-r1`
  built with a single `Emerging`/`Completed` pair, no `configure: error:
  ACLs enabled but support not detected`, no retry needed.
- **Bug #2 (`--buildpkg` empty-ED tar failure): fixed.** Zero `tar failed`/
  `Cannot open` messages anywhere in the log (previously 12+ occurrences).
  Directly verified real `.gpkg.tar` files exist for every one of the
  previously-failing packages: `find /root/xp -name '*.gpkg.tar'` → 66
  files, including `virtual/libintl-0-r2-1.gpkg.tar`,
  `virtual/libiconv-0-r2-1.gpkg.tar`, `virtual/libcrypt-2-r1-1.gpkg.tar`,
  `virtual/os-headers-0-r2-1.gpkg.tar`, `virtual/zlib-1.3.1-r1-1.gpkg.tar`,
  `virtual/acl-0-r2-1.gpkg.tar` — 66 packages completed this run, 66 real
  binpkgs written, 1:1.
- Workdir dual-root fix (`56435d4`) still holds: no duplicate `Emerging (N
  of 136)` index, no duplicate CPV in the emerge order — checked
  programmatically, zero dupes either way.

**Progress: 66/136 (previous best was 76/136, but that run died on the
sed/acl race before reaching this point in the graph — different failure
axis, not a regression; this run got further on some branches and less far
on others before its own new blocker below).**

**Run stopped (`EXIT=1`, correctly *not* using `--keep-going`) on
`sys-libs/pam-1.7.2`: `configure: Run-time dependency libcrypt found: NO`,
`Run-time dependency libxcrypt found: NO`.** Root-caused:

#### New bug #3 — a package can report "Completed" while its resolved runtime provider never gets scheduled at all

`virtual/libcrypt-2-r1` was planned to pull in `sys-libs/libxcrypt-4.5.2`
(both appear in the initial plan dump, `[ebuild N] sys-libs/libxcrypt-4.5.2
... to /root/xp/usr/.../`). `virtual/libcrypt` reports `>>> Completed (33 of
136)` early in the run — but `sys-libs/libxcrypt` **never gets a single
`Emerging` line anywhere in the 136-package run**, and is confirmed absent
after the fact: no `var/db/pkg/sys-libs/libxcrypt-*` VDB entry, no
`usr/lib64/libcrypt.so*`, no `usr/include/crypt.h` on disk in the sysroot.
This starves `sys-libs/pam` (a real RDEPEND consumer) of its crypt library
much later in the run, producing a confusing downstream `meson.build:257`
failure that doesn't point back to the real cause at all. This looks like
the same *class* of bug `a46027b` fixed (RDEPEND edges through virtuals not
tracked as blockers) but not the same *instance* — `virtual/libcrypt →
sys-libs/libxcrypt` specifically is still not correctly wired, and unlike
the sed/acl case this isn't even a race (libxcrypt isn't merely late, it's
never scheduled at all in this run). Not root-caused to file:line — next
step would be checking whether `sys-libs/libxcrypt` is even correctly
resolved as `virtual/libcrypt`'s chosen provider in the plan, or silently
dropped somewhere between plan construction and the scheduler's ready queue.
Worth checking `portage-cli/src/query/depgraph/mod.rs` (the same file
`a46027b` touched for the RDEPEND fix) for whether virtual-provider edges
are handled differently from ordinary RDEPEND edges.

#### New bug #4 — crossdev's LLVM/clang bootstrap path never seeds baselayout, so a merged-usr profile ends up genuinely split-usr on disk

Confirmed by direct inspection: `/root/xp/usr/riscv64-unknown-linux-gnu/bin`
and `.../usr/bin` are both real, separate, non-symlinked directories with
*different* content (`bin` has `sed`/`tar`/`attr`/`acl` tools written by
packages that install straight to `/bin`; `usr/bin` has `binutils-config`
and friends) — a genuine split-usr layout, even though the profile
(`default/linux/riscv/23.0/...`) declares merged-usr and every affected
package shows `(-split-usr)` in its USE string. Root cause (via
Explore-agent code read, not yet independently re-verified by me):
`portage-cli/src/crossdev/stages.rs:165-224` (`toolchain_plan`) — the
`kind.llvm()` branch returns early at line 201 (`clang wrappers → kernel
headers → libc → runtimes`) and never reaches the baselayout-seeding block
at lines 212-223, which is gated to `Native || self_contained` and lives
only in the non-LLVM (GCC) branch's control flow. So `libc` (glibc,
`stages.rs:187-192`, run during the earlier `crossdev --setup` step, before
this scenario's main `-b llvm-core/clang` invocation) writes real content
into `lib64` deterministically before `sys-apps/baselayout` ever gets a
chance to run against that ROOT — not a race, a structural ordering gap
specific to the LLVM cross-bootstrap path. The doc comment at
`stages.rs:204-211` explains exactly why a fresh ROOT needs baselayout's
skeleton first; that reasoning was apparently never extended to the LLVM
branch.

#### New bug #5 — `pkg_setup`'s profile-`bashrc` die is silently swallowed, so packages "complete" despite failing their own sanity check

Directly downstream of bug #4, and independently a real correctness bug on
its own: 27 separate `die: ERROR: 23.0 merged-usr profile, but disk is
split-usr` lines appear in the log (from `profiles/releases/23.0/profile.bashrc`,
which every package sources during `pkg_setup` and which correctly detects
the split-usr state bug #4 caused) — yet only **one** package
(`sys-libs/pam`, for the unrelated libcrypt reason above) ends up in the
final failed-to-merge list. `sys-devel/gcc-config`, `sys-devel/binutils-config`,
`sys-apps/acl`, `sys-libs/binutils-libs`, `app-alternatives/bzip2` all die
in this check once or twice each, then go on to fetch/configure/install and
report `>>> Completed` in the same run. Root-caused precisely (via
Explore-agent code read):
`portage-repo/src/build/shell.rs`, inside `run_phase`:
- lines 2099-2111: profile `bashrc` hooks (including `profile.bashrc`) are
  sourced via `self.run_string(&script).await.ok()` — both the die flag
  this sets *and* any hard shell error from the hook itself are ignored at
  this point (`.ok()` discards the `Result`).
- **line 2117: `self.die_flag.take()`** unconditionally clears whatever the
  bashrc hooks just set, before the real phase function body runs (line
  2171) and before the only die-flag check in the function (line 2178,
  which now sees an empty flag).
- Confirmed via `portage-cli/src/ebuild.rs:1257-1298`/`1799-1802`: the
  phase-chain loop only sees `run_phase`'s `Ok(())`, so
  `src_unpack`/`src_configure`/`src_install` all proceed normally — matches
  the observed behavior exactly (single EAPI/phase semantics bug, not
  scheduler-related).

**Fix shape (not yet implemented):** the die raised while sourcing the
profile `bashrc` hooks (`shell.rs:2099-2111`) needs to be checked and
propagated *before* `self.die_flag.take()` at line 2117 resets the slate for
the phase function proper — either check-and-return right after the hook
`run_string` call, or don't discard its `Result`/die-flag until after that
check. This is a general EAPI-phase bug (any profile/eclass `bashrc` hook
that calls `die` during `pkg_setup` is currently silently ignored), not
specific to merged-usr or to crossdev — worth flagging as higher-priority
than bug #4 itself, since bug #4 (crossdev not seeding baselayout) is
plausibly acceptable/fixable on its own terms, but bug #5 means *any*
`pkg_setup`-time bashrc sanity check in the whole `::gentoo` tree is
currently a no-op in `em`.

**Minor, not investigated further:** `dev-lang/perl-5.44.0`'s postinst
elog reports `Unable to establish //root/xp//usr/bin/ptar symlink` (and
~18 siblings) — note the double slash and that the path targets the
*outer* prefix (`/root/xp/usr/bin`) rather than the target sysroot
(`/root/xp/usr/riscv64-unknown-linux-gnu/usr/bin`) it was actually merging
into. Non-fatal (elog warning only), but suggests an EPREFIX/EROOT
path-join issue for at least one postinst code path under `--target`. Not
root-caused; flagging for whoever picks this up next.

**`llvm-core/clang` itself was still not reached** (66/136, stopped before
getting there) — this remains open. The immediate next blocker for another
pass would be either bug #3 (libcrypt) or bug #4/#5 (split-usr), whichever
is fixed first; fixing #5 alone would surface whether other `pkg_setup`
bashrc checks across the 136-package graph are also currently silent
no-ops.

#### P1 package.env / BuildClass drop

- GCC linux-gnu: **not inspected** — `crossdev --setup` was launched on a
  fresh `--prefix` sandbox but the run was not followed through to a
  package.env spot-check before stopping per direction.
- bare-metal: not attempted.
- LLVM -L: not attempted.
- pretend: **both checks pass.**
  - `em -p --prefix /root/never setup` → prints a labeled preview
    (`>>> would bootstrap layout…`, `(pretend — no files written)`),
    `/root/never` does not exist afterward. Confirms finding #1 (`em setup
    -p` used to write for real) is fixed.
  - `em -p --prefix /root/never2 --target riscv64-unknown-linux-gnu crossdev
    --setup` on a **never-before-initialized** target → resolves and prints
    the full real 6-step plan (binutils → headers → glibc-headers →
    gcc-stage1 → glibc → gcc-stage2) correctly, `RC=0`, and `/root/never2`
    does not exist afterward — genuinely zero disk writes. Confirms finding
    #2 (`crossdev --setup -p` used to hard-fail on a first-time target with
    `no ebuilds in ::gentoo or overlays`) is fixed.

#### New bugs (status after Grok follow-ups)

| # | Issue | Status |
|---|--------|--------|
| 1 | sed/acl RDEPEND scheduling race | ✅ fixed `a46027b` (live confirmed) |
| 2 | empty-ED `--buildpkg` tar | ✅ fixed `a46027b` (live confirmed) |
| 3 | virtual/libcrypt Completed, libxcrypt never scheduled | 🟡 open — needs live re-verify after #4/#5; suspect USE/`prefix-guest` or silent skip, not only blockers |
| 4 | no baselayout → genuine split-usr under merged-usr | ✅ fixed: seed baselayout for **all** `toolchain_plan` (incl. default cross + LLVM early path) |
| 5 | profile bashrc `die` swallowed | ✅ fixed: check `die_flag` after bashrc, before phase body |

**Bug #5 root cause:** `run_phase` ran `die_flag.take()` *after* sourcing
bashrc, discarding profile.bashrc dies. Fix: take before bashrc; if die after
hooks → `Err`.

**Bug #4 root cause:** baselayout only for `Native \|\| self_contained`, and
LLVM returned before that block. Default `--prefix --target` cross never
seeded sysroot baselayout; packages wrote real `/bin` vs `/usr/bin`.

**Why pam is in the clang plan (not a bug):**  
`llvm-core/clang` → `${PYTHON_DEPS}` → `dev-lang/python` → unconditional
`sys-apps/util-linux` → profile `USE=pam` → `sys-libs/pam` →
`virtual/libcrypt` / libxcrypt. Empty sysroot ⇒ all of that is planned.

---

### Results — YYYY-MM-DD (Sonnet), `f8ac293` re-verify

**em SHA:** …

#### crossdev --setup (baselayout / #4)
- …

#### clang -b --jobs 80
- Progress: N/M …
- #5 bashrc die behaviour: …
- #3 libxcrypt: …
- clang reached?: …

#### New bugs
- …
