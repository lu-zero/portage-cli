# For Sonnet — live verification handoff

**Current pin (do this next):** `master` tip after **soft-order #3 fix**  
(`fix(solver): repair soft RDEPEND order after soft-cycle walk` — B+C)

Build `em` from that SHA (or tip if only docs after). Note the SHA in Results.
Unit tests green; they do **not** replace live sandbox checks.

---

## NEXT HOMEWORK — soft-order #3 fix (Sonnet)

### What landed (read this; do not re-blame the old root cause)

| SHA / topic | What |
|-------------|------|
| `1ac8067` | #4 baselayout into **target sysroot** (merged-usr) — **confirmed PASS** last pass |
| `bf35c79` | `is_virtual` = solver-internal only — **not** Gentoo `virtual/*`. Edge filter at depgraph is **not** why libxcrypt never Emerged |
| **this tip** | **`install_order` B+C** in `portage-atom-pubgrub`: soft-ready pick + pass-2 acyclic soft-edge repair. Unit fixture: libxcrypt before `virtual/libcrypt` through the glibc soft/hard cycle |

**Correct #3 root cause (Grok, 2026-08-07):**  
`order_cycle` hard-first linearisation inside a soft SCC put empty `virtual/libcrypt` **before** `sys-libs/libxcrypt`. `build_blockers` only keeps edges with `to < from`, so the virtual did not wait; pam started and failed; `stop_new` prevented late glibc/libxcrypt from ever `Emerging`.

**Not the bug:** `is_virtual` edge filter, silent VDB skip, “libxcrypt missing from plan” (it was in the plan, late).

### Product decisions (do not re-open)

1. **#4 layout:** disk matches **default merged-usr** profile. No `split-usr` profile switch.  
2. Full **@system/stage1** not required for pure cross theory; library DEPEND identity (cross-T vs real Cpn) remains a **separate** open item.  
3. **No `--keep-going`.** Fresh sandbox only. **Record + stop** — no large redesigns. One-line regressions of the soft-order fix only if you can prove them.

### Goal

1. **Primary — #3:** after this fix, plan order and schedule put **provider before empty virtual before pam**, and pam should see libcrypt **or** fail for a *new* reason (not “libxcrypt never Emerging”).  
2. **Regression smoke:** #4/#5/#1/#2 still hold.  
3. **How far does clang get?** N/M, first real failure if any.  
4. **Library identity** note only (`-p` / VDB) — do not fix Favor/provided this pass.

### Commands

```sh
git -C /path/to/portage-cli rev-parse --short=8 HEAD
# build em from that tree

P=/root/xp   # FRESH bare sandbox — never reuse prior xp trees
em --prefix "$P" setup

# --- 1) Toolchain (expect #4 still green) ---
em --prefix "$P" --target riscv64-unknown-linux-gnu crossdev --setup --jobs 8
SYSROOT="$P/usr/riscv64-unknown-linux-gnu"
ls -la "$SYSROOT/bin"   # expect symlink → usr/bin (merged-usr)

# --- 2) Plan-order probe (cheap, before full clang) ---
em -p --prefix "$P" --target riscv64-unknown-linux-gnu \
  virtual/libcrypt sys-libs/pam 2>&1 | tee /tmp/libcrypt-pam-p.txt
# In the printed plan order, expect roughly:
#   … sys-libs/libxcrypt …  before  virtual/libcrypt  before  sys-libs/pam
# (glibc may appear before libxcrypt if planned — fine)
rg -n 'sys-libs/libxcrypt|virtual/libcrypt|sys-libs/pam|sys-libs/glibc' /tmp/libcrypt-pam-p.txt

# --- 3) Clang stress ---
em --prefix "$P" --target riscv64-unknown-linux-gnu \
  -b llvm-core/clang --jobs 80 2>&1 | tee /tmp/clang-softorder.log
```

### Pass / fail checklist

| Check | Pass if |
|-------|---------|
| **#3 plan order** | In `-p` and in clang plan dump: `sys-libs/libxcrypt` index **&lt;** `virtual/libcrypt` **&lt;** `sys-libs/pam` (same MergeRoot/sysroot) |
| **#3 schedule** | `>>> Emerging` for `sys-libs/libxcrypt` (and likely `sys-libs/glibc` if still planned) **before** pam fails or completes |
| **#3 outcome** | Either pam **configures** (libcrypt found in sysroot) **or** a **new** failure past libcrypt — not the old “virtual Completed, libxcrypt never Emerging” |
| **#4** | `$SYSROOT/bin` still symlink; profile not split-usr |
| **#5** | No die-then-Completed |
| **#1/#2** | No sed/acl race if sed runs; virtuals still get `.gpkg` under `-b` |
| **Clang** | N ok / M total; `llvm-core/clang` reached? first failure package + phase |
| **Workdir** | No duplicate Emerging index for same CPV host+target |

### If #3 still fails — evidence only (do not re-implement)

```sh
LOG=/tmp/clang-softorder.log
# Plan positions
rg -n '\[ebuild.*(libxcrypt|libcrypt|glibc|pam)' "$LOG" | head -30
# Schedule
rg -n 'Emerging.*(libxcrypt|glibc|libcrypt|pam)|Completed.*(libxcrypt|glibc|libcrypt|pam)|Failed.*pam' "$LOG" | head -40
# Sysroot after stop
ls "$SYSROOT/var/db/pkg/sys-libs/" 2>/dev/null
ls "$SYSROOT/usr/include/crypt.h" "$SYSROOT/include/crypt.h" 2>/dev/null
# Did soft promote lose to a hard path? Note Host vs Target on python/glibc lines if present
rg -n 'python|glibc|libxcrypt|libcrypt' "$LOG" | rg '\[ebuild|Emerging' | head -40
```

**Known residual risk (record, don’t redesign):** if a **hard** path `virtual/libcrypt → … → libxcrypt` exists on the **same** MergeRoot (e.g. Target python DEPEND on virtual **and** Target glibc BDEPEND on that python), pass-2 cannot promote the soft edge. Live cross often Host-routes python BDEPEND — note Host vs Target if #3 still inverted.

### Out of scope

- Changing profile to `…/split-usr/…`  
- “Fixing” the depgraph `is_virtual` edge filter (wrong root cause)  
- package.provided / Favor alias for cross-T → real Cpn (library identity)  
- BuildClass, multi-em registry  
- Implementing further order heuristics beyond evidence

### Results template

```text
### Results — YYYY-MM-DD (Sonnet), soft-order #3

**em SHA:** …

#### Plan order (-p virtual/libcrypt pam)
- libxcrypt index vs virtual vs pam: …
- PASS/FAIL #3 plan: …

#### clang -b --jobs 80
- Progress: N ok, F failed, of M
- Emerging libxcrypt?: Y/N (index)
- Emerging glibc?: Y/N
- virtual/libcrypt Completed before/after libxcrypt?: …
- pam: success / fail (phase + reason)
- #3 schedule PASS/FAIL: …
- #4/#5/#1/#2: …
- clang reached?: …
- first failure if any: …

#### Library identity (optional note)
- outer cross-* VDB vs sysroot VDB: …

#### New bugs
- …
```

---

### Results — 2026-08-07 (Sonnet), soft-order #3

**em SHA:** `42e4042` (tip; code is `732d604`, `42e4042` on top is docs-only).
Fresh sandbox (`em-softorder-verify`, `sandbox prepare --bare`).

#### Plan order (`-p virtual/libcrypt sys-libs/pam`, isolated 2-target probe)

- Order: `sys-libs/glibc` → `sys-libs/libxcrypt` → `virtual/libcrypt` →
  `sys-libs/pam` — exactly the expected order.
- **PASS** on this isolated probe.

#### clang -b --jobs 80

- **Progress: 0 ok, 0 failed, of 135 — the run never started merging at
  all.** `EXIT=1` from a **pre-flight dependency check**, before the first
  `>>> Emerging` line:
  ```
  !!! pre-flight dependency check failed — these build dependencies are not
  satisfied by the installed view or earlier plan entries:
    sys-devel/gcc-16.1.1_p20260718 needs: sys-libs/glibc[cet(-)?]
  ```
- **Emerging libxcrypt? N/A** — nothing ever started emerging.
- **#3 schedule: cannot be evaluated this pass** — the run never reached
  the scheduling phase.
- **But the *plan-dump* order in this full 135-package graph is
  inverted relative to the isolated probe above — this is the important
  finding.** In the full clang plan: `virtual/libcrypt` is listed at index
  **66**, `sys-libs/pam` at **67** — both **before** `sys-libs/glibc` (86)
  and `sys-libs/libxcrypt` (87). This is the *opposite* order from the
  isolated 2-target probe (which correctly put glibc/libxcrypt first). The
  soft-order fix's correctness **does not generalize from the small
  reproduction case to the full real-world graph** — embedding the same
  virtual/provider/consumer triple in a much larger dependency graph (more
  SCCs, more competing soft/hard edges) produces the old, wrong order
  again. Caveat: this is the **printed plan order**, not confirmed
  execution order (the scheduler may pick ready packages dynamically
  rather than strictly walking the printed list) — but since the run
  never got past pre-flight, execution order couldn't be directly
  observed this pass either way.

#### New bug — pre-flight check catches `sys-devel/gcc` scheduled before its own BDEPEND `sys-libs/glibc`

Distinct from bug #3 (which was about a virtual completing without its
provider). Here a **real** `sys-devel/gcc-16.1.1_p20260718` (a second,
full, non-cross gcc for the target sysroot — same library-identity class
as the `sys-libs/glibc` re-plan noted in earlier passes) is listed at plan
index **85**, immediately **before** its own `sys-libs/glibc` BDEPEND at
index **86** — backwards. `sys-libs/glibc`'s planned USE string does
include `(-cet)` (the flag exists, just forced off), so the BDEPEND
`glibc[cet(-)?]` (accepts either value) *would* be satisfiable by the
planned glibc entry — except the checker correctly recognizes glibc
hasn't been merged yet at the point gcc would need to consume it, since
gcc is scheduled to run *first*. This reads as a genuine ordering
violation surfaced by (apparently new, and evidently useful) pre-flight
validation, not a false positive — the fix shape would be the same
family as bug #3's (ordering/scheduling for real-Cpn BDEPEND edges), but
this is the harder, non-virtual case: a plain real-to-real BDEPEND edge
between two full packages, not a virtual's provider selection. Not
root-caused to file:line this pass (out of scope — "record only" per the
rules); worth checking whether this predates the soft-order fix or is a
side effect of the pass-2 repair walk touching an unrelated part of the
same larger SCC.

#### #4/#5/#1/#2 regression smoke

- **#4:** PASS — `$SYSROOT/bin` is a real symlink after `crossdev
  --setup`, confirmed before the clang run.
- **#5/#1/#2:** not exercised this pass — the run never reached any real
  merge, so there's nothing to check die-honesty, sed/acl, or `--buildpkg`
  against.
- **Workdir:** not exercised (no merges attempted).

#### Library identity (note only)

Not re-checked this pass beyond what's already on record — the
`sys-devel/gcc`/`sys-libs/glibc` re-plan for the target sysroot (same
outer-vs-sysroot VDB split documented in the prior pass) is consistent
with, and plausibly the direct cause of, the new pre-flight failure above.

#### Summary

The isolated fix (as tested in the small probe) works exactly as
intended. But the full clang plan reveals two problems, neither
previously visible because earlier passes never got this far: (1) the
soft-order fix's correctness doesn't hold once the same virtual/provider
triple sits inside the full 135-package graph, and (2) a new, distinct
ordering violation between `sys-devel/gcc` and its own `sys-libs/glibc`
BDEPEND blocks the run before any merge starts. `llvm-core/clang` is
still not reached — this is the first pass where the blocker is a
pre-flight rejection rather than a mid-build failure, which is arguably
safer (no wasted build time, no partial/inconsistent sysroot state) but
still fully blocking.

#### Performance cost of the soft-order fix (`732d604`)

Separately requested: benchmarked `732d604` against the immediately-prior
`bf35c79` — criterion `resolve` microbench (mixed ±2-4% deltas, no
consistent direction) plus three same-run interleaved `hyperfine`
head-to-heads (exact soft-cycle scenario: 1.00×; full clang plan: 1.02×;
unrelated host target: 1.00×). **No measurable regression.** Full
writeup: `benchmarks/results/20260807-145520-softorder-732d604-vs-bf35c79/README.md`.

---

### Results — 2026-08-07 (Sonnet), `em stages --stage1` under `--root`+`--target`, and the `732d604` regression confirmed + fixed by `566b67b`

Luca asked for a quick check of `em stages --target riscv64-unknown-linux-gnu
--root <path> --stage1`. First pass found a real blocker; traced it to a
genuine regression from `732d604` (this session's own soft-order fix,
above); `566b67b` (landed while investigating) fixes it. Full trail below
since the first answer given in chat was wrong and needs a correction on
the record.

**Invocation note:** `--root` here must be the **outer** prefix
(`/root/xp`), not the sysroot itself — `--target` appends the
`usr/<tuple>` suffix internally. Pointing `--root` directly at the
already-nested sysroot double-appends the suffix and fails immediately
with `cannot resolve make.profile at .../usr/riscv64.../usr/riscv64.../etc/...`.
Correct form: `em --root /root/xp --target riscv64-unknown-linux-gnu
stages --stage1 -p`.

**Correction to an earlier same-session chat answer:** I initially blamed
a printed `!!! REQUIRED_USE flag constraints are unsatisfied` banner
(`app-alternatives/{yacc,tar,lex,gzip,bzip2,awk}`, each missing their
`^^ (...)` exactly-one-of selection under stage1's forced `USE="-*
build"`) for the failure. **That banner is not the blocker** — confirmed
by code read (`portage-resolve/src/required_use.rs`'s `find_violations` is
a post-solve advisory-only check; `portage-cli/src/query/depgraph/mod.rs`
explicitly excludes it from `exit_code`; the real gate is
`--autosolve-use`, opt-in and off by default, matching real `emerge -p`'s
own non-fatal advisory behavior for this same case) and empirically (the
banner appears identically on both a broken and a working run — see
below). This gap itself is pre-existing and long-known, not new: commit
`defb035` (2026-07-12) already documents hitting this exact
`app-alternatives/tar` `^^ ( gnu libarchive )` scenario during `em stages
--stage1`, weeks before this session.

**The actual blocker, found after re-reading the full (non-truncated)
output:**

```
!!! pre-flight dependency check failed — these build dependencies are not
satisfied by the installed view or earlier plan entries:
  sys-devel/gcc-16.1.1_p20260718 needs: sys-libs/glibc[cet(-)?]
```

`sys-devel/gcc` was printed at plan index 90, `sys-libs/glibc` (its own
BDEPEND) at 91 — backwards. This is the exact same bug class as the
`llvm-core/clang` plan's pre-flight failure reported in the soft-order
results above, now confirmed to also hit `stages --stage1`.

**Confirmed as a genuine regression from `732d604`, not a pre-existing
gap** — direct A/B against the pre-fix binary (`bf35c79`, still had a
copy from the earlier benchmark run) on the identical sandbox/scenario:

| em SHA | `sys-devel/gcc` (2nd instance) vs `sys-libs/glibc` | `-p` exit |
|--------|------|------|
| `bf35c79` (pre-`732d604`) | gcc at 60, glibc at 58 — **correct order** | `EXIT=0` |
| `42e4042` (post-`732d604`, pre-`566b67b`) | gcc at 90, glibc at 91 — **inverted** | `EXIT=1` |

Matches Luca's "it used to work" exactly: `bf35c79` really did work for
this scenario; `732d604` broke it.

**Now fixed by `566b67b`** (`fix(solver): lock pass-1-forward edges in
soft-order repair`, landed mid-investigation) — its own commit message
names this precise gcc/glibc case as the regression it addresses. Rebuilt
`em` from `566b67b`, re-ran the identical `-p` command:

- `sys-devel/gcc` (90) / `sys-libs/glibc` (91) → order restored, matches
  `bf35c79`.
- `EXIT=0`. `em stages --stage1` now completes its `-p` plan validation
  cleanly under `--root`+`--target`. The REQUIRED_USE advisory banner
  still prints (expected, confirmed non-fatal, unrelated) — present
  identically on both the broken and the fixed run.
- Also re-checked the `llvm-core/clang` plan-order regression from the
  soft-order results above with `566b67b`: `virtual/libcrypt` (66) /
  `sys-libs/pam` (67) still print before `sys-libs/glibc` (85) /
  `sys-libs/libxcrypt` (86) — **but `-p` now exits 0** (previously `1` on
  `42e4042`). This matches `566b67b`'s own documented remaining
  limitation exactly (commit message: "unit test documents the hard path
  virtual→python→glibc→libxcrypt that still blocks soft promote" — a real
  hard/soft conflict needing dual-root routing or library-identity Favor,
  not a repair-pass bug). Print order being "wrong" here no longer means
  the schedule/pre-flight gate is actually broken — worth remembering for
  future passes: check `-p`'s exit code and the pre-flight banner, not
  just the printed index order, before calling something broken.

**Not yet tested:** a real (non-`-p`) `em stages --stage1` merge to
completion — this pass only confirms the plan validates cleanly, not that
the actual bootstrap merges succeed end to end. Worth a follow-up pass if
someone wants that confirmed.

---

## Prior results (kept for history)

Last full live pass before soft-order fix: **`1ac8067`** — #4/#5 PASS, #3 still broken (virtual Completed, libxcrypt never Emerging, stop on pam). Details below in archived sections.

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

---

### Results — 2026-08-07 (Sonnet), `1ac8067`

**em SHA:** `1ac8067`. Fresh sandbox (`em-1ac8067-verify`, `sandbox prepare
--bare`).

#### crossdev --setup / #4

- **Baselayout merge root (log):** `sys-apps/baselayout-2.18-r1 ... to
  /root/xp/usr/riscv64-unknown-linux-gnu/` — target sysroot, not the outer
  prefix. Matches the fix's intent exactly.
- **`ls -la $SYSROOT/bin`:** `bin -> usr/bin`, a real symlink. Confirmed
  still intact after the full 135-package run finished (not clobbered by
  any later package).
- **`make.profile`:** resolves to
  `.../profiles/default/linux/riscv/23.0/rv64/lp64d` — default merged-usr
  profile kept, no `split-usr` in the path, as the product decision
  requires.
- **Outer vs sysroot VDB:** outer (`$P/var/db/pkg/cross-riscv64-unknown-linux-gnu/`)
  still has `binutils`/`gcc`/`glibc`/`linux-headers` as before. Sysroot's
  own VDB (`$SYSROOT/var/db/pkg`), previously completely empty, now has a
  real `sys-apps/baselayout-2.18-r1` entry.
- **#4: PASS.**

#### -p zlib (library identity)

- Same as prior passes: `sys-libs/zlib` alone has no direct DEPEND edge on
  glibc, so this probe doesn't exercise the identity question by itself
  (not a new result; noted for completeness).

#### clang -b --jobs 80

- **Progress: 65 ok, 1 failed, 65/135** (up from 42/136 on the `f8ac293`
  pass — real progress, not noise).
- **#4 (sysroot layout): PASS**, confirmed above.
- **#5 (die honesty): PASS**, and this pass has nothing to even test it
  against — **zero** `merged-usr`/`split-usr` die messages anywhere in the
  log this time (previous passes had 27). `sys-devel/gcc-config` and
  `sys-devel/binutils-config` both completed cleanly with single
  `Emerging`/`Completed` pairs, no die at all.
- **#1/#2: still fixed.** `sys-apps/sed` built with a single clean
  `Emerging`/`Completed`, no ACL race. `find /root/xp -name '*.gpkg.tar'`
  → 65 files for 65 ok packages, 1:1, zero tar errors.
- **#3: still broken, identical pattern.** `virtual/libcrypt-2-r1`
  completes (`Completed (32 of 135)`) while `sys-libs/glibc-2.43-r2` and
  `sys-libs/libxcrypt-4.5.2` are both listed as `[ebuild N]` in the plan
  but neither ever gets an `Emerging` line — matches the already-root-caused
  `query/depgraph/mod.rs:988-992` virtual-endpoint edge filter exactly (not
  re-investigated further this pass, per "record only" instruction). This
  is now the sole blocker: the run stops on `sys-libs/pam` (`Run-time
  dependency libcrypt/libxcrypt found: NO`), the same downstream failure
  as every previous pass.
- **Workdir: still holds.** No duplicate `Emerging (N of 135)` index.
- **`llvm-core/clang` itself: still not reached** — 65/135, stopped before
  getting there. This remains the single next blocker for the whole
  investigation.

#### New bugs

None. This pass is a clean confirmation: #4 and #5 are both genuinely
fixed with no caveats found. The only remaining item blocking
`llvm-core/clang` from ever being reached is bug #3, already root-caused
to `query/depgraph/mod.rs:988-992` in the previous pass — fixing that
filter (without breaking whatever it was originally added for) is the
clear next step for whoever picks this up.
