# bash-crossdev package matrix (and how `em` maps today)

Reference for reasoning about `em crossdev` fidelity — especially later
**clang / LLVM (`-L`)** work. Companion plan: drop the planner-stamped
`BuildClass` re-classifier for cross-category packages
([`todo/drop-buildclass.md`](../todo/drop-buildclass.md)).

> **Names.** **bash-crossdev** = Gentoo’s `/usr/bin/crossdev`. **`em
> crossdev`** = this project. **host emerge** = ordinary emerge/`em` of
> `cross-*` / `cross_llvm-*` atoms using host (outer) config. **cross-emerge**
> = the separate wrapper that builds *ordinary* packages *into* a sysroot
> (`SYSROOT` / `PORTAGE_CONFIGROOT` / `CHOST` = target). This doc is about
> the first column only.

> **Slop warning.** Re-check `/usr/bin/crossdev` and live
> `/etc/portage/env/cross-*/` if behaviour here drifts; letter codes and
> package names are from the host’s crossdev as of the 2026-08-06 review.

---

## Architecture (both tools)

```text
bash-crossdev                         em crossdev
─────────────                         ───────────
select packages (letters)        →    packages() / stages
physical symlink overlay         →    virtual Location::Alias (repos.conf)
set_env → package.env + env/*    →    cross_env_entries → same files
host emerge cross-<T>/<pn>       →    em merge + use_outer_eroot (outer roots)
(no second PM classifier)        →    (+ BuildClass stamp — to be dropped)
```

**Durable build identity** for toolchain packages is **package.env on the
host/outer config root**. Eclasses still set `CTARGET` from the category when
needed (`crossdev.eclass` understands both `cross-*` and `cross_llvm-*`;
`toolchain.eclass` only special-cases `cross-*` for the GCC path).

bash-crossdev’s only host-vs-target switch at write time is the **stage
letter** passed to `set_env`:

```bash
case ${l} in
K|L)  # target packages — ABI = target multilib ;;
*)    # host packages  — ABI = host, plus TARGET_ABI / TARGET_* ;;
esac
```

`em` re-encodes that as `multilib::env_block(..., target_package: bool)` and
today keys `target_package` off `PackageArch::is_target()` — see
**em deltas** below.

---

## Letter codes (bash-crossdev)

| Letter | Default package | Role |
|--------|-----------------|------|
| B | sys-devel/binutils | host codegen (GCC model) |
| G | sys-devel/gcc | host codegen (GCC model) |
| D | dev-debug/gdb | host tool (`--ex-gdb` / extra) |
| K | sys-kernel/linux-headers | **target** env (K\|L) |
| L | sys-libs/{glibc,musl,newlib,…} | **target** env (K\|L) |
| C | sys-devel/clang-crossdev-wrappers | host (LLVM model) |
| R | llvm-runtimes/compiler-rt | host env (`*`), not K\|L |
| U | llvm-runtimes/libunwind | host env |
| A | llvm-runtimes/libcxxabi | host env |
| P | llvm-runtimes/libcxx | host env |
| X | `--ex-pkg` extras | host env |

LLVM pkglist (when `LLVM=yes`): K, L, R, C, A, P, U (plus optional extras).  
GCC pkglist: K, L, B, G, D (D only if ex-gdb).

Category prefix: `cross-<CTARGET>` (GCC) or `cross_llvm-<CTARGET>` (LLVM).

---

## Matrix — GCC model (`cross-<T>/…`)

| PN | Letter | bash package.env | em `PackageArch` (today) | em package.env (today) | HostCodegen specials needed? |
|----|--------|------------------|--------------------------|------------------------|------------------------------|
| binutils | B | host + `TARGET_*` | Host | host | **yes** (PATH / EPREFIX / ESYSROOT / idirafter) |
| gcc | G | host + `TARGET_*` | Host | host | **yes** |
| gdb (extra) | D / X | host + `TARGET_*` | undeclared → host | host | usually yes if built as cross gdb |
| linux-headers | **K** | **target** ABI | Target | target | no |
| glibc / musl | **L** | **target** | Target | target | no |
| newlib | **L** | **target** | Target | target | no |

HostCodegen = em-only shell behaviours keyed today on
`BuildClass::CrossToolHost` in `portage-repo` `shell.rs` (not expressed in
package.env).

---

## Matrix — LLVM model (`cross_llvm-<T>/…`)

| PN | Letter | bash package.env | em `PackageArch` (today) | em package.env (today) | HostCodegen specials needed? |
|----|--------|------------------|--------------------------|------------------------|------------------------------|
| clang-crossdev-wrappers | C | host + `TARGET_*` | Host | host | **yes** (same class as gcc wrappers) |
| linux-headers | K | target | Target | target | no |
| musl / newlib | L | target | Target | target | no |
| compiler-rt | **R** | **host** + `TARGET_*` | **Target** | **target** ⚠️ | no (ebuild uses host clang + `CTARGET`) |
| libunwind | **U** | **host** | **Target** | **target** ⚠️ | no |
| libcxxabi | **A** | **host** | **Target** | **target** ⚠️ | no |
| libcxx | **P** | **host** | **Target** | **target** ⚠️ | no |

⚠️ = **known em delta vs bash-crossdev**: we write K|L-style target env for
llvm-runtimes because `PackageArch::Target` drives `env_block`. bash-crossdev
gives them the host (`*`) branch. Fix under
[`todo/drop-buildclass.md`](../todo/drop-buildclass.md) Step 1 before relying
on package.env as the sole authority.

Ebuilds still install target bits into `/usr/${CTARGET}/…` via
`is_crosspkg` / cmake install prefixes; that is **not** the same as
package.env’s multilib ABI branch.

---

## What each channel controls

| Channel | Written when | Read when | Owns |
|---------|--------------|-----------|------|
| Stage letters / `packages()` | plan / setup | selection only | which atoms exist |
| package.env + `env/*.conf` | `--init-target` / `--setup` | every build (sourced) | multilib ABI, `TARGET_*`, CTARGET header (em) |
| Eclass + `CATEGORY` | n/a | ebuild inherit | `CTARGET` from `cross-*` / `cross_llvm-*` |
| HostCodegen (today: CrossToolHost) | plan stamp or unstamped | `run_phase` | PATH, EPREFIX flip, ESYSROOT=`…/usr/<triple>/`, `-idirafter` |
| Ordinary `--target` pkg | n/a | CrossTarget / cross-emerge-like roots | zlib-for-T etc. — **not** this matrix |

### package.env sniff (bash marker, already in em shell)

When no CrossTool stamp overrides:

- `TARGET_ABI` set → treat as **host-env** package (host tool_tuple, no
  `BUILD_*` from CTARGET path)
- `CTARGET` set, no `TARGET_ABI`, `CTARGET ≠ CHOST` → **target-env** package
  (CTARGET tools, `produces_target_code`)

This matches bash-crossdev’s K|L vs `*` marker without a second classifier.

---

## `BuildClass` vs this matrix (why it is a bad fit)

| Hope for BuildClass | Reality |
|---------------------|---------|
| Re-state K\|L vs `*` for the shell | package.env already does; stamp runs *after* source and can override |
| Drive HostCodegen specials | Real need, but only for B/G/C/D-ish — **not** all host-env letters (R/U/A/P) |
| One table for llvm + gcc | `PackageArch::Target` for runtimes ≠ bash letter env; `cross_llvm-*` never hit `strip_prefix("cross-")` in classify |

Full argument and removal plan: [`todo/drop-buildclass.md`](../todo/drop-buildclass.md).

---

## Later: clang support checklist

When revisiting LLVM/clang cross under `--prefix` / `--local` / bare host:

1. **package.env first** — R/U/A/P host-env like bash; C host-env; K/L target.
2. **HostCodegen only on C** (and any true codegen extra), not on runtimes.
3. Live: `EM_BUILD_CLASS` should not be required; sniff + eclass suffice if
   env files are correct.
4. Compare generated `env/cross_llvm-<T>/*.conf` to a real bash-crossdev `-L`
   tree on the same tuple if available.
5. Ordinary `em --target T -b llvm-core/clang` is the **other** job
   (cross-emerge-shaped), not this matrix — keep scenarios separate
   ([`todo/clang-crossbuild-prefix-local-test-plan.md`](../todo/clang-crossbuild-prefix-local-test-plan.md)).

---

## How to re-verify against the host

```bash
# Letters and set_env
rg -n 'set_env|pkglist|CROSSDEV_OVERLAY_CATEGORY_PREFIX|case \$\{l\}' /usr/bin/crossdev

# Live env samples (if a target was set up by bash-crossdev)
ls /etc/portage/env/cross-* /etc/portage/env/cross_llvm-* 2>/dev/null
# Host letter: expect TARGET_ABI=… and host ABI=
# K|L letter: expect target ABI=, no TARGET_ABI=

# eclass category handling
rg -n 'cross_llvm|cross-\*' /var/db/repos/gentoo/eclass/crossdev.eclass
```

---

## See also

- [`docs/crossdev.md`](./crossdev.md) — user-facing `em crossdev`  
- [`docs/root-topology.md`](./root-topology.md) — roots / outer EROOT / use_outer_eroot  
- [`todo/drop-buildclass.md`](../todo/drop-buildclass.md) — removal plan  
- [`todo/root-topology-refactor.md`](../todo/root-topology-refactor.md) — Track A history  
