# em prefix / multi-root path experiment

Working notes on **why** `--prefix`, EPREFIX, baselayout, host-tool links, and
related knobs exist — and what class of bug they address. This is design
context, not a user how-to. For topology tables see
[`root-topology.md`](./root-topology.md); for crossdev how-to see
[`crossdev.md`](../user/crossdev.md); for stage recipes see
[`stages-and-testing.md`](../user/stages-and-testing.md); for bash-crossdev env
letters see [`bash-crossdev-matrix.md`](./bash-crossdev-matrix.md); for the
user how-to (bootstrap + use a `--prefix`'s own compiler) see
[`prefix-toolchain.md`](../user/prefix-toolchain.md).

> **Slop warning.** Prefer the code when this disagrees. Status of live
> findings may lag in [`todo/for-sonnet.md`](../../todo/for-sonnet.md).

---

## One-line thesis

The hard bugs are not “em forgot a directory.” They are **Portage and ebuild
assumptions about a single coherent path world**, plus **workarounds when
paths leak or build systems probe the wrong root** — made louder by em’s
explicit multi-root model (host BROOT + overlay EPREFIX + target sysroot).

---

## Portage’s path assumption

Classic Portage (and much of the ebuild tree) behave as if:

```text
ROOT / EPREFIX / EROOT / SYSROOT / ESYSROOT / BROOT / PATH
  form one coherent story for a given emerge.
```

Consequences:

- Ebuilds and eclasses hardcode **prefix-absolute** tools  
  (`${EPREFIX}/bin/bash`, `${EPREFIX}/usr/bin/xargs`, shebangs,  
  `toolchain.eclass` `PREFIX=${EPREFIX}/usr`, …).
- Autoconf/meson/cmake/pkg-config **probe** whatever tree they are told  
  (or the host default search path) and bake results into the build.
- Profile **layout** (merged-usr vs split-usr) is implemented by  
  **baselayout**, not by the package manager inventing `bin`/`usr/bin`.

Portage already carries workarounds for partial failures of that story:
`package.env`, CHOST/CTARGET splits, ROOT vs BROOT, bashrc hooks, crossdev’s
own env files, `--with-sysroot`, etc. em inherits the same problem class and
makes more of it **visible** by naming three places at once.

---

## em’s multi-root reality

| Place | Typical role under `--prefix P --target T` |
|-------|--------------------------------------------|
| **Host `/`** | BROOT, native tools, often the real userland |
| **Outer EPREFIX `P`** | Overlay install root for host-arch / Prefix-shaped packages |
| **Sysroot `P/usr/<tuple>/`** | Target ABI headers/libs, target packages |

Config may be host, overlay, or (for self-contained `--root`) paired with
`--config-root`. That is orthogonal but often confused with “where files go.”

A plain `emerge` on a Gentoo host rarely hits all three at once.  
`em --prefix … --target …` does by design.

---

## Failure modes (the real bug class)

| Mode | What goes wrong |
|------|-----------------|
| **Path leakage** | Scripts, configure, or eclasses bake `${EPREFIX}/…` or a host path that is wrong for this topology |
| **Wrong probe** | Build system finds host `gcc`/headers/`.pc`, or an empty/half-empty prefix tree |
| **Layout vs profile** | Profile is merged-usr; disk has only `usr/bin` and no `bin` → `${EPREFIX}/bin/bash` dies |
| **Identity** | Files exist as `cross-T/glibc` while DEPEND asks for `sys-libs/glibc` (VDB / Favor / provided) |
| **Order** | Soft RDEPEND cycles put empty virtuals before providers when hard paths forbid the reverse |

These stack. Fixing only order does not fix identity; fixing only layout does
not fix every probe; host-tool allowlists do not replace baselayout.

### Canary: `Roots::build_eprefix` vs `Roots::eprefix`

Path leakage canary for the per-package `EPREFIX` a build should bake into
its own installed `.pc`/`.la` files: only when `eprefix()` IS `merge_root()`
(the PMS invariant `EROOT = ROOT + EPREFIX` holds) is it safe to use —
`None` whenever an explicit `--root` or `--target` sysroot substitution has
moved `merge_root()` away from the outer anchor, since that package installs
into a self-contained, unprefixed tree. Live-verified regressions before this
existed: `libffi`'s `.pc` under `--prefix P --target T`, and `zlib`'s `.pc`
under plain `--prefix P --root B` (no `--target` at all) — same root cause,
no `--target`/`is_cross_arch()` special-casing needed. `RootContext.eprefix`
has exactly three callers (`merge/mod.rs`, `emerge.rs`'s unmerge path,
`dispatch.rs`'s `__ebuild`); all three must read `build_eprefix()`, never
raw `eprefix()`, for that field.

---

## What each mechanism is for

### Layout (profile-correct tree)

**Authority: merge `sys-apps/baselayout`**, not a second mkdir implementation.

| Action | Destination |
|--------|-------------|
| `em setup` → oneshot baselayout (`USE=build`) | Outer EROOT / EPREFIX |
| `crossdev --setup` outer seed (same helper) | Outer EPREFIX before toolchain steps |
| `toolchain_plan` baselayout (`into_sysroot` for cross) | Target sysroot |
| `stages --stage1` baselayout | Stage product root / sysroot under `--target` |

**Product rule:** keep the default **merged-usr** profile; make **disk** match
it. Do **not** switch the profile to split-usr to paper over a missing
baselayout.

Hand-rolling `bin → usr/bin` in setup was rejected: `em -1 baselayout`
already encodes layout policy (and split-usr when the profile says so).

### Content under EPREFIX (overlay only)

**`HOST_BASE_TOOLS` / host python symlinks** (`setup.rs`): put real **host
binaries** at `${EPREFIX}/usr/bin/…` so shebangs and hardcoded tool paths
resolve **without** building a full Prefix userland under `P`.

After baselayout’s merged-usr `bin` → `usr/bin`, `${EPREFIX}/bin/bash` and
`${EPREFIX}/usr/bin/bash` both work if `bash` is linked under `usr/bin`.

This is a **workaround for leakage** (ebuilds assume Prefix-absolute tools),
not a layout engine. Prefer not to grow the list forever; fix layout and
root-var policy first.

### Build env / DESTDIR / ESYSROOT (host code generators)

**`host_codegen`** allowlist (`portage-repo` `EbuildShell::is_cross_host_codegen`:
`cross-*/{binutils,gcc,gdb,clang-crossdev-wrappers}`): remaps EPREFIX /
ESYSROOT / PATH for packages that must install as host tools with
Prefix-shaped `./configure --prefix=…` while living in multi-root em.

Generalizing this by PN list for every ebuild that does
`CONFIG_SHELL=${EPREFIX}/bin/bash` (e.g. libtool) is a symptom treadmill.
Better: correct EPREFIX tree + clear BROOT vs EPREFIX rules; only then
special-case remaining DESTDIR mismatches.

**Why the EPREFIX flip is needed at all, not just cosmetic:** `toolchain.eclass`
passes `--prefix=${EPREFIX}/usr` to the package's own `./configure`, and
DESTDIR+prefix is a *physical* install-path convention (`make install
DESTDIR=${D}` writes under `${D}${prefix}/...`) — unlike ESYSROOT (a pure
DEPEND-resolution hint), what `PREFIX` bakes in also determines where built
files land inside `${D}`, and `ED` must match for the merge step to find
them. `--local` already supplies a correct EPREFIX by construction; a
self-contained `--root DIR` (no `--local`) has `eprefix` empty, so
`toolchain.eclass`'s baked-in `--with-sysroot` collapses to the bare host
path `/usr/<tuple>` (the host's own unrelated crossdev sysroot, if any).
Fix: for `host_codegen` packages only, when `eprefix` is otherwise empty,
offset it exactly as `--local` would (root becomes EPREFIX, ROOT becomes
`/`) — reusing the already-tested EPREFIX-subtree merge logic
(`ebuild.rs::ed_image_dir`) generically rather than a new merge path.

SYSROOT/ESYSROOT are untouched by this flip and must stay so: SYSROOT
already equals `root_str` for a plain `--root` build (it must link against
the root's own, not the real host's, native libc), and ESYSROOT for this
package class is computed straight from `root_str` independent of
`eprefix` — so flipping `eprefix` doesn't double-count either.

### Stage1 USE wipe and repair

Catalyst-shaped:

```text
USE="-* build ${BOOTSTRAP_USE}"   # + elibc_*/kernel_* re-add after wipe
--autosolve-use always on for stages --stage1
```

| Piece | Role |
|-------|------|
| `-*` | Wipe optional USE (Portage-identical, including IUSE `+` defaults) |
| `BOOTSTRAP_USE` | Profile bootstrap tokens (python targets, …) not in normal USE fold |
| autosolve + prefer IUSE `+` when ceding | Restore eclass defaults (e.g. `app-alternatives` first provider) |

Do **not** replace autosolve with hardcoded `gawk`/`bison` in global USE.

### Soft install order (B+C)

`install_order` soft-ready pick + pass-2 acyclic soft-edge repair. Fixes
orders that soft cycles scramble when soft promote is **legal**.

Does **not** invent order across a **hard** path
(`virtual/libcrypt → python → glibc → libxcrypt` on one MergeRoot). That is
**library identity / dual-root BDEPEND**, not more order heuristics.

### Library identity (open)

After `crossdev --setup`, files often sit under the sysroot while VDB identity
is `cross-T/*` on the outer tree. Ordinary packages Depend on **real** Cpns.
Until Favor/provided/real-Cpn VDB bridges that, plans re-pull `sys-libs/glibc`
and friends — the residual “libxcrypt never Emerges / plan balloons” class
after soft-order.

---

## Intended ladder (ordinary packages under `--target`)

```text
1. em setup --prefix P | --local | --root R
     → skeleton + config (+ host tool links if overlay)
     → oneshot baselayout on outer EROOT

2. em … crossdev --setup
     → outer baselayout if not already (prefix bootstrap path)
     → sysroot baselayout + toolchain into T

3. em … stages --stage1
     → baselayout + packages.build under USE=-* build + BOOTSTRAP_USE
     → autosolve-use on

4. em … ordinary atoms (e.g. llvm-core/clang)
     → full USE on a base that has layout + real-Cpn identity as far as we
       have implemented it
```

Skipping (1)–(3) and emerging a fat world into an empty or identity-blind
sysroot is out of order, not a pure solver bug.

---

## What is *not* the bug

- Soft-order failing to break a real hard cycle  
- Gentoo `virtual/*` packages being filtered as solver-internal `is_virtual`  
  (they are Real; see package docs)  
- “Just switch the profile to split-usr” for a missing baselayout  
- Replacing Portage path assumptions with an ever-growing host-tool list  

---

## Streamlining principles (for later work)

1. **One authority per concern**  
   layout → baselayout; host-vs-target env → package.env; install-path
   specials for cross host tools → host_codegen / RootVars; order →
   install_order; identity → Favor/VDB/provided.

2. **Prefer merge over reinvent**  
   `em -1 baselayout` beats mkdir layout clones.

3. **Workarounds are for Portage/ebuild leakage**, not a parallel filesystem
   policy language.

4. **Docs stay honest about multi-root**  
   Overlay `--prefix` is not a second full OS; it still needs a
   profile-correct EPREFIX tree because ebuilds will probe it.

5. **Open product work**  
   - Library identity for cross-T vs real Cpn  
   - Bare `--root` config/repos overlay (alias written but not always read)  
   - Optional: generalize host-side EPREFIX handling beyond the codegen PN list  
   - Topology bool soup (`use_outer_eroot` × `into_sysroot` × …) → clearer
     `MergeDestination` / topology enum  

---

## Related code (entry points)

| Concern | Location |
|---------|----------|
| Setup + outer baselayout + host links | `portage-cli/src/setup.rs` |
| Cross plan baselayout / stage1 plan | `portage-cli/src/crossdev/stages.rs` |
| Staged driver, BOOTSTRAP_USE, stage1 autosolve | `portage-cli/src/crossdev/mod.rs` |
| `use_outer_eroot` routing | `portage-cli/src/emerge.rs` |
| host_codegen / ROOT vars | `portage-repo/src/build/shell.rs` |
| Soft-order B+C | `portage-atom-pubgrub/src/graph.rs` |
| Roots model | `portage-resolve/src/roots.rs` |

---

## Status (snapshot)

Written 2026-08-07 after live stage1 / soft-order / setup-layout work:

- Soft-order B+C + hard-edge lock: landed; residual #3 is mostly identity.  
- Stage1 autosolve + IUSE `+` prefer: live-confirmed for app-alternatives.  
- Outer baselayout via oneshot merge: landed (replaces hand-rolled layout seed).  
- Library identity, bare-`--root` repos overlay, libtool/EPREFIX residual:
  still open or live-verify after baselayout on fresh prefixes.
)
