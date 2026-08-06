# `--local` bootstrap via `package.provided`

Status: 🔴 not started · Design drafted **2026-08-06**  
Companion: [`docs/local-bootstrap.md`](../docs/local-bootstrap.md)  
Related live findings: [[clang-crossbuild-prefix-local-test-plan]],
[[em-stages-scenario-matrix]] (`--local` bootstrap failures),
`install-order-scc-tiebreak-fix` memory (11-node hard cycle is **expected**)

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

---

## Do we support `package.provided`?

**Yes — read path is implemented; write path is not.**

| Layer | Status |
|-------|--------|
| Profile stack read (`package.provided` lines, incremental `-cpv`) | ✅ `ProfileStack::package_provided` (`portage-repo`) |
| Site profile `/etc/portage/profile` as highest layer | ✅ `with_user_profile` |
| Solver: edges to provided CPVs dropped (never planned) | ✅ `set_provided` / `edge_is_provided` (`portage-atom-pubgrub`) |
| Preflight / BDEPEND avail seed | ✅ `record_provided`, `preflight::check` |
| Depgraph outcome carries provided + slot map | ✅ |
| **`em setup` / toolchain writes a bootstrap provided set** | ❌ not implemented |
| Auto-remove provided when package lands in prefix VDB | ❌ not implemented |
| Host-version probing (pick a CPV the host actually has) | ❌ not implemented |

User can already drop a hand-written
`<prefix>/etc/portage/profile/package.provided` and `em` will honour it. That
is the correct Portage surface; we need **curated generation + lifecycle**,
not a new concept.

---

## Design intent

```text
host tools (compiler, libc, python, …)
        │
        ▼
package.provided  ──►  solver treats as installed, never builds
        │
        ▼
em --local toolchain --setup   (small plan, no irreducible cycle)
        │
        ▼
prefix VDB grows (real packages)
        │
        ▼
shrink / drop provided entries  (optional phases)
        │
        ▼
self-hosting --local  (BROOT effectively the prefix)
```

`package.provided` is **bootstrap scaffolding**, not a permanent lie that the
prefix owns those packages. Long-term the prefix should own toolchain bits it
cares about; provided only bridges the gap until then.

---

## Implementation plan

### Step 0 — Document ✅

- [x] This plan + [`docs/local-bootstrap.md`](../docs/local-bootstrap.md)
- [x] PENDING.md queue entry
- [x] Cross-link from root-topology

### Step 1 — Baseline provided sets (data)

Define **version-agnostic policies** that expand to concrete CPVs at setup
time (probe host or use a safe floor version).

**Minimal “compiler + core userland” set** (any-linux / glibc or musl host):

Aim: enough that `toolchain --setup`’s first atoms (binutils → headers → gcc
stages → libc) do not re-pull python/meson/glibc into a hard cycle.

Draft membership (refine live against a failing plan):

| Kind | Examples (CPN) | Notes |
|------|----------------|-------|
| Toolchain | `sys-devel/gcc`, `sys-devel/binutils`, `sys-libs/glibc` *or* musl, `sys-kernel/linux-headers` | Host compiler identity; version = host’s where known |
| Build tools | `dev-build/meson`, `dev-build/ninja`, `dev-build/cmake`, `sys-devel/make`, `dev-build/autoconf`, `dev-build/automake`, `sys-devel/m4`, `dev-build/libtool` | Break meson/gettext cycles |
| Scripting | `dev-lang/python` (one slot), often `dev-lang/perl` | glibc/python edges |
| Archivers / bootstrap glue | `app-arch/xz-utils`, `app-portage/elt-patches`, `app-arch/zstd`, `sys-apps/ whatexist` | elt-patches ↔ xz cycle |
| Base utils | `sys-apps/coreutils`, `sys-apps/findutils`, `sys-apps/gawk`, `sys-apps/grep`, `sys-apps/sed`, `sys-devel/patch`, `sys-apps/file` | configure/scripts |

**macOS / Darwin** (streamline later, same machinery):

| Kind | Notes |
|------|-------|
| Compiler | host clang (may map to `sys-devel/clang` or leave unprovided and rely on PATH — open) |
| No glibc | libc is the system; never provide `sys-libs/glibc` |
| Headers | Xcode SDK; package set differs (prefix-on-Darwin profiles) |
| Python | host python3 provided until prefix python exists |

Do **not** invent a huge Prefix-complete list on day one. Start with the
**smallest set that makes `toolchain --setup -p` cycle-free**, then grow when
live preflight names the next missing edge.

### Step 2 — Write path in `em setup --local`

1. Ensure `<EROOT>/etc/portage/profile/` exists (site profile layer).
2. Write `package.provided` (or `profile/package.provided` as a file under
   that dir — match portage(5): lines are under the profile node).
3. Idempotent: managed block marked with a tag (like crossdev autogen), so
   re-run can refresh without clobbering hand lines outside the block.
4. Only for **self-contained** topologies (`--local`, optionally bare
   `--root` bootstrap) — **not** for `--prefix` overlay (host VDB already
   satisfies BDEPEND via dual-root; provided would double-lie).

### Step 3 — Host CPV probing (any-linux first)

For each CPN in the policy set:

1. Prefer host VDB (`/var/db/pkg`) when the host is Gentoo / has a VDB.  
2. Else probe `command -v` / known paths and map to a **floor** CPV
   (`cat/pkg-0` is invalid — need a parseable version; use a documented
   sentinel like `pkg-0.0.0` only if solver accepts it for all ops, else
   pick a conservative real version from the tree’s newest stable).  
3. Record provenance in a comment line above each entry.

macOS: skip VDB; use floor versions + Darwin profile expectations.

### Step 4 — Wire lifecycle commands (optional but useful)

| Command / flag | Behaviour |
|----------------|-----------|
| `em setup --local` | layout + **bootstrap provided set** |
| `em --local toolchain --setup` | uses provided; builds real toolchain into prefix |
| `em --local provided sync` (new) or setup flag | re-probe / refresh managed block |
| `em --local provided drop <cpn\|--built>` | remove entries for packages now in prefix VDB |
| (later) auto-drop after merge | when cpv installs, remove matching provided line |

Until auto-drop exists, document the manual shrink step so operators do not
leave forever-provided gcc and never upgrade the prefix compiler.

### Step 5 — Solver / preflight verification

1. Unit: provided file under `etc/portage/profile` appears in
   `UseEnv.provided` for a `--local` roots fixture.  
2. Integration: synthetic cycle (A BDEP B, B BDEP A) broken when A is
   provided.  
3. Live: `em -p --local DIR toolchain --setup` after setup — **no** hard
   cycle preflight; plan size ≪ full world.

### Step 6 — Streamline presets

| Preset | Host | Profile hint | Provided policy |
|--------|------|--------------|-----------------|
| `any-linux` (default) | Linux + host gcc/clang + gnu userland | default/linux/… or embedded | Step 1 table |
| `macos` | Darwin + Xcode CLI | prefix-on-Darwin / vendor profile | Darwin table |
| `gentoo-host` | Gentoo | may use **smaller** provided set (or none) and prefer dual-root/`--prefix` instead | optional: only provide what’s missing |

Detect preset from `uname` + presence of `/var/db/pkg` unless
`--bootstrap-profile=` overrides.

### Step 7 — Live matrix (after code)

| Host | Topology | Expect |
|------|----------|--------|
| Gentoo Linux | `--local` + provided | toolchain --setup completes |
| Debian/Fedora (any-linux) | `--local` + provided | same; floor CPVs |
| macOS | `--local` + macos preset | same, no glibc |
| Gentoo | `--prefix` (control) | **no** auto provided; still works as overlay |

Also re-open Scenario B in [[clang-crossbuild-prefix-local-test-plan]] once
native bootstrap works.

---

## Non-goals

- Replacing stage tarballs for full stage3 production  
- Making `--local` as cheap as `--prefix` on a fat Gentoo host (overlay stays
  the fast path)  
- Lying forever: permanent provided of the whole `@system`  
- Fixing the dual-plan-entry / WORKDIR race — [[workdir-dual-root]]

---

## Open decisions

1. **Floor versions** when host has no VDB — use tree’s oldest stable, newest
   stable, or a fixed sentinel version the solver always matches?  
2. **Provide host libc or not on Linux** — providing glibc avoids the cycle
   but the prefix never builds its own until drop; Prefix tradition often
   provides libc initially.  
3. **Interaction with `with_bdeps` / InstalledPolicy** — ensure provided
   does not also need host VDB dual-root under `--local` (self-contained:
   base = target; host tools are only via provided + PATH).  
4. **macOS first-class when** — after any-linux green, or parallel data-only.

---

## Suggested commit series (when implementing)

```text
docs: --local bootstrap via package.provided (plan + design)
feat(setup): write bootstrap package.provided for --local
feat(setup): host CPV probe for any-linux provided set
feat(cli): em provided sync/drop (optional)
test: provided breaks bootstrap hard-cycle fixture
```

---

## References

- portage(5) `package.provided`  
- `portage-repo` `ProfileStack::package_provided`  
- `portage-atom-pubgrub` `set_provided`  
- Gentoo Prefix bootstrap (host tools + provided) — conceptual analogue  
- Live: [[clang-crossbuild-prefix-local-test-plan]] Scenario B  
- Cycle: `todo/dedup-availability-walks.md`, PENDING hard-cycle notes  
