# For Sonnet — live verification handoff (2026-08-07)

**Pin:** `master` at **`fad35a3`** (or tip if only docs landed after).  
**Do not** re-open design of Drop BuildClass or workdir dual-root without new
evidence; this is **live verify + record findings**.

Unit tests already green. Unit tests do **not** catch silent host/target env
or workdir races under real parallelism.

---

## Context (what Grok landed)

| Commit | What |
|--------|------|
| `56435d4` | Per-root workdirs (`work_base/<root-key>/<cat>/<pf>`), builddir flock, parallel schedule barrier; `setup -p` no writes; crossdev `-p` first-time plan |
| `480daff` | Crossdev `-p` injects aliases **in-memory** (no disk write) |
| `fad35a3` | **Drop BuildClass**: package.env letter-faithful (llvm R/U/A/P = host); HostCodegen PN allowlist; no `EM_BUILD_CLASS` stamp |

Plans / matrices:

- [[drop-buildclass]] Step 5 live table  
- [[workdir-dual-root]] near-term (code landed; re-verify Scenario A)  
- [[local-bootstrap-provided]] still open (do **not** block on this unless
  you have spare time for hand-written package.provided)  
- [[clang-crossbuild-prefix-local-test-plan]] prior blocked state  

Matrix reference: [`docs/bash-crossdev-matrix.md`](../docs/bash-crossdev-matrix.md)

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

## Priority queue

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

#### New bugs

- **Dependency/scheduling race**: `sys-apps/sed`'s `econf` can run before
  `sys-apps/acl` (a real DEPEND) has finished installing into the same
  target sysroot, under `--jobs 80` — `checking for sys/acl.h... no` despite
  the header genuinely existing on disk once the run finishes. No file:line
  yet; needs someone to check the scheduler's readiness/dependency-edge
  logic for same-sysroot ordering under high `--jobs`.
- **`--buildpkg` produces no binpkg for near-empty-image packages**
  (`virtual/*`, `*-toolchain-symlinks`) under `--prefix --target` — 12+
  occurrences in one run, silent (non-fatal warning only), confirmed
  pre-existing (not caused by this session's landed commits) but not
  previously known to be this systemic. `tar: .../image/<merge_root>:
  Cannot open` — looks like the buildpkg image-tar path assumes a full
  nested `image/<merge_root>/...` copy exists, which an empty-content
  package's image never has. No file:line yet (image dirs are cleaned up
  post-merge, so this needs catching mid-run or reproducing with
  `--keepwork`/similar to inspect the actual image layout before it's
  removed).
