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

_(Sonnet fills in)_
