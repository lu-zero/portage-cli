# `--local` bootstrap via `package.provided`

Status: 🟡 design deepened **2026-08-07** (not implemented)  
Companion: [`docs/local-bootstrap.md`](../docs/local-bootstrap.md)  
Related: [[clang-crossbuild-prefix-local-test-plan]] Scenario B,
[[em-stages-scenario-matrix]] (`--local` KNOWN-PARTIAL),
`install-order-scc-tiebreak-fix` memory (11-node hard cycle is **expected**),
[[workdir-dual-root]] (orthogonal),
[`docs/root-topology.md`](../docs/root-topology.md) § Lifecycle (`--local` config-root footgun)

---

## Setup ladder (what `em setup --local` must eventually own)

`package.provided` is **step 4**, not the whole bootstrap. A fresh `--local`
needs a **self-consistent config root** before any merge plan is meaningful.
Today several of these are manual or only implemented for `--root`.

```text
 1. Layout skeleton          dirs, bashrc, make.conf placeholder     ✅ setup today
 2. Main repo (::gentoo)     ebuild tree the prefix can resolve      🟡 partial / gap
 3. Profile (make.profile)   ARCH/USE/keywords stack                 🟡 gap for --local
 4. package.provided seed    host tools for empty-VDB plans          🔴 not started
 5. toolchain --setup        real baselayout→…→gcc into prefix       🟡 blocked on 2–4
 6. (later) shrink provided  stop lying once prefix owns tools       🔴
```

Once steps 2–3 flip the prefix to its **own** `PORTAGE_CONFIGROOT`, host
`repos.conf` / host site profile are **no longer** in the search path
(`ReposConf::load_rooted(config_root, extra)` only sees the prefix + its
overlay dir). So **repo + profile must land together** — not “profile first,
hope host repos still apply.”

### Step 2 — Main repo (piggy-back or own)

| Host situation | Desired behaviour |
|----------------|-------------------|
| Gentoo (or any host) with a usable `::gentoo` at a known path (`repos.conf` or `/var/db/repos/gentoo`) | **Piggy-back:** write prefix `etc/portage/repos.conf/gentoo.conf` with `location = <that path>` (and optional `sync-type`/`sync-uri` copy). No clone. |
| No tree on host | **Own tree:** write the same conf pointing at e.g. `<prefix>/var/db/repos/gentoo`, then `em --local sync` (git/rsync from default mirror URI). Setup should either run sync or print the exact next command. |
| User override | `--repo PATH` / `repos.conf` already in overlay / `em setup --local --repo-location=…` (flag TBD) wins over auto-detect. |

**Already implemented pieces**

- `ReposConf::load_rooted` + `config_overlay` merge (host conf still visible
  while config root is host).
- `main_repo()` / `Cli::repo_path` fallbacks for finding a tree.
- `self_contained_prefix_entries` writes `gentoo.conf` + profile **only for
  `--root`** (`is_self_contained_root()`), not `--local`.
- `em sync` can update a tree once `repos.conf` names it.

**Gap**

- `em setup --local` writes **no** `repos.conf`.
- When config root later becomes the prefix, without a prefix `gentoo.conf`
  the main repo disappears (only defaults under prefix, which is empty).
- No “detect host tree → write location=” path for `--local`.
- No first-time clone/sync orchestration for foreign hosts (Debian/macOS).

**Policy recommendation**

1. Prefer piggy-back when a valid repo exists (read host `repos.conf` main /
   `gentoo`, else `/var/db/repos/gentoo` if it has `profiles/repo_name`).
2. Else install a **default** entry:
   ```ini
   [DEFAULT]
   main-repo = gentoo
   [gentoo]
   location = <prefix>/var/db/repos/gentoo
   sync-type = git
   sync-uri = https://github.com/gentoo-mirror/gentoo.git
   ```
   and require `em --local sync` (or `setup --sync`) before profile/provided.
3. Never invent a partial tree; md5-cache + profiles must come from a real
   checkout.

### Step 3 — Profile selection

Profile must be a real path under the **chosen** repo’s `profiles/` (from
`profiles.desc` or an explicit relative path).

| Host / intent | Default pick (when user does not override) |
|---------------|--------------------------------------------|
| Gentoo host, host `make.profile` resolves into the **same** tree we piggy-back | **Mirror host:** symlink target = host’s resolved profile (same as `--root` today). |
| Linux, no usable host profile (foreign distro, or host profile outside our tree) | **Prefix profile for host ARCH** (decided 2026-08-07): e.g. `default/linux/<arch>/<release>/no-multilib/prefix` (or newest release dir that has a prefix leaf in `profiles.desc`). Safer under EPREFIX than a plain desktop/`23.0` profile — path/relocatable assumptions match `--local`. No desktop/systemd flavor. |
| macOS / Darwin | **Prefix Darwin:** from `profiles.desc` arch `arm64-macos` / `x64-macos`, pick newest `prefix/darwin/macos/<ver>/<cpu>` (often `…/gcc`). If host macOS is **newer** than any tree entry: still pick the newest available and **warn** (decided 2026-08-07) — do not fail setup. Keyword arch is already `arm64-macos` / `x64-macos` in `gentoo-core`. |
| User override from day one | `em setup --local --profile <path-or-number>` and/or `em --local select profile set …` after setup (see select ergonomics below). |

**Already implemented pieces**

- `em select profile list|show|set` (cross-aware; any path).
- Config target for select is **only** explicit `--config-root` today (not
  `--local`) — footgun; **change planned** (below).
- Host profile symlink for **`--root`** only (`prefix_profile_entries`).

**Gaps**

| Gap | Why it hurts |
|-----|----------------|
| `setup --local` writes **no** `make.profile` | Config stays host; site provided under prefix ignored; or user must invent the select step. |
| No default profile policy for Linux/macOS foreign hosts | Cannot one-shot setup off Gentoo. |
| `em --local select profile set` does not target the prefix | Users hit host `/etc/portage` permission errors; must know `--config-root`. **Fix: honour `--local` (and `--prefix`/`--root` when explicit) as the select config target** (decided 2026-08-07). Prefer topology flags over bare host; explicit `--config-root` still wins if both are set. |
| Profile + repo ordering | Cannot set profile until repo path exists and contains `profiles/`. |

**CLI sketch (setup)**

```text
em setup --local [DIR]
  [--profile PATH|N]          # override default; relative to profiles/
  [--repo-location PATH]      # piggy-back or empty-dir destination
  [--sync]                    # after writing repos.conf, run em sync once
  [--bootstrap-profile=…]     # optional alias for preset (any-linux|macos|…)
```

Idempotent: if `make.profile` already exists and user did not pass
`--profile`, leave it (do not clobber a deliberate choice). If missing, apply
default. Same for `repos.conf` gentoo entry (`CreateOnly` / fill-gaps).

### Ordered algorithm for `setup --local`

```text
1. Skeleton + bashrc/make.conf          (existing)
2. Resolve main-repo location
     a. --repo-location if set
     b. else host main/gentoo from host repos.conf if valid tree
     c. else /var/db/repos/gentoo if valid
     d. else <prefix>/var/db/repos/gentoo + default sync-uri
3. Write <prefix>/etc/portage/repos.conf/{DEFAULT?,gentoo.conf}
4. If location empty/missing and --sync (or prompt): em sync
5. Resolve profile
     a. --profile if set (validate under repo)
     b. else host make.profile if it resolves under this repo
     c. else default for uname/ARCH (linux base / darwin prefix)
6. Write <prefix>/etc/portage/make.profile → absolute profile path
   → config root becomes prefix on next em --local invocation
7. Write managed package.provided (step 4 / Phase 1 of provided plan)
8. Print next step: em --local toolchain --setup
```

Pretend (`-p`): print all of the above, write nothing.

### What is still *after* this ladder

- Live discovery of minimal provided CPNs
- Host version probing
- `provided drop` / auto-shrink
- Full stage3 / self-hosting polish

---

## Problem

`em --local` is the **standalone** unprivileged deployment (EPREFIX =
`~/.gentoo` or `DIR`, base = target = prefix). After `em setup --local` the
layout exists, but **`em --local toolchain --setup` cannot start from an empty
VDB**: the first native toolchain steps pull a full bootstrap closure that
contains **genuine hard cycles** (python ↔ glibc ↔ gettext ↔ meson ↔
elt-patches ↔ …). That is not an install-order bug — real Gentoo never builds
from absolute zero either; it starts from a stage tarball (or Prefix’s host
tools + `package.provided`).

Confirmed 2026-08-06: fresh `--local` fails the same way for both
`toolchain --setup` and `crossdev --setup` (cross is blocked because native
bootstrap never completes). Regression matrix classifies the native cycle
outcome `KNOWN-PARTIAL`.

So: to get a toolchain into `--local`, we must tell the solver that a curated
set of packages is **already supplied by the host** for the bootstrap window.
That mechanism is **`package.provided`**.

### Why provided is stronger than “break cycles”

Under `--local`, `Roots.broot` is the **prefix itself**, not host `/`
(`cli.rs` `base_roots` / `host_roots`). Host VDB is **not** woven into
BDEPEND satisfaction the way `--prefix` dual-root does. Consequences:

| Mechanism | `--prefix` overlay | `--local` standalone |
|-----------|--------------------|----------------------|
| Target VDB | prefix (empty → grows) | prefix (empty → grows) |
| Host / BROOT VDB for BDEPEND | host `/` | **prefix** (same as target) |
| Seed for “host already has X” | host VDB auto | **`package.provided` only** |

So provided is not optional sugar for a cycle: it is the **only** Portage-native
way to claim host tools exist while broot is still the empty prefix. PATH still
runs the host `gcc`/`python` at build time; the solver must be told separately.

---

## Do we support `package.provided`?

**Yes — read path is implemented; write path is not.**

| Layer | Status |
|-------|--------|
| Profile stack read (`package.provided` lines, incremental `-cpv`) | ✅ `ProfileStack::package_provided` (`portage-repo`) |
| Site profile `/etc/portage/profile` as highest layer | ✅ `with_user_profile` |
| Solver: edges to provided CPVs dropped (never planned) | ✅ `set_provided` / `edge_is_provided` (`portage-atom-pubgrub`) |
| Host-seed: provided also `add_host_installed` + slot map | ✅ `depgraph/mod.rs` `provided_avail` |
| Preflight / BDEPEND avail seed | ✅ `record_provided`, `preflight::check` |
| **`em setup` / toolchain writes a bootstrap provided set** | ❌ not implemented |
| **Prefix owns `make.profile` so site profile is under the prefix** | ❌ gap (see below) |
| Auto-remove provided when package lands in prefix VDB | ❌ not implemented |
| Host-version probing (pick a CPV the host actually has) | ❌ not implemented |

User can already drop a hand-written
`<prefix>/etc/portage/profile/package.provided` **if** the prefix is already
the config root (see coupling). That is the correct Portage surface; we need
**curated generation + lifecycle + config-root readiness**, not a new concept.

---

## Architecture coupling (must resolve in setup, before provided matters)

### Config root flip is a triple: repos.conf + make.profile + site profile

`build_use_env` reads the profile stack from **`roots.config()`**, then folds
`{config}/etc/portage/profile` as the site layer. It does **not** read
`package.provided` from `config_overlay` (unlike `package.use` / `package.env`).

`repos_conf()` likewise loads from **config root** (+ overlay). For `--local`,
overlay is the same `prefix/etc/portage` — so once config is the prefix, **only
prefix (and empty defaults under it)** supply `::gentoo`. Host
`/etc/portage/repos.conf` drops out of the search path.

```text
make.profile under prefix?   config root     package.provided from     ::gentoo from
---------------------------  --------------  ------------------------  -------------------
no                           host `/`        /etc/portage/profile/…    host repos.conf
yes                          the prefix      <prefix>/…/profile/…      <prefix> repos.conf only
```

(`cli.rs` `TopologySource::Local`: prefer prefix config when
`prefix/etc/portage/make.profile` exists.)

**Implication:** writing `$PREFIX/etc/portage/profile/package.provided` alone is
a no-op until `make.profile` exists; and creating `make.profile` without
`repos.conf` breaks ebuild resolution on the next run. Setup must write
**repo + profile + provided** as one unit (or refuse to flip config mid-way).

Today `em setup --local` only does skeleton + bashrc/make.conf.
`self_contained_prefix_entries` does repo+profile for **`--root` only**.

### Decision: setup owns the full ladder (steps 2–4)

**`em setup --local` must:**

1. Resolve and record main-repo location (piggy-back or own + sync).
2. Choose and link `make.profile` (default policy or `--profile`).
3. Ensure `etc/portage/profile/` and write managed `package.provided`.

Alternative considered and **rejected for v1**: load `package.provided` from
`config_overlay` while config is still host. That papers over missing
make.profile and diverges from Portage. Prefer a real prefix config root
early (`C=~/.gentoo/etc/portage`).

### Pretend (`-p`)

Under `-p`, print repo location, profile choice, provided block; write nothing.

---

## Design intent

```text
host tools (compiler, libc, python, …) on PATH
        │
        ▼
em setup --local
   make.profile → host profile
   package.provided  (managed block)  ──►  solver treats as installed, never builds
        │
        ▼
em --local toolchain --setup   (small plan, no irreducible cycle)
   baselayout → binutils → headers → libc(--nodeps) → gcc
        │
        ▼
prefix VDB grows (real packages)
        │
        ▼
shrink / drop provided entries  (manual first; auto later)
        │
        ▼
self-hosting --local  (BROOT effectively the prefix + real VDB)
```

`package.provided` is **bootstrap scaffolding**, not a permanent lie that the
prefix owns those packages. Long-term the prefix should own toolchain bits it
cares about; provided only bridges the gap until then.

---

## Native plan shape (what provided must enable)

`stages::toolchain_plan(Native, self_contained=true)`:

| Step | Atom | Notes |
|------|------|--------|
| baselayout | `sys-apps/baselayout` USE=build | FS skeleton |
| binutils | `sys-devel/binutils` USE=-debuginfod | avoid elfutils explosion |
| kernel headers | `sys-kernel/linux-headers` headers-only | or musl equiv |
| libc | `sys-libs/glibc` (or musl) **--nodeps** | cycle break vs gcc |
| gcc | `sys-devel/gcc` (GCC_DISABLE flags) | full compiler into prefix |

Each step still **resolves a depgraph**. `--nodeps` only skips *runtime*
DEPEND for that step’s merge; BDEPEND and earlier packages’ closures still
expand. Live 11-node cycle:

```text
gdbm ↔ elt-patches ↔ meson ↔ gettext ↔ glibc[cet] ↔ python ↔ …
```

Provided must cover enough of that expansion that preflight + install-order
SCC analysis see no hard cycle. Exact CPN set is an **empirical floor**, not
a theoretical closed form — grow from live `-p` failures.

Also: native plan uses `root_deps=true` and
`with_target_only_installed_view` so host VDB does not fake “already in
prefix.” Provided is independent of that view (edges dropped before plan
membership).

---

## Resolved decisions (2026-08-07)

### 1. Floor versions when host has no VDB

**Policy: tree-present floor, not a magic sentinel.**

1. Prefer host VDB CPV when `/var/db/pkg/<cpn>-*` exists (Gentoo host).
2. Else map host probe (`gcc --version`, `python3 -V`, …) to a **repo
   version ≤ host major.minor when possible**, else the **oldest stable**
   version of that CPN still in the active tree that satisfies typical
   `>=` deps (conservative under-estimate beats over-estimate that no
   ebuild accepts).
3. Never use `cat/pkg-0` or invented versions absent from the tree —
   `same_slot_series` / keyword filtering need a real repo match for slot
   mapping; missing tree version → slotless provided (still satisfies
   unslotted edges, fails `:3.13`-style deps).

Ship a small **built-in floor table** (data) as fallback when probing fails,
updated when the matrix finds a bad floor. Document that floors are
“good enough for bootstrap,” not host fingerprints.

### 2. Provide host libc on Linux?

**Yes for v1 bootstrap set.** Prefix tradition and the native plan both need
libc edges satisfied without re-planning a full glibc+python world before
any merge. Providing `sys-libs/glibc` (or musl) means the **first** real
glibc merge only happens when the operator drops the provided line (or
after auto-drop once a real glibc is in the prefix VDB).

Document clearly: “your prefix does not own libc until you drop provided.”
Optional later flag: `--bootstrap-provide=minimal` vs `full` (full includes
libc/compiler; minimal only build tools — may still cycle).

### 3. `with_bdeps` / InstalledPolicy under `--local`

No dual-root weave. Provided + PATH only. Do **not** auto-write provided
for `--prefix` (host VDB already answers). Optionally share the same write
path for bare `--root DIR` self-contained bootstraps (empty VDB, same
class) — same code, gate on `is_standalone_prefix || is_self_contained_root`.

### 4. macOS

**Data-only after any-linux is green.** Same write machinery; different CPN
table (no glibc; clang/Xcode; Darwin profile). Do not block Linux on Darwin
presets.

### 5. Provide host compiler or rebuild immediately?

**Provide `sys-devel/gcc` (or clang) in the seed**, then let
`toolchain --setup`’s gcc step still **want** to install into the prefix:
provided satisfies *dependency edges to* gcc from other packages, but a
**named root atom** `sys-devel/gcc` in the stage plan still selects a merge
unless selective/`--noreplace` treats provided as installed.

**Verify in implementation:** does `set_provided` alone stop a root atom
from planning, or only transitive edges? Today provided drops **dependency
edges** in `get_dependencies`; roots are still solved. If a provided CPV
matches a root, PubGrub may still pick it as “already satisfied” via
host_installed seed (`add_host_installed` for provided). Confirm with a
unit test: root atom that is provided → empty plan step / no rebuild
unless forced.

Desired product behaviour for `toolchain --setup`:

- Transitive deps on host gcc/python/meson → provided, not built.
- Stage steps that *are* the bootstrap product (binutils, headers, libc,
  gcc) → **must still merge into the prefix** even if a provided line names
  the same CPN.

That implies either:

- **(A)** Do not put stage-product CPNs in provided (only their *build*
  deps: python, meson, make, …), **or**
- **(B)** Put them in provided for early steps but remove those lines
  before the step that builds them, **or**
- **(C)** Stage driver forces rebuild of plan atoms regardless of provided
  (InstalledPolicy::Rebuild for stage roots).

**Recommendation: (A) for v1.** Smallest mental model:

| In provided (host supplies for bootstrap) | Not in provided (prefix must build) |
|-------------------------------------------|-------------------------------------|
| python, perl, meson, ninja, cmake, make, m4, autoconf, automake, libtool, patch, coreutils, sed, grep, gawk, findutils, file, xz-utils, zstd, elt-patches, gettext, … | baselayout, binutils, linux-headers, glibc/musl, gcc (stage products) |
| **Optional / policy** host libc headers? | libc **merge** is a stage product — if we provide full glibc CPV, stage libc step may no-op |

Tension with open decision 2: if glibc is provided, the libc stage step may
skip. That is **acceptable for a first “get a compiler into the prefix”
milestone only if** the host glibc ABI is fine to link against via
SYSROOT/PATH — but `--local` wants packages *installed into* the prefix.
So:

**Refined: do not provide stage-product CPNs.** Provide the **cycle fuel**
around them (python/meson/gettext/elt-patches/…). For glibc’s own BDEPEND
explosion, rely on `--nodeps` on the libc step (already in the plan) **plus**
provided for tools glibc would pull. If live still cycles, add the next
missing **tool** CPN, not glibc itself, until proven necessary.

If live proves glibc must be provided to break a residual cycle, document
that as a temporary lie and schedule auto-drop after the first real glibc
merge (or force Rebuild on the libc stage step).

---

## Baseline provided set (data — still draft)

Version-agnostic **policy CPNs**; versions filled at setup (probe / floor).

### Tier 1 — cycle fuel (must for any-linux v1)

Aim: break the known 11-node SCC and common BDEPEND chains for binutils /
headers / glibc / gcc.

| Kind | CPN | Why |
|------|-----|-----|
| Scripting | `dev-lang/python` (one slot) | glibc/python edges |
| Scripting | `dev-lang/perl` | linux-headers / many |
| Build | `dev-build/meson`, `dev-build/ninja`, `dev-build/cmake` | meson↔gettext |
| Build | `sys-devel/make`, `dev-build/autoconf`, `dev-build/automake`, `sys-devel/m4`, `sys-devel/libtool` | configure |
| Glue | `app-portage/elt-patches`, `app-arch/xz-utils`, `app-arch/zstd` | elt-patches↔xz |
| i18n | `sys-devel/gettext` | cycle participant |
| Base utils | `sys-apps/coreutils`, `findutils`, `gawk`, `grep`, `sed`, `file`, `sys-devel/patch` | scripts |
| Compress | `app-arch/bzip2`, `app-arch/gzip`, `app-arch/tar` | unpack |

### Tier 2 — add only if live `-p` still fails

| CPN | When |
|-----|------|
| `sys-libs/zlib`, `sys-libs/ncurses`, `dev-libs/libffi`, … | next unsatisfied BDEPEND |
| `dev-lang/python-exec`, `dev-python/*` seeds | python ecosystem edges |
| `sys-devel/binutils-config`, `sys-devel/gcc-config` | if tools assume them provided |
| `virtual/*` | **avoid** — provided is CPV, not virtual; satisfy via real package |

### Explicitly **out** of provided (stage products)

`sys-apps/baselayout`, `sys-devel/binutils`, `sys-kernel/linux-headers`,
`sys-libs/glibc` / musl, `sys-devel/gcc` — unless live forces a temporary
exception (then document + Rebuild).

### macOS / Darwin (later)

| Kind | Notes |
|------|-------|
| Compiler | host clang — map carefully or PATH-only |
| No glibc | never provide `sys-libs/glibc` |
| Headers | Xcode SDK; different package set |
| Python | host python3 until prefix owns one |

Do **not** invent a Prefix-complete list on day one. Start with Tier 1, run
`em -p --local DIR toolchain --setup`, add the next missing edge until
cycle-free, then freeze that set as the any-linux default.

---

## Managed block format

File: `<EROOT>/etc/portage/profile/package.provided`

```text
# BEGIN em-bootstrap-provided
# generated-by: em setup
# preset: any-linux
# regenerated: do not hand-edit inside this block
sys-apps/coreutils-9.6
dev-lang/python-3.13.0
# …
# END em-bootstrap-provided

# user lines outside the block are preserved
```

- Rewrite only the `BEGIN`…`END` region on `setup` re-run / `provided sync`.
- Preserve comments and CPVs outside the markers.
- Leading `=` tolerated by parser; we write bare `cat/pkg-ver`.
- Incremental `-cpv` removal is stack-level; inside one file prefer
  rewrite-of-block over `-` lines for managed entries.

---

## Implementation plan (phased)

### Phase 0 — Design docs ✅ / 🟡

- [x] Initial plan + [`docs/local-bootstrap.md`](../docs/local-bootstrap.md)
- [x] PENDING.md queue entry
- [x] Architecture coupling + resolved decisions
- [x] Setup ladder: repo + profile + provided
- [x] Product picks: Linux → prefix profile; select honours `--local`; Darwin newest + warn

### Phase 1a — Repo + profile (config-root readiness)

**Goal:** after `em setup --local`, the prefix is a complete
`PORTAGE_CONFIGROOT`: own `repos.conf` + `make.profile`, so the next
`em --local …` does not silently use host config.

1. Detect main-repo location (piggy-back host tree, else prefix path + sync URI).
2. Write `etc/portage/repos.conf` (DEFAULT main-repo + gentoo entry).
3. Optional `--sync` / document `em --local sync` when tree missing.
4. Default profile policy + `--profile` override; write `make.profile`.
5. **`em select profile` + topology flags:** when `--local` / `--prefix` /
   `--root DIR` is set, target that tree’s `etc/portage` (same as setup), not
   host `/`. Explicit `--config-root` still wins. Keeps eselect parity for
   *bare* `em select` (host only) without ignoring an explicit topology.
6. Unit tests: piggy-back path; missing-tree conf; Linux default is a
   **prefix** profile path; Darwin newest-with-warning; re-run does not
   clobber existing make.profile without `--profile`; `em --local DIR select
   profile show` reads the prefix link.

**Acceptance:** on a Gentoo host, `em setup --local DIR` alone leaves
`DIR/etc/portage/{repos.conf,make.profile}` such that
`em --local DIR -p sys-libs/zlib` resolves against the prefix config (not
host) and finds `::gentoo`.

### Phase 1b — Managed `package.provided` write path

**Goal:** same setup also seeds bootstrap provided (static Tier-1 floors).

1. `mkdir` `etc/portage/profile`.
2. Write managed provided block from **static Tier-1 floors** (no probe yet).
3. Idempotent re-run; pretend-safe; `--prefix` does **not** write provided.
4. Unit tests: provided non-empty in `UseEnv` for setup’d fixture.

**Acceptance:** `em -p --local DIR toolchain --setup` progresses past
“empty VDB hard cycle” on a Gentoo host (may still need Phase 2/3 for
versions / exact CPNs).

### Phase 2 — Host CPV probing (any-linux)

1. Gentoo: read `/var/db/pkg` for each policy CPN (newest matching slot).
2. Non-Gentoo: version probes + floor table.
3. Record provenance in comments (`# host-vdb: …` / `# floor: …`).
4. Integration test with fake host VDB fixture.

### Phase 3 — Discovery loop (live, not pure unit)

1. On a disposable prefix: setup → `-p toolchain --setup`.
2. If hard cycle / unsatisfied: identify CPNs in the SCC or `needs:` lines;
   promote to Tier 1; re-run.
3. Freeze the winning set in code + docs; add a regression test that
   synthetic graph + provided is cycle-free.
4. Optional: `em --local toolchain --setup -p` hint when provided empty
   (“run em setup --local or seed package.provided — see local-bootstrap.md”).

### Phase 4 — Lifecycle (optional but important)

| Command / flag | Behaviour |
|----------------|-----------|
| `em setup --local` | layout + make.profile + bootstrap provided |
| `em --local provided sync` | re-probe / refresh managed block |
| `em --local provided drop --built` | remove managed lines whose CPN is now in prefix VDB |
| (later) auto-drop after merge | when cpv installs, strip matching provided |

Until auto-drop: document “shrink after toolchain lands or upgrades look
already satisfied forever.”

### Phase 5 — Presets + matrix

| Preset | Detection | Provided policy |
|--------|-----------|-----------------|
| `any-linux` (default) | Linux + host cc | Tier 1 + discovery |
| `macos` | Darwin | Darwin table (later) |
| `gentoo-host` | `/var/db/pkg` | smaller set optional; prefer `--prefix` for delta |

Live matrix: Gentoo `--local`, Debian/Fedora floors, `--prefix` control (no
auto provided), macOS later. Re-open Scenario B in the clang crossbuild plan
once native bootstrap works.

---

## Module sketch (where code goes)

| Piece | Crate / file |
|-------|----------------|
| Managed-block parse/rewrite | `portage-cli/src/setup.rs` or new `setup/provided.rs` |
| Policy CPN lists + floors | `portage-cli/src/setup/bootstrap_provided.rs` (data) |
| Host VDB probe | thin read of `portage-vdb` or existing installed helpers |
| make.profile for `--local` | `setup::bootstrap` (reuse crossdev symlink logic carefully) |
| CLI `provided sync/drop` | `cli.rs` + small applet (Phase 4) |
| Tests | `setup.rs` unit + `portage-cli/tests/` integration with tmp prefix |

No solver changes expected if read path already works; if root atoms that
are provided incorrectly no-op stage products, fix in stage driver
(InstalledPolicy::Rebuild for stage roots) — do not weaken provided
globally.

---

## Non-goals

- Replacing stage tarballs for full stage3 production
- Making `--local` as cheap as `--prefix` on a fat Gentoo host
- Permanent provided of `@system`
- Fixing workdir dual-root race — [[workdir-dual-root]]
- Inventing a second “host tools” concept beside `package.provided`

---

## Resolved product picks (2026-08-07)

| # | Decision |
|---|----------|
| Linux default profile | **Prefix profile** (`…/no-multilib/prefix` or equivalent in tree) — safer under EPREFIX than plain `default/linux/…/23.0`. |
| `em select profile` config target | Honour **`--local` / `--prefix` / `--root`** (explicit topology); `--config-root` still wins if set. Bare `em select` → host only (eselect parity). |
| macOS profile newer than tree | Pick **newest available** Darwin/prefix profile for the arch; **warn** that host OS is ahead of the tree (do not fail). |

## Open questions still needing live data

1. **Minimal Tier-1 provided membership** after first successful `-p`.
2. Whether **glibc must be provided** despite the stage-product rule.
3. **Python slot pinning** host vs tree.
4. Exact **profiles.desc** lookup helper (newest release dir that still has a
   `…/prefix` leaf for this ARCH; Darwin version sort).
5. Interaction with **emptytree / stages** after toolchain (shrink provided
   first).

---

## Suggested commit series (when implementing)

```text
docs: --local setup ladder (repo + profile + provided)
feat(setup): --local repos.conf (piggy-back host ::gentoo or own + sync-uri)
feat(setup): --local make.profile default + --profile override
feat(setup): write managed bootstrap package.provided for --local
feat(setup): host CPV probe for any-linux provided set
test: setup --local is a complete config root; provided breaks hard-cycle fixture
feat(cli): em provided sync/drop (optional follow-up)
```

---

## Implementation readiness

| Ready to code without more design | Needs live discovery first |
|-----------------------------------|----------------------------|
| Piggy-back host `::gentoo` into prefix repos.conf | Exact Tier-1 provided CPNs |
| Own-tree repos.conf + sync-uri template | glibc-in-provided exception |
| Linux → prefix profile default; Darwin → newest + warn | Auto-drop heuristics |
| `select profile` respects `--local`/`--prefix`/`--root` | |
| make.profile link once path chosen | |
| Managed provided block rewrite | |
| Static floor write path | |
| Unit tests: config-root flip needs both repo+profile | |
| `--prefix` does not auto-write provided | |

**Suggested first PR:** Phase **1a** only (repos.conf + make.profile +
prefix-profile default + select-topology + tests).
**Second PR:** Phase **1b** provided block. Then live `-p` for Tier-1 freeze.

---

## References

- portage(5) `package.provided`
- `portage-repo` `ProfileStack::package_provided`
- `portage-atom-pubgrub` `set_provided` / `edge_is_provided`
- `portage-cli` `query/depgraph` provided + `add_host_installed`
- `cli.rs` Local config root when `make.profile` exists
- Gentoo Prefix bootstrap (host tools + provided) — conceptual analogue
- Live: [[clang-crossbuild-prefix-local-test-plan]] Scenario B
- Cycle: `todo/dedup-availability-walks.md`, PENDING hard-cycle notes
