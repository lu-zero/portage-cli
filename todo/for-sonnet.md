# For Sonnet — live verification handoff

**Current pin (do this next):** `master` at **`1ac8067`**  
(`fix(crossdev): baselayout into target sysroot for merged-usr profiles`)

Build `em` from that SHA (or tip if only docs after). Note the SHA in Results.
Unit tests green; they do **not** replace live sandbox checks.

---

## NEXT HOMEWORK — `1ac8067` (Sonnet)

### What landed since your last full pass

| SHA | What |
|-----|------|
| `f8ac293` | #5 bashrc die propagates; baselayout first in plan (**outer only** — incomplete for #4) |
| `1ac8067` | **`StageStep::into_sysroot`**: cross baselayout merges under **`--target` sysroot**. Keeps **default merged-usr** profile |

### Product decisions (do not re-open)

1. **#4 layout:** make **disk match the default merged-usr profile**.  
   **Rejected:** switch sysroot `make.profile` to `…/split-usr/…` so the die goes quiet.  
2. Full **@system/stage1** not required for pure cross-build *theory*; library DEPEND identity remains open. Stage1 optional for plan-size comparison only.  
3. **No `--keep-going`.** Fresh sandbox only. Record + stop; no large redesigns unless a one-line regression of `1ac8067`.

### Goal

1. **Confirm #4:** after `crossdev --setup` alone, **target sysroot** is merged-usr on disk.  
2. **Confirm #5** still honest (die → fail, not Completed).  
3. **Clang stress** past 42/136 if #4 holds; #3 evidence if still broken.  
4. **Library identity** probe (`-p zlib`, outer vs sysroot VDB).

### Commands

```sh
git -C /path/to/portage-cli rev-parse --short=8 HEAD   # expect 1ac8067 or docs-only tip
# build em from that tree

P=/root/xp   # FRESH bare sandbox — never reuse prior xp trees
em --prefix "$P" setup

# --- 1) Toolchain ---
em --prefix "$P" --target riscv64-unknown-linux-gnu crossdev --setup --jobs 8
# Expect EXIT=0, real riscv64-unknown-linux-gnu-gcc
# Log: baselayout should merge *to the sysroot* (…/usr/riscv64-…/), not only "to $P/"

SYSROOT="$P/usr/riscv64-unknown-linux-gnu"

# --- #4 PASS gate (immediately after setup, before clang) ---
ls -la "$SYSROOT/bin" "$SYSROOT/usr/bin" | head -20
# PASS: bin is a symlink into usr (merged-usr)
# FAIL: bin and usr/bin both real dirs with different content
readlink -f "$SYSROOT/etc/portage/make.profile"
# Expect: default path WITHOUT "split-usr" in the path (merged-usr profile kept)

echo "=== outer cross-* VDB ==="
ls "$P/var/db/pkg/cross-riscv64-unknown-linux-gnu/" 2>/dev/null | head
echo "=== sysroot VDB ==="
find "$SYSROOT/var/db/pkg" -maxdepth 2 -type d 2>/dev/null | sort | head -40

# --- 2) Library DEPEND probe ---
em -p --prefix "$P" --target riscv64-unknown-linux-gnu sys-libs/zlib | tee /tmp/zlib-p.txt
# Note: does [ebuild N] sys-libs/glibc appear despite cross-T/glibc files on disk?

# --- 3) Clang stress ---
em --prefix "$P" --target riscv64-unknown-linux-gnu \
  -b llvm-core/clang --jobs 80
```

### Pass / fail checklist

| Check | Pass if |
|-------|---------|
| **#4 sysroot layout** | `$SYSROOT/bin` is a **symlink** (merged), not a real dir next to divergent `usr/bin`. Profile path stays **non–split-usr** default |
| **#4 baselayout root** | Setup log: baselayout **to sysroot** (`…/usr/riscv64-…/`), not only outer `$P/` |
| **#5 die honesty** | No “die then Completed”; failures appear in the failed-merge list |
| **#1/#2** | No sed/acl race if sed runs; virtuals still get `.gpkg` under `-b` |
| **#3** | If still broken: virtual Completes vs `sys-libs/glibc`/`libxcrypt` never `Emerging` — note plan lines + counts (you already root-caused the virtual-edge filter; do **not** implement it this pass) |
| **Clang** | N/M; clang reached? |
| **Workdir** | No dual-phase / dual WORKDIR for same CPV host+target |

### If #3 still fails — evidence only

```sh
rg -n 'Emerging.*(libxcrypt|sys-libs/glibc)|Completed.*libcrypt' "$LOG" | head -40
rg -n 'sys-libs/glibc|sys-libs/libxcrypt|virtual/libcrypt' "$LOG" | head -40
```

### Out of scope

- Changing profile to `…/split-usr/…`  
- Implementing the virtual-edge filter fix (record only)  
- package.provided / --local bootstrap  
- BuildClass, multi-em registry  

### Results template

```text
### Results — YYYY-MM-DD (Sonnet), 1ac8067

**em SHA:** …

#### crossdev --setup / #4
- baselayout merge root (log): …
- ls -la $SYSROOT/bin: …
- make.profile: …
- outer vs sysroot VDB: …

#### -p zlib (library identity)
- …

#### clang -b --jobs 80
- Progress: N/M …
- #5: …
- #3: …
- clang reached?: …

#### New bugs
- …
```

---

## Prior homework / results (historical)

Older pins: `f8ac293`, `a46027b`, `fad35a3`. Full Results sections follow.


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

### Results — 2026-08-07 (Sonnet), `f8ac293` re-verify

**em SHA:** `f8ac293` (tip at time of build; `30aa845` on top is docs-only).
Fresh sandbox (`em-f8ac293-verify`, `sandbox prepare --bare`), same P0
sequence as before.

#### crossdev --setup (baselayout / #4)

- **Ordering fix confirmed at the outer-prefix level.** Setup log shows
  `[1/7] baselayout` before `[4/7] libc headers`/`[6/7] libc` — matches the
  commit's intent exactly. `EXIT=0`, real
  `riscv64-unknown-linux-gnu-gcc --version` works.
- **But this baselayout step never touches the target sysroot, and the
  underlying split-usr disk state is unchanged.** The `baselayout`
  `StageStep` in `toolchain_plan` (`portage-cli/src/crossdev/stages.rs:183-188`)
  uses a bare `"sys-apps/baselayout"` atom, explicitly *not* passed through
  `atom()`'s `cross-<tuple>` rewrite (per the comment at line 179:
  "baselayout is never part of the `cross-<tuple>` package set — bypass
  `atom()`'s rewrite"). That means it always resolves against the
  **host/outer** root (`to /root/xp/` in the log), never the **target**
  sysroot (`/root/xp/usr/riscv64-unknown-linux-gnu/`). Meanwhile `libc`
  (line 207-212 in the LLVM branch) *does* go through `atom()` and installs
  straight into the target sysroot, during this same `crossdev --setup`
  step — writing real content into `lib64`/`sbin` there before any
  baselayout for that specific root has ever run (the target sysroot's own
  baselayout is only reached later, as package #7 of the main 136-package
  plan). Directly verified right after `crossdev --setup` finished:
  `/root/xp/usr/riscv64-unknown-linux-gnu/{lib64,sbin}` are real,
  non-symlinked directories with real glibc content already in them, and
  `usr/{lib64,sbin}` are separate real directories — genuine split-usr,
  unchanged from the `a46027b` run.

#### clang -b --jobs 80

- **Progress: 40 ok, 2 failed, 42/136** (lower than `a46027b`'s 66/136 —
  see below, this is expected/correct, not a regression).
- **#5 bashrc die behaviour: confirmed fixed.** `sys-devel/binutils-config`
  and `sys-devel/gcc-config` both now **fail for real** —
  `phase setup failed: shell error: bashrc (before pkg_setup): die: ERROR:
  23.0 merged-usr profile, but disk is split-usr` — and correctly appear in
  the final `2 package(s) failed to merge` summary. No more "die then
  Completed anyway": the run legitimately stops scheduling new work after
  this (no `--keep-going` used), which is why progress is *lower* this pass
  than the previous one — `a46027b`'s run silently limped to 66/136 past
  the same underlying split-usr state; this run correctly halts at 42/136
  once it hits real, honest failures. Net: #5 is a genuine fix, its lower
  headline number is the fix working as intended, not a regression.
- **#3 libxcrypt: unchanged, reproduces identically.** `virtual/libcrypt-2-r1`
  → `Completed (33 of 136)`; `sys-libs/libxcrypt-4.5.2` never gets a single
  `Emerging` line in the whole run. Confirmed post-run:
  `var/db/pkg/sys-libs/` contains only `ncurses`/`zlib`, no `libxcrypt`
  entry; `usr/include/crypt.h` does not exist. Same evidence shape as the
  `a46027b` pass — not yet investigated further per the homework's "you
  only need evidence" instruction.
- **#1 (sed/acl race): not re-reached this pass** — run stopped at 42/136,
  before `sys-apps/sed` (planned but never got an `Emerging` line either).
  Can't confirm or deny this pass; no evidence of regression, just not
  exercised.
- **#2 (buildpkg tar for virtuals): still fixed.** `find /root/xp -name
  '*.gpkg.tar'` → 40 files for 40 ok packages, 1:1, zero tar errors in the
  log.
- **Workdir dual-root fix: still holds.** No duplicate `Emerging (N of
  136)` index, no duplicate CPV in the emerge order.
- **`llvm-core/clang` itself: not reached** (same as every prior pass).

#### New bugs

None beyond what's already tracked. This pass narrows bug #4 precisely:
it's not a general "baselayout runs too late" problem anymore (ordering is
correct now, confirmed via the setup log), it's that **`toolchain_plan`'s
baselayout step only ever seeds the outer/host root, never the target
sysroot that `libc` (and, in the non-LLVM branch, other `atom()`-rewritten
steps) install into.** Fix shape: the `baselayout` `StageStep` needs a
second instance (or a root-aware variant) that *does* go through the
target-sysroot resolution — i.e. install `sys-apps/baselayout` into the
same `EROOT` that `libc`/`kernel headers` are about to write into, before
they run, for both the LLVM and non-LLVM branches. Whether that's "rewrite
baselayout through `atom()` after all" or a distinct explicit sysroot-target
step is a design call for whoever picks this up — flagging the shape, not
prescribing the fix per the handoff rules.

---

### Results — 2026-08-07 (Sonnet), blank-slate probe (library identity hypothesis)

**em SHA:** `f8ac293`-based tip (`f88d091` at time of this run; docs-only on
top). Fresh sandbox (`em-stage1-ladder-verify`), no stage1 step run —
followed the revised (library-identity) homework, not the earlier
stage1-required version.

**Directly answers the "is split-usr caused by skipping stage1" question:
no.** Checked the disk layout immediately after `crossdev --setup` alone,
on a completely blank sandbox, before anything else ever touched the tree:
`usr/riscv64-unknown-linux-gnu/bin` does not exist yet, `usr/bin` already
has real glibc-utility content, and `lib64`/`usr/lib64` are both real,
separate, non-symlinked directories. The split-usr state is present at the
earliest possible observation point — it cannot be a consequence of
skipping a later stage1 step, since no later step has run yet.

**Library-identity hypothesis (Grok's #1): confirmed directly, with a
sharper finding than expected.**

- VDB check right after `crossdev --setup`: `find
  $P/usr/riscv64-unknown-linux-gnu/var/db/pkg -maxdepth 2` returns
  **nothing** — the target sysroot's own VDB is completely empty. The
  installed `cross-riscv64-unknown-linux-gnu/glibc-2.43-r2` (and
  binutils/gcc/linux-headers) is recorded under `$P/var/db/pkg/cross-
  riscv64-unknown-linux-gnu/`, the **outer prefix's** VDB — even though its
  files are physically written into the target sysroot path. So anything
  resolving dependencies *for the target sysroot* sees zero packages
  installed there, regardless of what's really on disk.
- `em -p --prefix "$P" --target riscv64-unknown-linux-gnu virtual/libcrypt
  sys-libs/pam` on this exact state: plans a brand-new `sys-libs/glibc-
  2.43-r2` (`[ebuild N]`) install, plus `sys-libs/libxcrypt-4.5.2`, then
  `virtual/libcrypt-2-r1` — confirms the resolver doesn't credit the
  already-installed `cross-*/glibc` as satisfying `sys-libs/glibc` for
  ordinary target-sysroot packages, exactly per Grok's hypothesis.
- **New, sharper finding: it's not just `libxcrypt` that never gets
  dequeued — `sys-libs/glibc` doesn't either.** Re-checked both this run's
  full clang log *and* the two earlier full runs
  (`a46027b`/`f8ac293`-without-blank-slate): `sys-libs/glibc-2.43-r2` is
  listed in the plan (`[ebuild N] sys-libs/glibc-2.43-r2 ...`, identical
  line in all three logs) but **never once gets an `Emerging` line** in
  any of them — the exact same "planned but never scheduled" pattern as
  `sys-libs/libxcrypt`. Meanwhile `virtual/libcrypt-2-r1` itself schedules
  fine and reports `Completed` (package #33 in the emerge order, well
  before glibc/libxcrypt would need to run) in all three runs. This
  reframes bug #3: it isn't specifically a libxcrypt problem, it's that
  **whatever real-Cpn packages sit behind `virtual/libcrypt`'s RDEPEND
  resolution (both `sys-libs/glibc` and `sys-libs/libxcrypt`) are
  consistently excluded from scheduling, while the virtual that supposedly
  depends on them merges anyway** — worth checking whether
  `build_blockers`/the ready-queue logic treats a virtual's RDEPEND
  edge as satisfied without ever actually gating on the provider package's
  completion, for *any* provider, not specifically a libxcrypt corner case.

**Run outcome: 40 ok, 2 failed (`binutils-config`, `gcc-config`, same
merged-usr die as the previous `f8ac293` pass), 42/136 — byte-identical
outcome to the earlier (non-blank-slate-verified) `f8ac293` run.**
Deterministic, not timing noise. #1/#2/workdir all still hold (same checks
as the previous pass, not re-run in full detail here since the outcome was
identical).

**Root cause found for half of this (why `virtual/libcrypt` doesn't wait on
its provider) — confirmed by direct code read + a live disproof of the
alternate hypothesis:**

- **Confirmed by disk check:** `find $sysroot/var/db/pkg -iname
  '*glibc*' -o -iname '*libxcrypt*'` on this exact sandbox → **nothing at
  all**, not even a stale/partial directory. This rules out "silently
  skipped because the merge-root already-installed check
  (`portage-cli/src/merge/mod.rs:1173-1186`) sees a stale VDB dir" — there
  is no VDB dir, stale or otherwise, for either package. So it's not a
  false already-installed skip; `sys-libs/glibc`/`sys-libs/libxcrypt` are
  never attempted at all, from a genuinely clean start.
- **Confirmed by code read:** `portage-cli/src/query/depgraph/mod.rs:988-992`
  filters the dependency-edge list *before* `build_blockers` is computed
  from it (`mod.rs:1566` on):
  ```rust
  let edges: Vec<_> = provider
      .dependency_graph(&solution)
      .into_iter()
      .filter(|e| !e.from.0.is_virtual() && !e.to.0.is_virtual())
      .collect();
  ```
  This drops **every** edge where either endpoint is virtual — including
  `virtual/libcrypt → sys-libs/glibc`, which is *exactly* the edge shape
  `a46027b`'s own fix (`sed[acl] → virtual/acl → sys-apps/acl`) was meant
  to preserve. The filter runs first and silently undoes the fix for this
  specific case: `build_blockers[virtual/libcrypt]` ends up empty, so
  `virtual/libcrypt` is ready from `Scheduler::new`
  (`merge/mod.rs:1056-1058`) and can complete before its provider ever
  starts — matching the observed behavior exactly.
- **Still open:** the filter explains why `virtual/libcrypt` doesn't
  *wait*, but not why `sys-libs/glibc`/`sys-libs/libxcrypt` never even get
  *attempted* — dropping an edge should only make a node ready *earlier*,
  never permanently stuck, and `build_blockers` is structurally acyclic
  (`to < from` gate, `mod.rs:1578`), ruling out a scheduler deadlock. Given
  the confirmed-clean VDB, the remaining candidates are: (a) a real file
  collision/blocker against the physically-already-installed
  `cross-riscv64-unknown-linux-gnu/glibc` at the same on-disk path (different
  Cpn, same files) that never resolves, or (b) some other readiness
  precondition for these two specific packages that's never satisfied.
  Fix shape for the confirmed half: the `mod.rs:988-992` filter needs to
  stop dropping edges where exactly one endpoint is virtual (only drop
  virtual-to-virtual, or rethink the filter's purpose entirely) — but
  don't apply that fix blind; it may have been added intentionally for a
  different reason and could have its own blast radius, worth checking
  its own history/tests before touching it.

---

### Bug #4 product decision (Luca, 2026-08-07): **reject** split-usr profile switch

A proposal to fix #4 by linking
`default/linux/…/23.0/…/split-usr/…` for crossdev sysroots (so the profile
matches the accidental split disk layout) is **unacceptable**.

**Rule:** keep the **default merged-usr** profile the target arch already
selects (`profile_path()`). **Start with the layout that profile needs**
(baselayout into the **target sysroot** before libc writes real dirs) — do
not change the default profile to paper over missing layout.

**Code direction (landed / in progress):** `StageStep::into_sysroot` so
cross `toolchain_plan` baselayout merges under `--target` even when
`crossdev --setup` uses plan-wide `use_outer_eroot: true` for host-arch
`cross-*` tools. Verify live: after setup,
`$P/usr/<tuple>/bin` is a symlink (merged), not a second real tree.

Profile-path mechanics of `profile.bashrc` (string contains `split-usr` vs
disk `/bin` symlink) remain correct diagnostics when layout and profile
disagree; the fix is layout, not re-picking the profile.
