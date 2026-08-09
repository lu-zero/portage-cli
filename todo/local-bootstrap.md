# Bootstrapping `em --local` (standalone prefix)

How a fresh **self-contained** unprivileged prefix gets a working toolchain
without resolving an irreducible hard cycle from absolute zero. Implementation
tracker: [`todo/local-bootstrap-provided.md`](./local-bootstrap-provided.md).

Topology background: [`root-topology.md`](../docs/design/root-topology.md) scenario **2b**.

---

## Setup ladder (repo → profile → provided → toolchain)

`package.provided` alone is not enough. A usable `--local` needs a complete
**config root** under the prefix. As soon as
`<prefix>/etc/portage/make.profile` exists, em treats the prefix as
`PORTAGE_CONFIGROOT` and **stops** reading the host’s `repos.conf` / site
profile — so repo and profile must be established **together**.

| Step | What | Status today |
|------|------|----------------|
| **1. Layout** | dirs, `bashrc`, `make.conf` placeholder | ✅ `em setup --local` |
| **2. Main repo (`::gentoo`)** | ebuild tree: **piggy-back** host tree if present, else **own** checkout under the prefix + `em sync` | 🟡 host tree often works while config is still host; **no** prefix `repos.conf` written by setup — breaks once config flips |
| **3. Profile** | `make.profile` → path under that repo’s `profiles/` | 🟡 manual: `em --config-root DIR select profile set …` (note: **not** `em --local select` today); no defaults for Linux/macOS foreign hosts |
| **4. `package.provided`** | host tools so empty VDB plans are cycle-free | 🔴 hand-write only |
| **5. Toolchain** | `em --local toolchain --setup` | 🟡 blocked on 2–4 for a true empty prefix |

**Target one-shot (planned):**

```sh
em setup --local [DIR] [--profile …] [--repo-location …] [--sync]
em --local [DIR] toolchain --setup
```

### Repo (step 2) — piggy-back or own

| Situation | Behaviour |
|-----------|-----------|
| Host has `::gentoo` (`repos.conf` or `/var/db/repos/gentoo`) | Write prefix `repos.conf` with `location =` that path (share the tree). |
| No tree | `location = <prefix>/var/db/repos/gentoo` + default git `sync-uri`; run or instruct `em --local sync`. |
| Override | User-supplied location / existing overlay `repos.conf` wins. |

### Profile (step 3) — defaults and override

| Host | Default when user does not pass `--profile` |
|------|-----------------------------------------------|
| Gentoo, host profile resolves **into the same tree** | Mirror host’s resolved `make.profile`. |
| Linux foreign | **Prefix profile** for host ARCH (e.g. `default/linux/<arch>/<release>/no-multilib/prefix`) — safer under EPREFIX than a plain desktop profile. |
| macOS | Newest `prefix/darwin/macos/…` for `arm64-macos` / `x64-macos`. If host OS is newer than any tree entry, still pick newest and **warn**. |
| Always | Override: planned `em setup --local --profile …`; after setup, `em --local DIR select profile set …` (planned — topology flags target the prefix; today only `--config-root DIR` does). |

Full algorithm and gaps: the todo’s **Setup ladder** section.

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
   `{config root}/etc/portage/profile/package.provided`)
2. Solver drops dependency edges that match a provided CPV (package is never
   planned for merge as a *dependency*)
3. Preflight / BDEPEND availability treat provided as present; depgraph also
   seeds provided as host-installed for slot-aware BDEPEND

**Config-root coupling:** for `--local`, em uses the **host** as config root
until `<prefix>/etc/portage/make.profile` exists; only then does the prefix’s
site profile (and its `package.provided`) apply. Hand-seeding provided under
the prefix without that link is a silent no-op.

**Not yet automated:** creating `make.profile` + generating provided at
`em setup --local`, probing host versions, or retiring provided lines after
real packages land in the prefix VDB. Until the setup write path lands, the
manual recipe below works if both files are in place.

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

On a machine with a usable host compiler and userland. **Three files matter
together** once the prefix owns config:

1. `repos.conf` — where `::gentoo` lives (host path or prefix checkout)
2. `make.profile` — flips `PORTAGE_CONFIGROOT` to the prefix
3. `profile/package.provided` — host-tool seed

Without (2), provided under `$PREFIX` is ignored (host site profile wins).
With (2) but without (1), the next `em --local` loses the host’s
`repos.conf` and cannot open ebuilds.

```sh
PREFIX=$HOME/.gentoo   # or any DIR
em setup --local "$PREFIX"

# 1) Main repo — piggy-back host tree (or point at your own checkout).
mkdir -p "$PREFIX/etc/portage/repos.conf"
if [[ ! -e $PREFIX/etc/portage/repos.conf/gentoo.conf ]]; then
  GENTOO=$(portageq get_repo_path / gentoo 2>/dev/null || echo /var/db/repos/gentoo)
  cat >"$PREFIX/etc/portage/repos.conf/gentoo.conf" <<EOF
[DEFAULT]
main-repo = gentoo
[gentoo]
location = $GENTOO
EOF
fi

# 2) Own config root: without this, package.provided under $PREFIX is not read.
#    Prefer a *prefix* profile under EPREFIX (not plain default/linux/…/desktop).
#    Today select needs --config-root; planned: em --local "$PREFIX" select profile …
mkdir -p "$PREFIX/etc/portage"
if [[ ! -e $PREFIX/etc/portage/make.profile ]]; then
  # Gentoo host: mirror host profile if it lives under the same tree.
  # Foreign Linux: pick …/no-multilib/prefix for your ARCH from profiles.desc.
  # macOS: newest prefix/darwin/macos/… for arm64-macos|x64-macos (warn if OS newer).
  ln -s "$(readlink -f /etc/portage/make.profile)" \
        "$PREFIX/etc/portage/make.profile"
fi

# 3) Bootstrap seed — host-supplied *tools*, not stage products.
#    Prefer: do NOT list baselayout/binutils/linux-headers/glibc/gcc here so
#    `toolchain --setup` still merges them into the prefix. List the cycle fuel
#    (python, meson, gettext, elt-patches, coreutils, …). Versions: host VDB
#    on Gentoo, or a version present in ::gentoo that satisfies deps.
mkdir -p "$PREFIX/etc/portage/profile"
cat >> "$PREFIX/etc/portage/profile/package.provided" <<'EOF'
# bootstrap seed — host tools until the prefix owns replacements
# (adjust versions to your host / tree; example sketch only)
dev-lang/python-3.13.0
dev-lang/perl-5.40.0
dev-build/meson-1.7.0
dev-build/ninja-1.12.1
sys-devel/make-4.4.1
dev-build/cmake-3.31.0
app-portage/elt-patches-20250317
app-arch/xz-utils-5.6.4
sys-devel/gettext-0.23
sys-apps/coreutils-9.6
sys-apps/gawk-5.3.1
sys-apps/grep-3.11
sys-apps/sed-4.9
sys-apps/findutils-4.10.0
sys-apps/file-5.46
sys-devel/m4-1.4.19
dev-build/autoconf-2.72
dev-build/automake-1.17
sys-devel/libtool-2.5.4
sys-devel/patch-2.7.6
app-arch/bzip2-1.0.8
app-arch/gzip-1.13
app-arch/tar-1.35
app-arch/zstd-1.5.7
EOF

em -p --local "$PREFIX" toolchain --setup   # expect: no hard-cycle preflight
# em --local "$PREFIX" toolchain --setup    # real run when plan looks right
```

**Rules of thumb**

- Each line is an exact **CPV** (`cat/pkg-version`), optional leading `=`.
- Versions must be ones the **solver accepts** for the deps that name them
  (too-old floors can still fail version constraints; prefer host VDB on
  Gentoo: `ls /var/db/pkg/dev-lang/python*`).
- On non-Gentoo hosts, pick versions present in the active `::gentoo` tree
  that are ≤ what the host tools roughly are (or use known-good floors —
  TBD under the todo).
- Prefer providing **build tools**, not the packages `toolchain --setup`
  is about to install (binutils/headers/libc/gcc). Providing those can make
  stage steps no-op.
- Removing a line means “build this for real next time.”
- Under `--local`, BROOT is the prefix: host VDB is **not** dual-root
  woven in. Provided is the only solver-visible host seed.

Site profile is the right place: Portage (and em) append
`{PORTAGE_CONFIGROOT}/etc/portage/profile` as the highest profile layer via
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

Details and step list: [`todo/local-bootstrap-provided.md`](./local-bootstrap-provided.md).

---

## What belongs in provided vs what must be built

| Host supplies (v1 **provided** — cycle fuel) | Prefix builds via `toolchain --setup` (**not** provided) |
|-----------------------------------------------|----------------------------------------------------------|
| python, perl, meson, ninja, cmake, make, m4, autoconf/automake/libtool | `sys-apps/baselayout` |
| gettext, elt-patches, xz/zstd/bzip2/gzip/tar | `sys-devel/binutils` |
| coreutils, sed, grep, gawk, findutils, file, patch | `sys-kernel/linux-headers`, then libc, then `sys-devel/gcc` |

Providing stage-product CPNs (gcc/glibc/binutils) can make those steps
no-op while PATH still uses the host — fine only as an explicit temporary
exception, not the default. Full policy:
[`todo/local-bootstrap-provided.md`](./local-bootstrap-provided.md).

**HostCodegen / cross packages** are a different topic (host emerge of
`cross-*` after native bootstrap) — see
[`bash-crossdev-matrix.md`](../docs/design/bash-crossdev-matrix.md).

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

- [`docs/root-topology.md`](../docs/design/root-topology.md) — 2b `--local`, lifecycle  
- [`docs/crossdev.md`](../docs/user/crossdev.md) — cross after native bootstrap  
- [`docs/prefix-toolchain.md`](../docs/user/prefix-toolchain.md) — the `--prefix` equivalent, live-verified 2026-08-08  
- [`todo/local-bootstrap-provided.md`](./local-bootstrap-provided.md) — implementation plan  
- [`todo/clang-crossbuild-prefix-local-test-plan.md`](./clang-crossbuild-prefix-local-test-plan.md) — Scenario B blocked on this  
