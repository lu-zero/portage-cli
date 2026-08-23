# `--emptytree` (`-e`) — design and implementation

Tracking emerge parity for `em -pe` / `em -e`. Updated 2026-06-17 after
rejecting the `grok-broke-emptytree` approach (`prepend_host_build_pretend`,
`trim_bootstrap_gcc`, skip-`host_installed` hacks).

> **PARITY REACHED 2026-06-19 (re-verified).** `em -pe www-client/firefox` in
> the clean stage3 sandbox is **0 diffs** vs `emerge -pe` (both 383, identical
> sets) — the chroot edge-follow gap (was em 371 vs emerge 396) and the
> toolchain divergence (gcc-16/binutils upgrades, llvm-22, six rust-bin
> bootstrap slots, gcc-11) are both gone. On the dev host the **only** residual
> diff is the pre-existing slotless-rust `||` preference (`em` rust-bin vs
> `emerge` source rust) — see `license-use-conditional-bug.md`. The sections
> below remain as the investigation history; the "Open / deferred" polish list
> at the bottom is the only live item set.

> **2026-06-18 — AGREED REDESIGN below supersedes the `broot_filter` + expand-pass
> implementation.** Everything from "## Implementation plan" down documents the old
> (crap) approach and the investigation that led here; keep for history but do not
> treat it as the target. `emptytree_expand.rs` and the rust/gcc hardcodes are being
> deleted.

## AGREED REDESIGN: the simple solution (2026-06-18)

### Two concerns the old code fused

| concern | correct source | old code (wrong) |
|---|---|---|
| **plan membership** (what to list/build) | the full deep closure — every dep class, nothing pruned | `broot_filtered` deletes host-satisfied BDEPEND, then `expand_satisfied_rebuilds` re-adds it from incomplete info |
| **action tags** (`N`/`R`/`U`) | post-solve lookup in the **destination VDB** | mixed into membership |

### Why the old path is crap

It **prunes the closure during the solve (`broot_filtered`) then reconstructs it
post-solve** (`emptytree_expand.rs`, ~640 lines, with hardcoded `dev-lang/rust` /
`sys-devel/gcc` slot trims). The reconstruction is lossy (re-added nodes carry no
real edges → "orphan" nodes → name-matched heuristics that broke parity as 399 /
402 / orphan). Dropping the expand pass proves the re-listing is load-bearing:
`em -pe firefox` falls 400 → 269, losing exactly the 132 host-satisfied build tools
(autoconf, cmake, perl tail, docbook, `rust-1.95`, …).

### Why the simple solution was "ignored": historical accident

`broot_filtered` was built for **offset/stage** (`--root <empty> --config-root /`)
and `--with-bdeps`, where pruning host-provided build tools is *correct*. `--emptytree`
was then wired in by **reusing that path** (`solve_with_bdeps = with_bdeps ||
emptytree_native`), inheriting the prune — exactly wrong for emptytree — and the
expand pass was bolted on to undo it. (`InstalledPolicy::Rebuild` is even dead under
emptytree: the solver's installed set is empty, so its branch never fires.)

### The redesign

1. **Planner installed view = the stage1 seed only** (BROOT = host `/`), used **only**
   to resolve bootstrap version choices and break build-order cycles — **never to
   prune membership**.
2. **Solve the full deep closure, no `broot_filtered`** → the solver yields all ~400
   nodes *with real edges* directly (already demonstrated in the stage3 chroot, where
   minimal `host_installed` gave 371 ≈ emerge 396).
3. **Tags are a post-solve destination-VDB lookup** (`N`/`R`/`U`) — display only.
4. Delete `emptytree_expand.rs` and every rust/gcc hardcode.

### `broot_filter` becomes mode-aware (the broot redesign)

| mode | BDEPEND handling |
|---|---|
| normal native `/` install | host satisfies — prune (don't rebuild host tools) |
| **native `--emptytree`** | **no prune** — list full closure; seed for bootstrap/ordering only |
| offset / stage (`--root <empty>`) | prune host-satisfied — *keep current* |
| cross | dual-root scheduling (BDEPEND→BROOT `/`, RDEPEND→target) |

The emptytree fix and the broot redesign are the same move from two angles: **stop
routing emptytree through the pruning path.**

### Open questions (being resolved during implementation)

- **Cyclic deps?** Yes — the full closure reintroduces hard build cycles
  (gcc↔binutils, glibc↔gcc) that `broot_filter` used to hide. `install_order` already
  does Tarjan SCC + breaks cycles on **soft** edges; the plan is that under native
  emptytree the host-satisfied BDEPEND/DEPEND edges become **soft for ordering** (the
  host provides those tools during the rebuild, so a build tool need not be built
  before its consumer), which breaks the cycles while keeping the nodes listed. To
  verify empirically.
- **Stage1 set?** For native emptytree (ROOT=/) and offset, the seed is just **BROOT
  = host `/`** — always present, no enumeration needed. It is used for bootstrap
  version choice + cycle-breaking + (for tags) the destination VDB. The canonical
  minimal seed is the profile `@system` closure, but we only need it if we ever build
  into an empty BROOT, which we don't (BROOT is always the host).

## What emerge does

User-facing semantics (`emerge -h`):

> Reinstalls target atoms and their entire deep dependency tree, **as though no
> packages are currently installed**.

Implementation is **not** “delete the VDB”. It is a **selection policy** on the
merge target (`ROOT`), with separate rules for the build host (`BROOT`).

`create_depgraph_params` with `--emptytree`:

- `empty = True` — do not **select** installed packages as merge candidates
- `deep = True` — traverse the full dependency tree
- `selective` removed
- `bdeps = auto` by default (unless `--with-bdeps` overrides)

Core depgraph rule (`depgraph.py` ~7707):

```python
if empty and pkg.installed and not excluded:
    continue  # do not select installed packages
```

| Axis | `--emptytree` on native `/` |
|------|-----------------------------|
| **Selection** | Installed TARGET packages never chosen as merge candidates |
| **VDB read** | Still read — action tags (`N`/`R`/`U`), slot logic, display |
| **BDEPEND** | Satisfied against **BROOT** (host `/var/db/pkg`) — the stage1 seed |
| **Deep closure** | Same-version deps appear as `[ebuild R]`, not omitted |

## Not the same as `--root <empty>`

| Mode | Target VDB | Flag | Purpose |
|------|------------|------|---------|
| Stage / offset | Literally empty | *(none)* | `em -p --root <empty> @system` |
| Native reinstall | Physically full | `--emptytree` | `emerge -pe firefox` on `/` |

Commit `43f7cf6` fixed the stage case (split target vs sysroot installed
views). That is orthogonal to `--emptytree`.

## The “*stage1 bdeps” tension

“Nothing installed” applies to **ROOT** (runtime / merge target). **BROOT**
(host `/`) is always assumed to carry the bootstrap toolchain:

- `BDEPEND` / `IDEPEND` → satisfied on BROOT
- Host `sys-devel/gcc` satisfies `BDEPEND` of target `gcc` → breaks the cycle
- Without this, you cannot bootstrap gcc from literal nothing on one arch

PMS dep-class roots (see `docs/root-model.md`):

| Class | Satisfied against |
|-------|-------------------|
| `RDEPEND` | `ROOT` |
| `DEPEND` | `SYSROOT` / `ESYSROOT` |
| `BDEPEND` / `IDEPEND` | `BROOT` (always host `/`) |

## What went wrong before

### v1 (`3e051d1`)

Skipped `add_installed` entirely when `empty` — treated emptytree as “solver
sees no installed packages”. Wrong: portage still reads the VDB.

### v2 (`installed.rs` + current master)

`load_target_installed(roots, empty)` returned `Vec::new()` — broke action
tags (everything `N`), post-solve filtering (dropped same-version `R` lines),
and hid build-time dep expansion.

### v3 (`grok-broke-emptytree` — reverted)

Compensating hacks on top of v2:

1. `load_tag_installed` — split view for tags only (good idea, wrong foundation)
2. Skip `host_installed` during solve
3. Auto `with_bdeps`
4. `prepend_host_build_pretend` — post-solve fixpoint to fake host `R` lines
5. `trim_bootstrap_gcc` — ad-hoc gcc slot trimming

Symptom: `firefox --emptytree` 411 vs emerge 400 CPVs; `bash --emptytree`
missing `pkgconf`, all tags `N` instead of `R`/`U`.

## Clean model (three layers)

```
TARGET (ROOT)     selection: --emptytree ⇒ never pick installed CPVs for merge
                  satisfaction: RDEPEND/DEPEND checked against target VDB
                  display: action tags still compare against real VDB

BROOT (host /)    satisfaction: BDEPEND/IDEPEND checked against host VDB
                  stage1 assumption: gcc, cmake, perl, … already present
```

No post-solve pretend layer. The solver and post-solve passes implement the
split directly.

## Implementation plan

### 1. `InstalledPolicy::Rebuild` (`portage-atom-pubgrub`) — **done**

New policy for packages registered from the target VDB under `--emptytree`:

- `choose_version`: never return the installed version (fall through to newest
  repo candidate, like “not favored”)
- `get_dependencies`: even when selected version == installed version, return
  **full** build-time deps (not runtime-only shortcut)

### 2. `rebuild_tree` flag on `PortageDependencyProvider` — **done**

When true (native `--emptytree`):

- Skip virtual/OR “prefer installed branch” heuristics in `choose_version`
- `get_dependencies`: still uses `broot_filter` during the solve (Tier C expand
  re-adds host-satisfied build tools afterward)
- Set from depgraph: `empty && !host_config_stage && !cross.active`

### 2b. Tier C — `emptytree_expand.rs` — **done**

Post-solve fixpoint (`expand_satisfied_rebuilds`):

1. Walk **all five** PMS dep fields from every **real ebuild** in the plan
   (`BDEPEND`, `IDEPEND`, `DEPEND`, `RDEPEND`, `PDEPEND`) — category-agnostic
2. Skip only [`PortagePackage::is_virtual()`] solver-internal nodes (Choice /
   UseDecision — no md5-cache metadata). This is **not** the same as `virtual/*`
   ebuilds, which are walked like any other package
3. Re-add atoms missing from the plan but already satisfied on the correct root:
   BROOT for `BDEPEND`/`IDEPEND`, ROOT for `DEPEND`/`RDEPEND`/`PDEPEND`
4. Pick best accepted repo version; trim superseded toolchain slots

#### Expand pass pitfalls (read before editing)

| Mistake | Symptom | Correct model |
|---------|---------|---------------|
| Limit `RDEPEND`/`PDEPEND` to `virtual/*` or `app-alternatives/*` | `firefox -pe` missing perl tail (`List-MoreUtils-XS`, `File-ShareDir`, …) | Emerge recurses satisfied edges for **any** parent when `deep` is active |
| Confuse `PortagePackage::is_virtual()` with `virtual/*` ebuilds | `virtual/pkgconfig` never expands to `pkgconf` | Category `virtual/*` = real ebuild; `is_virtual()` = solver node |
| Only walk `BDEPEND`/`IDEPEND` | `po4a` → `opensp` chain missing | Also walk `DEPEND` (target-satisfied build deps) |
| Zero the target VDB under `-e` | All tags `N`, no `R` lines | Emptytree is a **selection** policy; VDB stays loaded |
| `prepend_host_build_pretend` without host gate | `rust-bin` slot fan-out | Re-add only when `Avail::atom_satisfied` on the matching root |

Regression anchors:

```bash
emerge -pe app-shells/bash   # 6 CPVs, pkgconf in closure
em -pe app-shells/bash

emerge -pe www-client/firefox
em -pe www-client/firefox
# Expect ~396/400 shared CPVs; List-MoreUtils-XS present in both
```

Reference chain that caught the RDEPEND-only-virtual bug:

```
Syntax-Keyword-Try → BDEPEND → XS-Parse-Keyword → RDEPEND → File-ShareDir
  → List-MoreUtils → PDEPEND xs? → List-MoreUtils-XS
```

### 3. `portage-cli` depgraph (`mod.rs`) — **done**

- **Stop clearing** `target_installed` on `empty` — always load real VDB
- Register target packages with `InstalledPolicy::Rebuild` when `emptytree_native`
- Pass **empty** `installed_cpvs` to the repo `Adapter` under emptytree (so
  `cede_required_use` does not skip packages being rebuilt)
- Keep **full** `target_installed_cpvs` for action tags and tree display
- Post-solve order filter: when `emptytree_native`, **keep** same-version CPVs
  in the plan (do not drop “already installed” entries)
- `solve_with_bdeps = with_bdeps || emptytree_native` (emerge `bdeps=auto`)
- `host_installed`: always wired — no skip

### 4. Builder (`main.rs`)

Unchanged: `--emptytree` disables VDB-resume skip during merge. Planner and
builder concerns stay separate.

## Verification

```bash
# Action tags and closure
emerge -pe app-shells/bash
em -pe app-shells/bash
# Expect: R on deps, U on bash, pkgconf/pkgconfig present

# Stage path unaffected
em -p --root <empty> --config-root / @system
# Expect: ~181 target CPVs (with --with-bdeps=n parity from 43f7cf6)

# Normal -p unchanged
em -p app-shells/bash
# Expect: single U line (no regression)
```

## Correctness audit (2026-06-17)

### Small / medium closures — **exact**

| Case | emerge | em | Non-toolchain diff |
|------|--------|-----|-------------------|
| `bash -pe` | 6 | 6 | 0 |
| `zlib -pe` | 1 | 1 | 0 |
| `firefox -p` (no `-e`) | 79 | 79 | 0 |

Action tags on `bash -pe` match (`R`/`U`, same old-version brackets).

### Large closures — **userspace ~99% CPV match, small toolchain gap**

`firefox -pe` on this host (Tier C + unified dep walk, 2026-06-17):

| Metric | Value |
|--------|-------|
| emerge CPVs | 400 |
| em CPVs | 401 |
| **Shared (identical CPV)** | **396 (99%)** |
| emerge-only | 4 |
| em-only | 5 |

emerge-only (toolchain / one lib): `gcc`, `binutils`, `grep`, `simdjson`.

em-only (stale bootstrap slots): `rust-1.93.1`, `autoconf`, `python-3.12`,
`libmd`, `libbsd`.

### Toolchain divergence (the entire gap)

**emerge-only** (missing from em):

- `dev-lang/rust-{1.94.0,1.95.0}` + `rust-common-1.95.0` (source rust)
- `dev-libs/oniguruma-6.9.10`
- `llvm-22` clang/compiler-rt stack (partial)
- `sys-devel/{gcc-16,binutils-2.46.1}` upgrades

**em-only** (spurious in em):

- Six `dev-lang/rust-bin-*` slots (1.74 … 1.94) — fan-out instead of one path
- Full `llvm-16` bootstrap stack (clang/llvm/compiler-rt)
- `sys-devel/gcc-11.5.0` + `binutils-2.44-r4` (old bootstrap slots)
- Old `autoconf-{2.13,2.69}` / `automake-1.17` / `python-3.12` (NS)

Host already has `gcc-16`, current `binutils`, and source `dev-lang/rust`.
Emerge upgrades the host toolchain (`U gcc-16`, `U binutils`) and builds rust
from source. em schedules parallel **bootstrap slots** (gcc-11, llvm-16,
multiple rust-bin) because `rebuild_tree` disables `broot_filter` entirely —
every BDEPEND edge in the deep closure is expanded onto ROOT, and the solver
does not collapse “host already has a newer toolchain in another slot.”

**Correctness verdict:** for `-pe`, the **userspace / perl / doc build closure
is effectively emerge-identical** after the unified dep-class expand. The
remaining gap is **toolchain upgrade scheduling** (emerge upgrades host
`gcc`/`binutils`; em keeps host versions) plus a few em-only bootstrap slots.

**Likely fix direction** (not implemented): smarter toolchain slot collapse
and host-upgrade selection under `rebuild_tree` (without reverting
`broot_filter` during solve or bringing back `prepend_host_build_pretend`).

### The `dev-lang/rust` hardcodes vs the principled rule (2026-06-18)

`emptytree_expand.rs` matches `dev-lang/rust` / `dev-lang/rust-bin` by name in
three places: `prefer_or_branch` (pick source rust for a satisfied `||`),
`trim_rust_bin_when_source_present`, `trim_superseded_rust_sources`. They exist to
reproduce emerge's host output. An attempt to replace them with the principled
**"prefer the `||` branch already in the plan"** rule (emerge `dep_zapdeps`
"already in graph") was made and reverted — it does not reach emerge **parity**
(it produces a *smaller* plan, which — see the stage3 finding below — is actually
*more correct*):

- Re-list path only (in-graph preference, both trims removed): `firefox -pe`
  **399 vs 400** — drops source `dev-lang/rust-1.95.0` (LLVM slot 22).
- Re-list + build path both in-graph: **402** — fixpoint cascade. Once any
  `rust-bin` slot enters the plan it is itself "in-plan", so the build `||`
  then prefers `rust-bin` and fans out every installed slot (1.94.0/1.94.1/1.95.0).

### Stage3 chroot — keyword confound, then the real answer (2026-06-18)

A first **stage3 chroot** (`emerge -pe firefox`, stable) looked like emerge was
over-pulling: firefox-140.11.0, **382 pkgs, slot-21 only**, no slot-22 at all. That
was a **keyword artifact** — `llvm-22`/`clang-22`/`rust-1.95` are `~arm64`, so a
*stable* stage3 cannot see them.

Re-running the same chroot with `ACCEPT_KEYWORDS="~arm64"` (the host's actual
config) **reverses it**: firefox-151.0.4, **396 pkgs**, and the closure pulls **both
slots** — `llvm-21`+`llvm-22`, `clang-21`+`clang-22`, and `dev-lang/rust-bin-1.95.0`
as **`NS`** (new slot, dependency-driven — *not* an installed re-list) with
`LLVM_SLOT="22"`. Essentially the host's 400.

**So emerge is NOT over-pulling. The slot-22 chain is a real dependency on `~arm64`.**
A clean install genuinely gets two rust slots: firefox (`RUST_NEEDS_LLVM`,
`LLVM_COMPAT` → slot 21) binds slot-21 rust, while the slotless
`|| ( >=dev-lang/rust-bin-MIN:* >=dev-lang/rust-MIN:* )` consumers (no max) resolve
to the **newest** rust = `rust-1.95` (slot 22), which drags in `llvm-22`. emerge does
**not** consolidate the slotless dep onto the in-graph slot-21 rust; it takes newest.

Corrections this forces:
- The earlier "divergence is correct / drop the orphan" plan is **wrong** — it would
  drop `rust-1.95`+`llvm-22`, which a clean `~arm64` install actually installs. The
  "399 in-graph result" was a **genuine regression**, not an improvement.
- The "orphan, zero incoming edges" in em's JSON graph is a **graph-completeness
  gap**: the expand pass adds `rust-1.95` without drawing the slotless-dep edge that
  justifies it. The edge logically exists.
- **Keep emerge parity (slot-22 included).** The `dev-lang/rust` hardcodes reproduce
  the correct clean-install behaviour; do **not** remove them by dropping slot-22.
  Any generalization must *preserve* slot-22 (slotless `||` → newest rust, prefer
  source when installed), which is exactly what the current trims approximate.

Re-validation assets: stable plan `/tmp/stage3_ff_clean_plan.txt`; `~arm64` plan
`/tmp/stage3_ff_arm64_plan.txt`; chroot at `/var/tmp/ff-stage3` (binds unmounted,
`ACCEPT_KEYWORDS="~arm64"` left in its make.conf).

## Reconsider the whole `emptytree_expand` approach (2026-06-18)

Running **em inside a clean stage3 `~arm64` chroot** (binary copied in, `sudo chroot`,
run as root — the correct way to test, not host-`em --root`) changes how we should
think about `--emptytree`:

- em's **solver alone** reproduces emerge's slot picture there — firefox-151, both
  LLVM slots, `rust-bin-1.95.0` pulled as `NS` via the slotless `>=rust:*` → newest.
  **No expand pass, no `dev-lang/rust` hardcodes were needed** (rust-1.95 is not
  installed in the chroot, so nothing re-lists it; the solver pulls it directly).
- So the current host design — *solve with `broot_filter`, then post-solve re-list
  installed packages and trim with hardcoded rust/gcc rules* — is the fragile part.
  The expand pass + trims exist only to approximate emerge on the **host**, where
  installed source rust/llvm get re-listed under emptytree. They are not the solver.

**Direction to evaluate:** implement native `--emptytree` as *"solve the target's
real dependency closure against an empty installed set, then mark every node a
rebuild"* — i.e. let the solver produce the closure (as it already does correctly in
the chroot) and derive `R`/`U`/`N` tags from the real VDB afterward, instead of
solving normally and then re-listing installed packages via `expand_satisfied_rebuilds`.
If that holds, `emptytree_expand.rs` and the rust/gcc hardcodes can be deleted.

Validation harness (reusable): clean stage3 chroot at `/var/tmp/ff-stage3`
(`ACCEPT_KEYWORDS="~arm64"`, base `arm64/23.0` profile, em copied to
`/usr/local/bin/em`). Saved comparison: `/var/tmp/em-vs-emerge-firefox-chroot/`
(`emerge_firefox_pe.txt`, `em_firefox_e.txt`, `emerge_only.txt`, `em_only.txt`).

### em-vs-emerge gap in the chroot (to analyze later)

Identical on-disk config, both `rc=0`: **emerge 396, em 371**. `em_only` is **empty**
(em is a strict subset). `emerge_only` (25) is **not toolchain** — an
`acct-group/*`+udev cluster: 15 `acct-group/*`, `virtual/udev`, `virtual/libudev`,
`sys-apps/systemd-utils`, `sys-fs/udev-init-scripts`, `sys-apps/kmod`, plus
`media-video/ffmpeg-8.1.1`, `media-libs/libass`, `app-text/scdoc`,
`dev-util/patchelf`, `app-text/docbook-xml-dtd-4.5-r2`. Looks like em misses a
udev → systemd-utils → `acct-group/*` edge (a dependency-following gap, separate
from emptytree). See also [[autounmask-convergence]].

## Open / deferred

- Host VDB version for same-slot `R` lines (not always newest repo)
- Toolchain upgrade parity (gcc/binutils/rust/llvm paths)
- `--exclude`/`-X` is wired generally now (`query/depgraph/mod.rs`, drops
  matching CPVs post-solve — a documented simplification, not a no-op like
  this bullet used to claim); whether it specifically composes correctly
  with `--emptytree`'s "excluded installed pkgs can still be providers"
  semantics is unverified — needs a targeted test, not a blanket rewrite
- `--emptytree` on cross / host-config-stage combinations