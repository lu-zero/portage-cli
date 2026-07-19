# Binpkg identity: cross roots + ISA sub-targets

STATUS: **Phases 1/1a/1b/2 landed** (Phase 1: vibe, 2026-07-19; 1a/1b/2:
Claude/Sonnet + Fable, same day). Scenario design still authoritative. Open:
live S1 sandbox verification (see Success criteria). Operator-facing docs
(recipes, `fingerprint`, identity model) now live in
[`docs/binhost.md`](../docs/binhost.md) — this file stays the design/progress
record.

Related: [[PENDING]] binhosts section; `portage-binpkg` index; crossdev-stages
`lib/sysroot.sh` (PKGDIR per CFLAGS-named sysroot).

Commits (representative): `0f2d77f` sokgi dep · `6b2f3be` `build_env_key` ·
`c94ed73` prune by identity · `58802cb` `read_make_conf_var_for_roots` ·
`306fdec` merge per-entry CHOST/key · `73d5bfb` preview signature ·
`b8831df` ISA/ABI token pre-filter · `a163ba0` asymmetric gate + max-BUILD_ID ·
`5209462` header/entry parse boundary · `3187b80` per-entry PKGDIR dual index ·
`023c7da` make.conf `${VAR}` expansion · `09ae887` `list` columns ·
`5329262` `fingerprint` · `744848f` package.env key.

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

> Polished, up-to-date operator version (with the real `fingerprint`
> command and sample output) lives in
> [`docs/binhost.md`](../docs/binhost.md). Kept here too for the design
> rationale each recipe was derived from.

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

## What landed (vibe, 2026-07-19)

### Done well

| Item | Where | Notes |
|------|--------|--------|
| Multi-instance index | `BinpkgIndex` / `RemoteBinpkgIndex`: `cpv → Vec<BinpkgEntry>` | Last-wins gone |
| `build_env_key` | `portage-binpkg::build_env_key` via **sokgi** | ABI/ISA tokens filtered first, then sokgi hash; `__native__` for machine-dependent |
| Gate in `find_reusable` | USE + CHOST + build_env_key | Empty either side skips that gate |
| Prune by identity | `(cpv, chost, build_env_key)` keep newest BUILD_ID | S3 coexistence |
| Per-entry desired CHOST / CFLAGS | `read_make_conf_var_for_roots(entry_roots, …)` in merge seq/parallel | S1 Host vs Target make.conf |
| Preview path | `output.rs` updated for 4-arg `find_reusable` | |
| Unit tests | march differ/same order, native, ldflags, rustflags | 8 `build_env_key_*` tests green |

### Design divergences (vs original token-only proposal)

Original lean was **ISA tokens only**. Vibe first shipped full sokgi hashes
over all flags (too strict). **Refined:** pre-filter to ABI/ISA tokens
(`-march`/`-mcpu`/`-mtune`/`-mabi`/…, Rust `target-cpu`/`target-feature`),
then sokgi canonicalize + hash. `-O*`/`-pipe`/`-g`/generic `-Wl,…` no longer
split the cache; different `-march` still does; `-march=native` → `__native__`.

| Residual | Detail |
|----------|--------|
| **RUSTFLAGS in VDB** | Still often empty on recorded GPKGs if not written at merge |
| **package.env CFLAGS** | Desired key still global make.conf per root |

### Still open / incomplete

| Gap | Scenario | Severity |
|-----|----------|----------|
| ~~**Per-entry PKGDIR** — still one `resolve_pkgdir(globals)`~~ | S1, S4 | **Done in Phase 1b** |
| ~~**`parse_index_header`** still `\n\n` first block only~~ | S5 edge | **Done in Phase 1b** |
| ~~**RUSTFLAGS** not in VDB/GPKG write path~~ | key consistency | **Done in Phase 1b** |
| **package.env** CFLAGS not in desired key | S6 | Medium for workarounds |
| **S1 live test** host BDEPEND reuse | S1 | Code done; **live sandbox verification still open** |
| **Stages PKGDIR recipe / fingerprint helper** | automation | Phase 2 |
| **maint list** shows key/CHOST | UX | Phase 2 |
| ~~Dead global `desired_chost`~~ | cleanup | **Done in Phase 1b** |

---

## Phase 1a — correctness fixes found in review (landed, 2026-07-19)

Before starting Phase 1b, an independent review (Fable/Claude, verified by
hand against the actual code) found two bugs in the just-landed gate itself
that are **worse** than the "safe miss" gaps Phase 1b was about to prioritize
— both are **wrong-binary reuse**, not just a missed-cache-hit:

1. **Empty desired key over-matched a keyed binpkg.**
   `build_env_key_compatible` was `binpkg_key.is_empty() || desired_key.is_empty()
   → skip`. A generic-CFLAGS board (no `-m*` flags → computed key `""`) has
   an empty *desired* key, which alone satisfied the OR — so it would reuse
   a binpkg keyed to a specific `-march=...` from another board (SIGILL risk,
   exactly what this feature exists to prevent). Fixed to an **asymmetric**
   rule: permissive only when the **binpkg's own** key is empty (legacy/sparse
   — unknown build-env, backward compat with old GPKGs); a keyed binpkg
   against an empty *desired* key is now rejected.
2. **First-listed match won, not the newest.** `find_reusable` returned the
   first `Vec` entry passing the gates; `BinpkgEntry` had no `build_id` field
   at all, so "first" was scan/parse order, not BUILD_ID order — this was
   promoted above Phase 1b's original "Medium" rating once traced through:
   a legacy flagless entry (passes every gate per bug 1) could sit earlier in
   the list than a newer exact match and win every time. Fixed: `build_id`
   added to `BinpkgEntry` (parsed from the `BUILD_ID` field, both parsers),
   both `find_reusable` methods now take the max-`build_id` match among all
   passing entries.
3. **ISA/ABI allowlist was incomplete.** `is_c_family_abi_token` missed
   `-mrvv-vector-bits=` (the riscv64 zvl boards this feature targets),
   `-mno-outline-atomics`, `-mcmodel=`, `-mbranch-protection=`, `-mfpmath=`,
   and the whole x86 feature-toggle family (`-mavx2`/`-mno-avx2`/…) — any of
   these differing between boards was invisible to the key, same failure
   class as bug 1. Rather than keep growing an allowlist, switched to GCC's
   own convention (`-m*` = "machine dependent options"): `is_c_family_abi_token`
   is now `tok.starts_with("-m") && tok != "-m"`. Over-keying only costs an
   extra rebuild (Policy B's accepted tradeoff); under-keying risks silent
   wrong-arch reuse, so the broad side is the safe one.

Also fixed as low-risk hygiene: `cargo fmt` (vibe's commits didn't pass
`fmt --check`); `read_make_conf_var` now delegates to
`read_make_conf_var_for_roots` instead of duplicating it
(`portage-cli/src/binpkg.rs`); `maint.rs::container_build_id` takes the
already-read metadata map instead of re-reading the container from disk.

All in `portage-binpkg/src/index.rs` / `maint.rs` and
`portage-cli/src/binpkg.rs`; 41 `portage-binpkg` tests green (6 new), full
`portage-cli` suite green under `--test-threads=1` (two tests are
pre-existing parallel-run flakes, unrelated to this area, confirmed passing
serially both before and after this change).

---

## Refined residual plan

### Phase 1b — finish cross correctness (landed, 2026-07-19)

Planned by Fable (independent plan, verified against the actual source before
implementing) and implemented by Claude/Sonnet, one commit per item:

1. **VDB/GPKG RUSTFLAGS.** Producer never recorded it despite the consumer
   already reading it — `EbuildEnv`/`MergeSpec`/`vdb::register`/`portage-binpkg`
   regen all gained the field. Promoted first in the plan: a binpkg whose only
   ISA-relevant flags lived in RUSTFLAGS had an *empty* build_env_key, so the
   Phase 1a asymmetric gate treated it as legacy-permissive — same wrong-reuse
   class as Phase 1a's bugs, just a different flag source.
2. **Hardened the `Packages` header/entry parsing boundary.** A single shared
   `split_header_body` helper (stop at the first blank line or the first
   `CPV:` line) replaces two independent `"\n\n"`-splits that could disagree:
   `parse_index_header` used to lose a glued header's `URI` entirely, and
   `parse_index_blocks` used to merge a glued header's fields into the first
   entry's own (a real header field could leak into an entry that legitimately
   omitted it).
3. **Dropped the dead global `desired_chost`** from
   `merge_sequential`/`merge_parallel` (per-entry `desired_chost_entry` had
   already superseded it; the global arrived as an unused `_desired_chost`
   parameter). Also fixed 6 pre-existing `needless_borrow` clippy warnings in
   `merge_parallel` while there.
4. **Per-entry PKGDIR + dual index (S1/S4).** `resolve_pkgdir_for_roots(&Roots)`
   added alongside `resolve_pkgdir(&Cli)` (now a thin delegate). `run_merge_plan`
   resolves both `target_pkgdir`/`host_pkgdir`, opens a second `BinpkgIndex` for
   the host side only when the plan has `MergeRoot::Host` entries *and* the two
   paths genuinely differ (`dual_pkgdir` — a no-op outside `--target`), and a
   new `entry_binpkg_index()` helper (mirrors `entry_roots()`) picks the right
   index per entry. **No fallback** to the target index when the host index is
   unavailable — a Host entry with no host index just misses and builds,
   rather than reintroducing cross-PKGDIR confusion. `--buildpkg`'s
   writable-PKGDIR preflight now also checks the host PKGDIR when dual.
5. **Tests:** 4 new in `portage-binpkg` (header/glue-leak cases), 2 new in
   `portage-cli/src/binpkg.rs` (`resolve_pkgdir_for_roots` target-vs-host,
   config-root override), 2 new in `merge/mod.rs` (`entry_binpkg_index`
   host-vs-target selection, no-fallback-when-missing).

**Still open from the original list:** a **live sandbox check** that a real
`--target` build's Host BDEPEND actually reuses from the host PKGDIR (code
path is done and unit-tested; not yet run against a real riscv64 crossdev
sandbox — do this before ticking the matching Success-criteria box below).
`em maint binpkg list` UX and the stages PKGDIR recipe doc are Phase 2,
untouched.

### Phase 1c — key policy

**Chosen:** ISA/ABI token filter → sokgi hash (not full CFLAGS). Broadened in
Phase 1a from an explicit prefix allowlist to "any `-m*` token" (GCC/Clang's
own "machine dependent options" convention) specifically so a missed selector
(the allowlist had missed `-mrvv-vector-bits=`/`-mno-outline-atomics`/x86
feature toggles) can't silently under-key again — nothing left to extend here
short of a genuinely new non-`-m` ISA flag family.

### Phase 2 — automation UX (landed, 2026-07-19)

Planned by Fable, implemented by Claude/Sonnet. Found and fixed a
prerequisite bug during planning (see "Phase 2a" below) before the 4 UX
items themselves.

1. ✅ `em maint binpkg list`: CHOST/KEY(short build_env_key)/CFLAGS(trunc)
   columns added (`IndexRow` extended; `build_env_key_from_fields`/
   `short_build_env_key` helpers in `portage-binpkg::index`).
2. ✅ Documented — [`docs/binhost.md`](../docs/binhost.md), recipes 1–4
   (separate PKGDIR, shared multi-instance, cross host tools, remote
   binrepos.conf sections).
3. ✅ `em maint binpkg fingerprint [--full] [--host]` — done, under the
   existing `maint binpkg` nesting rather than a new top-level applet.
4. ✅ package.env CFLAGS folded into the per-package desired key
   (`DesiredBuildEnv::key_for`, a real brush shell round-trip via
   `MakeConf::apply_to` — see Phase 2a) — S6/WORKAROUND_CFLAGS-style
   overrides now get a binpkg key their own build can actually reproduce.
5. ✅ Documented as the recommended default (not code-enforced) —
   `docs/binhost.md`'s Stages section.

### Phase 2a — prerequisite fix found during Phase 2 planning

`MakeConf::get` returns `${VAR}` references unexpanded. The stock Gentoo
stage3 pattern `COMMON_FLAGS="-O2 -march=…"` + `CFLAGS="${COMMON_FLAGS}"`
therefore made every desired-key read see a literal, `-m*`-token-free
`${COMMON_FLAGS}` string — on such a host the desired key computed empty
while the binpkg producer recorded the real, shell-expanded flags, and the
Phase 1a asymmetric gate then **rejected** that keyed binpkg against the
empty desired key: a permanent rebuild loop on stock configs, worse than
any of the Phase 2 UX gaps. Fixed first: `MakeConf::apply_to` sources the
raw file through a minimal, non-interactive `brush_core::Shell` (bash's
standard builtin set only — no ebuild-specific builtins, no `Repository`
dependency) rather than hand-rolling a `${VAR}`-only scanner — brush (a
real bash interpreter) is already a project dependency, so this gets full
bash semantics (`${VAR:-default}`, command substitution, …) for free
instead of a second bash subset to maintain. `read_make_conf_var_for_roots`
now evaluates through it.

### Phase 3 — later

- Soft compatible-march matrix (probably never).
- GPG.
- Portage multi-instance UI parity.

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
| Global CHOST over-conservative for Host entries | S1 | **Done:** per-entry CHOST + per-entry PKGDIR dual index (Phase 1b) |
| parse_index_header needs blank line | S5 edge | **Done** (Phase 1b, `split_header_body`) |
| RVV / multi-march | S2, S3 | **Done** (key + multi-instance + prune + broadened `-m*` filter) |
| prune collapses multi-BUILD_ID | S3 | **Done** (`c94ed73`) |

---

## Open decisions (need product call)

1. **Default PKGDIR policy for `em stages`:** keep single root PKGDIR + multi-
   instance, or auto-subpath by `build_env_key`?  
   **Lean:** multi-instance in whatever PKGDIR is configured; document
   subpath as best practice for isolation. *(unchanged)*
2. ~~**Key strictness**~~ — ISA filter before sokgi (**done**).
3. ~~**Host PKGDIR when `--target` and no host make.conf PKGDIR**~~ —
   **done**: `resolve_pkgdir_for_roots` under host roots falls to
   `/var/cache/binpkgs` exactly as leaned; dual open implemented in Phase 1b.

---

## Success criteria

- [x] Multi-instance index + build_env_key gate + prune by identity
- [x] Per-entry desired CHOST / CFLAGS from entry make.conf
- [ ] Cross plan: host BDEPEND reuses host binpkg **from host PKGDIR** when
      USE+CHOST match — code done + unit-tested (`entry_binpkg_index`
      host-vs-target selection), **live sandbox run still open**
- [ ] Same PKGDIR: two glibc GPKGs with different `-march` both retained after
      prune; `-k` selects the one matching current make.conf CFLAGS — logic
      covers this (prune groups by `(cpv, chost, build_env_key)`), no
      dedicated multi-march prune test yet
- [x] Separate PKGDIR recipe still works without multi-instance (key gate
      no-ops when only one instance / empty keys) — `empty_binpkg_key_is_legacy_permissive`
- [x] Remote multi-block same-CPV Packages works (parser yes; prefer max BUILD_ID)
- [ ] No wrong-arch or wrong-march reuse — unit coverage now comprehensive
      (empty-key asymmetry, broadened `-m*` filter, max-BUILD_ID selection all
      tested); **live S1 sandbox check still open**
