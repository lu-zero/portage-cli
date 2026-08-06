# Bootstrapping `em --local` (standalone prefix)

How a fresh **self-contained** unprivileged prefix gets a working toolchain
without resolving an irreducible hard cycle from absolute zero. Implementation
tracker: [`todo/local-bootstrap-provided.md`](../todo/local-bootstrap-provided.md).

Topology background: [`root-topology.md`](./root-topology.md) scenario **2b**.

---

## Why this is hard

`--local` (default `~/.gentoo`, or `em --local DIR`) is **not** an overlay on a
Gentoo host VDB. Base = target = the prefix: the solver’s “already installed”
view starts **empty**. Building `sys-devel/binutils` then pulls python, meson,
glibc, gettext, elt-patches, … until the graph contains **genuine hard cycles**
that no install order can satisfy.

That matches real Gentoo practice: nobody bootstraps `@system` from a naked
root. Stage tarballs (or Gentoo Prefix’s host tools) seed the world. For
`--local`, the Portage-native seed is **`package.provided`**: CPVs the
**host** supplies externally for the bootstrap window.

`em --prefix` does **not** need this path: dual-root satisfaction reads the
host VDB. Prefer `--prefix` on a complete Gentoo machine when you only need a
delta; use `--local` when the prefix must stand alone (foreign distro, macOS,
reproducible self-contained tree).

---

## What we already support

`package.provided` is a **first-class read** in em:

1. Profile stack (including site profile
   `<EROOT>/etc/portage/profile/package.provided`)
2. Solver drops dependency edges that match a provided CPV (package is never
   planned for merge)
3. Preflight / BDEPEND availability treat provided as present

**Not yet automated:** generating that file at `em setup --local`, probing host
versions, or retiring provided lines after real packages land in the prefix
VDB. Until the setup write path lands, a hand-written file under the site
profile **already works** if you know what to list.

---

## Target lifecycle

```text
1. em setup --local [DIR]
      skeleton + make.conf/bashrc + (planned) bootstrap package.provided

2. em --local [DIR] toolchain --setup
      host tools satisfy provided deps; real toolchain merges into prefix

3. (optional) shrink package.provided as prefix VDB grows
      stop lying about packages the prefix now owns

4. em --local [DIR] stages --stage1 / normal merges
      self-hosting; BROOT effectively the prefix once its gcc is on PATH

5. (optional) em --local [DIR] --target T crossdev --setup
      cross toolchain on top of a bootstrapped native prefix
```

Crossdev under `--local` is **blocked** until step 2 works — not a separate
cross bug (confirmed 2026-08-06).

---

## Manual bootstrap (works today)

On a machine with a usable host compiler and userland:

```sh
PREFIX=$HOME/.gentoo   # or any DIR
em setup --local "$PREFIX"

mkdir -p "$PREFIX/etc/portage/profile"
# Managed block — versions should match what the host can actually run.
# Example sketch (adjust versions to your host / tree):
cat >> "$PREFIX/etc/portage/profile/package.provided" <<'EOF'
# bootstrap seed — host-supplied until the prefix owns replacements
sys-devel/gcc-14.2.1
sys-devel/binutils-2.44
sys-libs/glibc-2.41
sys-kernel/linux-headers-6.12
dev-lang/python-3.13.0
dev-build/meson-1.7.0
dev-build/ninja-1.12.1
sys-devel/make-4.4.1
dev-build/cmake-3.31.0
app-portage/elt-patches-20250317
app-arch/xz-utils-5.6.4
sys-apps/coreutils-9.6
sys-apps/gawk-5.3.1
sys-apps/grep-3.11
sys-apps/sed-4.9
sys-devel/m4-1.4.19
dev-build/autoconf-2.72
dev-build/automake-1.17
sys-devel/libtool-2.5.4
sys-devel/patch-2.7.6
EOF

em -p --local "$PREFIX" toolchain --setup   # expect: no hard-cycle preflight
# em --local "$PREFIX" toolchain --setup    # real run when plan looks right
```

**Rules of thumb**

- Each line is an exact **CPV** (`cat/pkg-version`), optional leading `=`.
- Versions must be ones the **solver accepts** for the deps that name them
  (too-old floors can still fail version constraints).
- Prefer host VDB versions on Gentoo (`ls /var/db/pkg/sys-devel/gcc`).
- On non-Gentoo hosts, pick versions present in the active `::gentoo` tree
  that are ≤ what the host tools roughly are (or use known-good floors from
  a tested matrix — TBD under the todo).
- Removing a line means “build this for real next time.”

Site profile is the right place: Portage (and em) append
`/etc/portage/profile` as the highest profile layer via
`ProfileStack::with_user_profile`.

---

## Planned automation (any-linux, then macOS)

| Preset | Detection | Role of provided |
|--------|-----------|------------------|
| **any-linux** | Linux + host cc | Seed compiler, libc, headers, python, meson, coreutils, … |
| **macos** | Darwin | Seed clang/python/userland; **never** glibc; Darwin profile |
| **gentoo-host** (optional) | `/var/db/pkg` | Smaller set or skip — often `--prefix` is better |

`em setup --local` will write a **tagged managed block** into
`etc/portage/profile/package.provided` so re-runs can refresh without wiping
hand edits outside the block. Host CPV probing fills versions when possible.

Streamlining goal: one command path —

```sh
em setup --local              # layout + provided seed
em --local toolchain --setup  # first real merges
```

— works on a generic Linux box and (later) macOS with Xcode CLT, without
hand-editing CPVs.

Details and step list: [`todo/local-bootstrap-provided.md`](../todo/local-bootstrap-provided.md).

---

## What belongs in provided vs what must be built

| Host supplies (candidates for provided) | Prefix should own eventually |
|----------------------------------------|------------------------------|
| C/C++ compiler driver used for bootstrap | prefix `sys-devel/gcc` or clang (after drop) |
| Host libc + kernel headers (Linux) | optional own glibc — policy open |
| python/meson/ninja used by ebuilds early | prefix copies once cycle is breakable |
| coreutils/sed/grep/gawk/m4/… | prefix set when you care about independence |

**HostCodegen / cross packages** are a different topic (host emerge of
`cross-*` after native bootstrap) — see
[`bash-crossdev-matrix.md`](./bash-crossdev-matrix.md).

---

## Relation to other topologies

| Topology | Bootstrap seed |
|----------|----------------|
| bare `/` Gentoo | host VDB (nothing special) |
| `--prefix DIR` | host VDB via dual-root / BDEPEND satisfaction |
| `--root DIR` self-contained | same class as `--local` (empty VDB) — may share provided machinery |
| `--local` | **package.provided** (this doc) |
| stage tarball into `--root` | VDB already populated — no provided needed |

---

## Failure modes to remember

1. **Empty provided + empty VDB** → hard cycle / preflight explosion (today’s
   default `--local` experience after setup-only).
2. **Stale provided forever** → prefix never builds its own gcc; upgrades look
   “already satisfied.” Shrink the file after toolchain lands.
3. **Wrong versions** → solver still fails version ops; probe host or bump
   floors.
4. **Using provided under `--prefix`** → usually wrong (host VDB already
   answers); do not auto-write provided for overlays.

---

## See also

- [`docs/root-topology.md`](./root-topology.md) — 2b `--local`, lifecycle  
- [`docs/crossdev.md`](./crossdev.md) — cross after native bootstrap  
- [`todo/local-bootstrap-provided.md`](../todo/local-bootstrap-provided.md) — implementation plan  
- [`todo/clang-crossbuild-prefix-local-test-plan.md`](../todo/clang-crossbuild-prefix-local-test-plan.md) — Scenario B blocked on this  
