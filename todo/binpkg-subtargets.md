# Binpkg identity: cross roots + ISA sub-targets

STATUS: **design** (2026-07-18). Not implemented. Driven by Fable review of
global-CHOST reuse + crossdev-stages’ heavy `-b -k` use across boards that
share a CHOST but differ in `-march` / CFLAGS.

Related: [[PENDING]] binhosts section; `portage-binpkg` index; crossdev-stages
`lib/sysroot.sh` (PKGDIR per CFLAGS-named sysroot).

## Why this exists

Three overlapping needs:

1. **Cross plans** (`em --target T …`) schedule both **target** packages and
   **host** BDEPEND copies (`MergeRoot::Host`). Reuse must use the **entry’s**
   CHOST, not one global make.conf CHOST.
2. **Board / micro-arch variants** share CHOST (e.g. all
   `riscv64-unknown-linux-gnu`) but differ in CFLAGS (`-march=rv64gcv_zvl256b`
   vs `rva23u64` vs generic `rv64gc`). Same problem on aarch64 (`-mcpu=…`) and
   x86_64 (`-march=x86-64-v3`). Wrong reuse → SIGILL or silent ABI mismatch.
3. **Automation** (crossdev-stages style): repeated `emerge -b -k` / `em -b -k`
   should fill a cache and hit it on re-rolls without hand-pruning or accidental
   cross-board reuse.

Portage’s stock rule is **CPV + USE ∩ IUSE** (and soft CHOST trust). em already
records CFLAGS/CXXFLAGS and gates CHOST once globally. That is not enough for
(1) or (2).

---

## Scenario matrix

### S0 — Native host, single make.conf CFLAGS

| | |
|--|--|
| Roots | one (merge = broot = `/` or one `--root`) |
| Identity | CPV + USE + CHOST + build-env key (from that make.conf CFLAGS) |
| PKGDIR | one |
| Today | Works for USE; CHOST global is fine; no multi-march |

### S1 — Cross `--target T`, single target CFLAGS (the common em case)

| | |
|--|--|
| Plan | Target packages + occasional `MergeRoot::Host` BDEPEND |
| Target binpkg CHOST | `T` (e.g. riscv64-…) |
| Host binpkg CHOST | CBUILD / host make.conf (e.g. aarch64-…) |
| Desired reuse | Target entry ↔ target PKGDIR / index; Host entry ↔ **host** CHOST packages |

**Bug today:** one `desired_chost` from target config → host BDEPEND never
reuses host-built binpkgs (safe miss).

**Fix:** per-entry desired CHOST (and CFLAGS) from **that entry’s config root**
(`entry_roots` already chooses Host vs Target for merge).

### S2 — Multiple boards, same CHOST, different CFLAGS (crossdev-stages)

crossdev-stages model (simplified):

- Boards: k1, k3, blackhole, … all `riscv64` CHOST.
- CFLAGS: `-O3 -march=rv64gcv_zvl256b`, `-O3 -march=rva23u64`, …
- **Isolation strategy today in crossdev-stages:** boards that share a
  **SYSROOT name** share a **sysroot + PKGDIR** (`PKGDIR=${crossdev_root}/packages`).
  Different CFLAGS sets → different sysroot names → **separate PKGDIRs**.
  That is correct by construction: `-k` never sees the other board’s packages.

So stages can stay correct **without** multi-instance **if** each CFLAGS set
has its own PKGDIR (or own binhost URL).

### S3 — One shared PKGDIR / one binhost, many CFLAGS (the em automation goal)

Operator wants e.g.:

```text
/var/cache/binpkgs/          # or one remote Packages index
  sys-libs/glibc-…-1.gpkg.tar   # march=rv64gc
  sys-libs/glibc-…-2.gpkg.tar   # march=rv64gcv_zvl256b
  …
```

Then:

- `em -k` for board A must pick only A-compatible instances.
- `em -b` for board B must **add** a new BUILD_ID, not overwrite A’s.
- `em maint binpkg prune` must **not** collapse A and B to “newest BUILD_ID”.

This needs **multi-instance index + reuse keyed by build-env**, not only
separate PKGDIRs.

### S4 — Host packages built once, reused across all target boards

Host tools (`cmake`, `meson`, python modules as BDEPEND) are **host CHOST** and
usually **host CFLAGS** (or generic). They should:

- Live in host PKGDIR (or be findable under a dual-root index view).
- Match Host plan entries regardless of target board CFLAGS.
- Not be pruned by target-side “keep one per CPV”.

### S5 — Remote binhost with mixed variants

Server publishes one `Packages` with many CPV×BUILD_ID rows, each with CHOST +
CFLAGS. Client:

- Parses **all** instances (not last-wins per CPV).
- `URI` header still sets BASE_URI.
- `find_reusable` picks matching USE+CHOST+build-env; prefer highest BUILD_ID
  among matches.

### S6 — package.env / per-package CFLAGS workarounds

crossdev-stages has `WORKAROUND_PKGS` / `WORKAROUND_CFLAGS`. em has package.env
build-env slice but **not** resolver-side USE; CFLAGS from env files **are**
applied at build if wired.

**Implication:** desired build-env for reuse should be the **effective** flags
for that package (make.conf ± package.env), not only global make.conf — at
least once package.env CFLAGS are reliable on the build path. Phase 1 can use
global make.conf CFLAGS; phase 2 folds package.env.

---

## Identity model (what makes two binpkgs interchangeable)

A candidate is reusable iff all of the following hold:

| Axis | Rule | Empty / missing |
|------|------|-----------------|
| **CPV** | exact | required |
| **USE ∩ IUSE** | equal (Portage bug #453400) | as today |
| **CHOST** | equal | either empty → skip gate |
| **Build-env key** | equal | either empty → skip gate (compat with old GPKGs) |

### Build-env key (proposal)

Not full CFLAGS string equality (too brittle: `-O2` vs `-O3`, flag reorder).

**Extract ISA / ABI-relevant tokens** from CFLAGS (and CXXFLAGS if CFLAGS
lacks them), normalize, sort, join:

```
-march=…
-mcpu=…
-mtune=…          # optional: include — affects codegen, not always ABI
-mfpu=…
-mfloat-abi=…
-mabi=…           # riscv
```

Examples:

| CFLAGS | build_env_key |
|--------|----------------|
| `-O3 -pipe -march=rv64gcv_zvl256b` | `-march=rv64gcv_zvl256b` |
| `-O3 -march=rva23u64 -pipe` | `-march=rva23u64` |
| `-O2 -march=x86-64-v3 -pipe` | `-march=x86-64-v3` |
| `-O3 -mcpu=neoverse-n1` | `-mcpu=neoverse-n1` |
| `-O2 -pipe` (no -m*) | fallback: full normalized CFLAGS, or empty key meaning “generic” |

**Open choice (decide at implement):**

| Policy | Pros | Cons |
|--------|------|------|
| **A. Tokens only** | Stable across -O level | Misses rare non-token ABI flags |
| **B. Tokens + hash of full CFLAGS** | Strict | More rebuilds |
| **C. Explicit `BINPKG_ABI` / `FEATURES` tag** | Operator control | Extra config surface |

**Recommendation:** **A** with fallback to normalized full CFLAGS when no
`-m*` tokens exist. Store full CFLAGS/CXXFLAGS on the entry for display and
stricter future policy; key only drives match/prune.

Optional later: parse march feature sets (e.g. “has V”) for **compatible**
reuse (generic consumer can use generic package; V package cannot go to
non-V). That is **asymmetric** compatibility — do **not** do this in v1;
exact key match only.

---

## Storage and index model

### On disk (unchanged GPKG layout)

```text
PKGDIR/
  <cat>/<PF>-<BUILD_ID>.gpkg.tar
  Packages
```

Multiple BUILD_IDs per CPV already work on write (`next_build_id`). Problem is
**index and prune treat CPV as unique**.

### In-memory / Packages

```text
cpv → Vec<BinpkgEntry>   # not BTreeMap<cpv, single Entry>
```

Each entry carries: path, USE, IUSE, CHOST, CFLAGS, CXXFLAGS, BUILD_ID,
derived `build_env_key`.

`Packages` may list **multiple blocks with the same CPV** (different PATH /
BUILD_ID / CFLAGS). Portage multi-instance already allows this shape; Portage
consumers that ignore CFLAGS still see multiple instances.

### Prune policy (change)

| Old | New |
|-----|-----|
| Keep newest BUILD_ID **per CPV** | Keep newest BUILD_ID **per (CPV, CHOST, build_env_key)** |

So k1 and k3 glibc both stay. Two rebuilds of the same board only keep the
newest.

Optional flags later: `em maint binpkg prune --by-cpv` (old behaviour) for
operators who use one CFLAGS per PKGDIR and want aggressive collapse.

### PKGDIR layout strategies (all valid; em should support both)

| Strategy | How | When |
|----------|-----|------|
| **Separate PKGDIR per variant** | make.conf `PKGDIR=…/packages-rv64gcv` or per-sysroot `…/usr/$CHOST/packages` | crossdev-stages today; simplest ops |
| **Shared PKGDIR + multi-instance** | one PKGDIR, many BUILD_IDs, key match | single binhost serving many boards |
| **Separate binhost URI per variant** | `binrepos.conf` sections with priorities | remote mirror of separate PKGDIRs |

Automation tip for stages:

```bash
# Per-board (or per-CFLAGS fingerprint) — zero multi-instance required
PKGDIR=/var/cache/em-binpkgs/${CHOST}/$(cflags_fingerprint)
em --target $T -b -k --emptytree @system
```

`cflags_fingerprint` can be `build_env_key` slugged, or a short hash of
normalized CFLAGS. Document as the **default recommended** layout; implement
multi-instance so a **shared** PKGDIR also works.

---

## Reuse algorithm (merge path)

For each plan entry:

1. Resolve **entry roots** (`MergeRoot::Host` → host, else target) — already done.
2. Desired CHOST = make.conf `CHOST` under **entry config root** (not global).
3. Desired CFLAGS = make.conf `CFLAGS` under **entry config root**
   (phase 2: overlay package.env for that atom).
4. Desired key = `build_env_key(desired_cflags, desired_cxxflags)`.
5. `find_reusable(cpv, use, chost, key)`:
   - among all instances for cpv
   - filter USE + CHOST + key
   - pick max BUILD_ID
6. Local first, then remote indices (same filter).

**Cross dual-index (optional phase 2):** today one `resolve_pkgdir(globals)` —
usually the **target** PKGDIR under `--target`. Host BDEPEND may need to look
at **host** PKGDIR as well (or a combined view).

| Phase | Host BDEPEND binpkg lookup |
|-------|----------------------------|
| 1 | Same PKGDIR; CHOST gate alone separates host vs target packages if both CHOSTs appear in one index (unusual) |
| 2 | Prefer `host_pkgdir` for Host entries, `target_pkgdir` for Target entries |

crossdev-stages puts target packages under `${crossdev_root}/packages` and
host packages under the sandbox host PKGDIR — **two caches**. em should
mirror that with per-root `resolve_pkgdir` from `entry_roots`, not only
`globals.roots()`.

### PKGDIR resolution today

`resolve_pkgdir(globals)` uses `globals.roots()` (target-substituted under
`--target`). That is right for **target** packages. For **Host** entries,
PKGDIR should come from `broot()` / outer roots make.conf (or host default
`/var/cache/binpkgs`).

**Scenario S1 correctness needs both:** per-entry CHOST **and** per-entry
PKGDIR (or dual index open).

---

## Producer path (`-b` / quickpkg)

Already:

- Writes CFLAGS/CXXFLAGS into VDB/GPKG metadata.
- Auto-reindexes Packages.
- Allocates next BUILD_ID per PF.

Needs:

- Index regeneration lists **all** containers (already walks files) — parser
  must **not** collapse by CPV.
- Do **not** delete older env variants on write.
- quickpkg: same multi-instance rules.

---

## Remote / binrepos

- `RemoteBinpkgIndex`: multi-entry per CPV; same `find_reusable`.
- Header `URI` BASE_URI (harden parse: lines until first `CPV:`, not only
  `\n\n` first block).
- Optional: publish separate `binrepos.conf` sections per board with
  `sync-uri=…/rv64gcv/` — works with **separate** PKGDIR export, no multi-
  instance required on the client.

---

## Automation recipes (what “easy” means)

### Recipe 1 — Separate PKGDIR (stages default, zero risk)

```bash
KEY=$(em … print-build-env-key)   # or shell slug of -march=
export PKGDIR=/var/cache/em-binpkgs/${CHOST}/${KEY}
em --target $T -b -k @system
```

Fingerprint from the same `build_env_key` function em uses for matching.

### Recipe 2 — Shared PKGDIR (one disk cache, multi-board)

```bash
export PKGDIR=/var/cache/em-binpkgs/shared
# board A
em --target riscv64-… --root /boards/k1 -b -k @system   # CFLAGS in that root's make.conf
# board B
em --target riscv64-… --root /boards/k3 -b -k @system
# both sets remain; -k only hits matching key
em maint binpkg prune   # keeps one BUILD_ID per (cpv, chost, key)
```

### Recipe 3 — Cross host tools

```bash
# Host tools accumulate in host PKGDIR with host CHOST
em -b -k dev-build/cmake
# Cross plan Host BDEPEND reuses them via per-entry CHOST + host PKGDIR
em --target $T -k -e @system
```

### Recipe 4 — Remote

```ini
# binrepos.conf
[riscv64-rvv]
sync-uri = https://binhost/riscv64/rv64gcv
priority = 10

[riscv64-generic]
sync-uri = https://binhost/riscv64/rv64gc
priority = 5
```

Or one URI with multi-instance Packages and client-side key match.

---

## Phased implementation

### Phase 0 — document + scenarios (this file)

### Phase 1 — correctness for cross + multi-instance foundation

1. **Per-entry CHOST** from entry config root.
2. **Per-entry PKGDIR** (open host vs target index as needed).
3. **Multi-entry index** (`cpv → Vec`); stop last-wins parse.
4. **`build_env_key` + gate** in `find_reusable` (empty key skips).
5. **Prune** by `(cpv, chost, build_env_key)`.
6. **parse_index_header** harden.
7. Tests: S1 host BDEPEND reuse; two CFLAGS same CPV both kept; prune keeps both keys; wrong march rejected.

### Phase 2 — automation UX

1. `em maint binpkg list` shows CHOST + build_env_key (+ CFLAGS truncated).
2. Helper: `em … env fingerprint` or document make.conf-only fingerprint.
3. Stages: default PKGDIR layout suggestion under `--target` / board roots.
4. package.env CFLAGS in desired key.

### Phase 3 — optional sophistication

1. Asymmetric “generic can serve stricter consumer? **no** / “strict can serve
   generic? **no**” — stay exact match unless someone needs feature-subset.
2. GPG still independent.
3. RVV-specific feature decoding only if exact march strings prove painful.

---

## Non-goals (v1)

- XPAK.
- Matching Portage’s multi-instance BUILD_ID selection UI.
- Soft “compatible march” matrix (v1 exact key only).
- Changing GLEP 78 container format (metadata fields already enough).

---

## Mapping from Fable findings

| Finding | Scenario | Disposition |
|---------|----------|-------------|
| Global CHOST over-conservative for Host entries | S1 | Phase 1.1 + 1.2 |
| parse_index_header needs blank line | S5 edge | Phase 1.6 |
| RVV / multi-march | S2, S3 | Phase 1.3–1.5 + recipes |
| prune collapses multi-BUILD_ID | S3 | Phase 1.5 |

---

## Open decisions (need product call)

1. **Default PKGDIR policy for `em stages`:** keep single root PKGDIR + multi-
   instance, or auto-subpath by `build_env_key`?  
   **Lean:** multi-instance in whatever PKGDIR is configured; document
   subpath as best practice for isolation.
2. **Include `-mtune` in key?** Affects performance not ABI.  
   **Lean:** include only `-march`/`-mcpu`/`-mabi`/`-mfpu`/`-mfloat-abi` first.
3. **Host PKGDIR when `--target` and no host make.conf PKGDIR:**  
   **Lean:** `/var/cache/binpkgs` host default (current host behaviour).

---

## Success criteria

- [ ] Cross plan: host BDEPEND reuses host binpkg when USE+CHOST match.
- [ ] Same PKGDIR: two glibc GPKGs with different `-march` both retained after
      prune; `-k` selects the one matching current make.conf CFLAGS.
- [ ] Separate PKGDIR recipe still works without multi-instance (key gate
      no-ops when only one instance / empty keys).
- [ ] Remote multi-block same-CPV Packages works.
- [ ] No wrong-arch or wrong-march reuse (property tests / unit tests).
