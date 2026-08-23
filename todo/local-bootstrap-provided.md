# `--local` bootstrap via `package.provided`

Status: 🟢 Phase 1a + 1b landed **2026-08-09** (repo/profile/provided ladder
for `em --local setup`); **the empty-VDB hard cycle is confirmed cleared,
live-verified 2026-08-23** (see Phase 1b/3 below) — `--profile` CLI override
and Darwin policy are the remaining open pieces.  
Companion: [`docs/local-bootstrap.md`](./local-bootstrap.md)  
Related: [[clang-crossbuild-prefix-local-test-plan]] Scenario B,
[[em-stages-scenario-matrix]] (`--local` KNOWN-PARTIAL),
`install-order-scc-tiebreak-fix` memory (11-node hard cycle is **expected**),
[[workdir-dual-root]] (orthogonal),
[`docs/root-topology.md`](../docs/design/root-topology.md) § Lifecycle (`--local` config-root footgun)

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
- **Already fixed (2026-06-23, `7a8c5bc`), not a pending change**: `em select`
  resolves `--config-root` first, else the `--local`/`--prefix` overlay, else
  host `/` (`config_portage_dir_for()` in `select/mod.rs`). This section used
  to describe it as still-planned; corrected 2026-08-09 after live-verifying
  `em --local DIR select profile show` targets the prefix. Only a bare
  `--root` (no `--config-root`/`--local`/`--prefix`) still falls back to host,
  matching real `eselect` on purpose.
- Host profile symlink for **`--root`** only (`prefix_profile_entries`).

**Gaps**

| Gap | Why it hurts |
|-----|----------------|
| `setup --local` writes **no** `make.profile` | Config stays host; site provided under prefix ignored; or user must invent the select step. |
| No default profile policy for Linux/macOS foreign hosts | Cannot one-shot setup off Gentoo. |
| ~~`em --local select profile set` does not target the prefix~~ | **Already fixed** (2026-06-23, `7a8c5bc`) — see above. The remaining real gap is only that `setup --local` writes no `make.profile` for `select` to find, not that `select` ignores `--local`. |
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

### Explicitly **out** of provided (stage products) — with a live exception

`sys-apps/baselayout`, `sys-devel/binutils`, `sys-devel/gcc` stay out.
**`sys-kernel/linux-headers` and `sys-libs/glibc` are the "unless live forces
a temporary exception" case this bullet already flagged as possible** —
confirmed 2026-08-09/2026-08-23: without them, `sys-devel/gcc`'s
`elibc_glibc?` RDEPEND still pulls a from-scratch glibc build regardless of
the dedicated libc step, reintroducing the real cycle. Both are in `TIER1`
(`setup/provided.rs`) today. Documented as the temporary lie this bullet
already said to write down; **not yet Rebuild-forced** — see the Phase
1b/2026-08-23 acceptance note below for the open decision.

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

- [x] Initial plan + [`docs/local-bootstrap.md`](./local-bootstrap.md)
- [x] PENDING.md queue entry
- [x] Architecture coupling + resolved decisions
- [x] Setup ladder: repo + profile + provided
- [x] Product picks: Linux → prefix profile; select honours `--local`; Darwin newest + warn

### Phase 1a — Repo + profile (config-root readiness) ✅ landed 2026-08-09

**Goal:** after `em setup --local`, the prefix is a complete
`PORTAGE_CONFIGROOT`: own `repos.conf` + `make.profile`, so the next
`em --local …` does not silently use host config.

1. [x] Detect main-repo location (piggy-back host tree, else prefix path +
   sync URI) — `setup/repo.rs::resolve_repo_path`/`detect_host_gentoo`.
2. [x] Write `etc/portage/repos.conf` (`gentoo.conf`, `[DEFAULT] main-repo`
   for the own-tree case).
3. [x] Auto-sync (not just `--sync`/documented next-step): `em setup --local`
   runs `em sync gentoo` itself when the resolved tree has no
   `profiles/repo_name` yet — one command gets a working tree, shallow
   (`--depth 1`, the git sync backend's existing default).
4. [x] Default profile policy (ARCH-filtered `.../prefix` leaf, newest
   release, `split-usr`/`kernel-N+` variants excluded) + host-mirror when a
   real Gentoo host's own `make.profile` resolves under the synced tree;
   write `make.profile`. **No `--profile` CLI override yet** — not
   implemented, still a gap if you need to pick a non-default profile.
5. ~~**`em select profile` + topology flags**~~ — **already done** (`7a8c5bc`,
   2026-06-23), no work needed here. `--local`/`--prefix` target that tree's
   `etc/portage`; bare `--root` still falls back to host, matching `eselect`.
6. [x] Unit tests: piggy-back vs. own-tree (fixture host `repos.conf`/no
   `repos.conf`); host-profile-mirror vs. ARCH-default (fixture
   `profiles.desc`); re-run does not clobber an existing `gentoo.conf` or
   `make.profile` (including a dangling-symlink idempotency fix found while
   writing these tests — see `local_profile.rs`'s `ensure_profile` doc
   comment). **Not done**: Darwin newest-with-warning (macOS profile policy
   is still Phase 5/later, untouched); a live `em --local DIR select profile
   show` integration test (verified manually instead, see Acceptance).

**Acceptance:** live-verified 2026-08-09 on this (Gentoo, arm64) host: a
fresh `em --local DIR setup` piggy-backs `/var/db/repos/gentoo`, mirrors the
host's own `default/linux/arm64/23.0` profile, and a re-run is a clean no-op
(only the baselayout oneshot re-merges, no repo/profile/provided writes) —
i.e. `DIR/etc/portage/{repos.conf,make.profile}` end up such that later
`em --local DIR` invocations resolve against the prefix config, not host.
The "no host `::gentoo`/profile at all" (e.g. Debian) branch is covered by
the fixture tests above, not live-verified against a real non-Gentoo host.

### Phase 1b — Managed `package.provided` write path ✅ landed 2026-08-09

**Goal:** same setup also seeds bootstrap provided.

1. [x] `mkdir` `etc/portage/profile`.
2. [x] Write managed provided block — **not** static Tier-1 floors (Phase
   2's original scope): folded host-probing straight into this phase per
   product decision 2026-08-09 (`setup/provided.rs`). Each Tier-1 CPN's
   version comes from probing the host's own tool (`gcc --version`,
   `python3 -V`, …, best-effort first-PMS-version-token extraction) and
   picking the closest tree-present version `<=` that, falling back to the
   oldest tree version when the probe misses — resolved from the **just-
   synced** tree (Phase 1a), not a hardcoded table baked into the binary.
3. [x] Idempotent re-run (block is recomputed and only rewritten on content
   change; hand-written lines outside the `BEGIN`/`END` markers preserved);
   pretend-safe (whole ladder is skipped under `-p`, matching the rest of
   `setup::run`); `--prefix` does **not** write provided (gated on
   `ActiveKind::Local`, `--prefix` registers as `ActiveKind::Prefix`).
4. [x] Unit tests: version-token extraction across real-world `--version`
   banner shapes; closest-`<=`-host vs. oldest-fallback picking; managed-
   block rewrite preserves surrounding hand-written lines.

**Acceptance:** live-verified 2026-08-09: `em --local DIR setup` on this host
wrote 24 Tier-1 entries with real probed-then-tree-mapped versions (e.g.
host `python3` 3.13.12 → tree `dev-lang/python-3.12.9999`, the correct
closest-`<=`-host match once `9999`-suffixed live-ebuild version ordering is
accounted for).

**2026-08-23: the real remaining acceptance test — confirmed, live.** Fresh
disposable prefix, `em --local DIR setup` (piggy-backed host `::gentoo`,
mirrored `default/linux/arm64/23.0`, wrote 25 provided entries — one more
than the Aug-9 pass since `TIER1` grew `sys-libs/glibc` and
`sys-kernel/linux-headers` in the meantime, see the caveat below) →
`em -p --local DIR toolchain --setup`: **`EXIT=0`**, all 6
`toolchain_plan` steps (baselayout, python, binutils, kernel headers, libc,
gcc) resolve with no hard-cycle error, no REQUIRED_USE die, no `!!!`
banners, 383 merge-preview lines total, ending at `sys-devel/gcc-15.3.0`.
The empty-VDB hard cycle this whole phase exists to clear is confirmed
cleared by the current Tier-1 set — no further CPNs needed for this host.

**Caveat found in the same pass, not a bug — a real, undocumented tension
this file's own "Refined" text (Tier-1 table section) predicted:**
`sys-libs/glibc` and `sys-kernel/linux-headers` are both in the generated
provided block (confirmed: `cat DIR/etc/portage/profile/package.provided`),
despite the design table above still listing them under "Explicitly out of
provided (stage products)". This is not an oversight — `setup/provided.rs`
(`TIER1`, `sys-libs/glibc` entry, comment at line ~125) documents finding
live that skipping the dedicated libc step under `prefix-guest` does
nothing to stop `sys-devel/gcc`'s own `elibc_glibc? ( sys-libs/glibc[...] )`
RDEPEND edge from pulling a full from-scratch glibc build anyway, hitting
the exact bootstrap cycle prefix-guest exists to avoid (glibc BDEPENDs on a
gcc that doesn't exist yet). Providing glibc was the fix, and today's run
confirms it works. **Consequence, not yet addressed:** the `[5/6] libc` step
plans *nothing* (confirmed: empty package list in the log) — a `--local`
prefix built this way never actually owns its own glibc; it stays a
"temporary lie" indefinitely, exactly the scenario this file's Phase 3/4
text already anticipated ("if live proves glibc must be provided … document
that as a temporary lie and schedule auto-drop … or force Rebuild on the
libc stage step") but never got a resolution once it actually happened.

**Resolved, same day, landed:** not a `--rebuild`-style opt-in flag, and
not a staged-driver-specific `Rebuild` override — `package.provided` was
architecturally the wrong mechanism (a separate dependency-edge-deletion
filter that also deleted the synthetic solver root's own edges, so an
*explicit* target atom naming a provided CPN — a plain `em sys-libs/glibc`,
or the libc step's own atom — silently vanished from the plan instead of
being solved; a second, independent bug: the deletion never checked
whether the provided version even satisfied the constraint that reached
it). Fixed at the actual root: `package.provided` now registers each CPV
through the same `add_installed` pipeline real VDB-sourced installed
packages use, under a new `InstalledPolicy::Provided` (same
version-selection as `Favor`, but — since there's no real VDB record
behind it — its own dependencies are never explored when kept, only when
forced by a real build). `set_provided`/`edge_is_provided` are deleted
outright; no CLI surface added. Live-verified: the `[5/6] libc` step now
plans and (confirmed via a real, non-pretend `--nodeps sys-libs/glibc`
merge) actually builds a real glibc, while every other step's plan is
byte-for-byte unchanged from before (diffed against this session's own
pre-fix baseline — the only difference across the whole 6-step `-p`
sequence is the one new, correct `sys-libs/glibc` line). The "permanent
lie" this note worried about no longer exists: once glibc is genuinely
installed, the plan-membership filter already prefers the real VDB entry
over the synthetic provided one for free, no file rewrite needed.

### Phase 2 — Host CPV probing (any-linux) — merged into Phase 1b above

Originally scoped as a separate later phase reading `/var/db/pkg` (Gentoo)
or version probes + a static floor table (non-Gentoo). Landed 2026-08-09 as
part of Phase 1b instead, host-tool-probe-only (no `/var/db/pkg` read path —
`--local` never weaves host VDB into BDEPEND satisfaction in the first
place, see "Why provided is stronger than break cycles" above, so a VDB
read wouldn't be authoritative here anyway). Provenance comments
(`# host-vdb: …` / `# floor: …`) per line were **not** added — the managed
block is regenerated wholesale each run instead, so provenance would go
stale immediately; worth reconsidering if per-line provenance turns out to
matter for debugging a bad pick.

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
| make.profile link once path chosen | |
| Managed provided block rewrite | |
| Static floor write path | |
| Unit tests: config-root flip needs both repo+profile | |
| `--prefix` does not auto-write provided | |

(`select profile` respecting `--local`/`--prefix`/`--root` was in this table
as not-yet-done; it's already shipped, see Step 2/Step 3 sections above.)

~~**Suggested first PR:** Phase **1a** only... **Second PR:** Phase **1b**...~~
— **superseded**: both landed together 2026-08-09 (`portage-cli/src/setup/{repo,local_profile,provided}.rs`).
**Next up:** live `-p --local DIR toolchain --setup` against the freshly-provided
prefix to check whether the current Tier-1 set actually clears the empty-VDB
hard cycle, or needs another round (Phase 3's original discovery loop).

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
