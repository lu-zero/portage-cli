# Dep Resolver Investigation

> **Status (2026-07):** This investigation is largely complete. The gaps below
> were closed in 2026-06; see [`architecture.md`](./architecture.md) for the
> current `em -p` / `em query depgraph` pipeline and known emerge divergences.

## Goal

Close the gap between `em query depgraph <atom>` and `emerge -p <atom>` output.
Three identified gaps:

1. **No VDB awareness** — we always resolve from scratch; portage skips installed packages
2. **Profile USE flags not loaded** — we only read `make.conf` USE; portage applies the full profile stack
3. **Virtual-to-provider expansion** — `virtual/*` packages appear in our output; portage resolves them to concrete providers

## Benchmark

### app-text/texlive (initial state)

| | Portage | em |
|---|---|---|
| Total packages | 93 | 196 |
| In portage, missing from em | 34 | — |
| In em, extra vs portage | — | 137 |

After CPN normalization (strip version):

- **137 extra in em**: 136 of 137 are already installed on the system (VDB gap)
- **34 missing from em**: packages like `dev-libs/kpathsea`, `media-libs/harfbuzz`,
  `x11-libs/cairo`, `media-libs/fontconfig`, `app-text/texlive-core`,
  `dev-texlive/texlive-basic`, `dev-texlive/texlive-latex`, etc.

### Current state after all fixes (2026-06-03)

**`www-client/firefox`**: em=78, portage=78 ✓  
Only difference: `app-text/docbook-xml-dtd-4.4-r3` shown as `N` by em vs `NS` by
portage (`NS` = new in new slot; em does not yet distinguish slot-update installs).

**`app-text/texlive`**: em=93, portage=93 ✓ (after Gap 6 fix)

## Gap 1: VDB awareness — FIXED

136/137 extra packages in em's output were already installed on the system.

**Implementation** (in `portage-cli/src/query/depgraph/` + `portage-atom-pubgrub/src/provider.rs`):

1. **Load VDB** via `Vdb::open_default()` into `Vec<VdbEntry>` (cpn, slot, version).
2. **Exact-CPV filter**: build `HashSet<Cpv>` from VDB; after `install_order()`,
   drop packages whose exact CPV is already installed.
3. **InstalledPolicy::Favor**: register every VDB entry with the solver via
   `add_installed(..., InstalledPolicy::Favor)` so the solver prefers installed
   versions over the newest when the constraint is already satisfied.
4. **Empty-deps fallback** in `add_installed`: when the installed version is NOT
   in the repo (e.g. icu-78.2 installed, only 78.3 in tree), register it with
   empty deps so PubGrub can select it without getting `Unavailable`.  Without
   this, PubGrub rejected the installed version and fell back to the repo's newest.
5. **Action tags**: N (new), U (upgrade), D (downgrade) in pretty + JSON output.
6. **Skip build-time deps for installed versions** in `get_dependencies`
   (`provider.rs`): when PubGrub queries dependencies for a package at exactly
   its installed version, only RDEPEND (1), PDEPEND (3), and IDEPEND (4) are
   returned; DEPEND (0) and BDEPEND (2) are dropped.

7. **`--with-bdeps` / BDEPEND filtering [native parity, 2026-06]:** default-off
   `BDEPEND` exclusion plus per-edge `host_installed` filtering when
   `--with-bdeps` is set. Closes the native offset `@system` over-pull
   (was 316 → now **182**, exact parity with `emerge --with-bdeps=n` = 182,
   verified 2026-06-18). Crossdev dual-root scheduling (`BDEPEND` merge
   to `/`, `RDEPEND` merge to `ROOT`, same CPV twice) is **not** implemented —
   see [root-model.md § BDEPEND / crossdev](../user/root-model.md#bdepend-rdepend-and-with-bdeps).

   Without this, even with `InstalledPolicy::Favor`, the solver still tried to
   satisfy the build-time deps of already-installed packages — pulling in old
   toolchain packages (e.g. the exact old gcc/autoconf needed to build each
   installed package).  Since those packages are already built, their build
   deps are irrelevant at install time.  Portage's "world update" logic does
   the same: it never re-resolves build deps for packages that are not being
   rebuilt.

**Result**: 264 → 68 packages (portage shows 63). After Gap 2 (see below), 70 packages.
Remaining 7 extras are all BDEPEND old-slot issues (see Gap 2 note).

## Gap 2: Profile USE flags — FIXED

`build_use_config()` is now `async` and delegates to `ProfileStack::configure_shell()`,
which sources the full `make.defaults` chain through brush (a real bash runtime),
applies `make.conf` on top, then applies `use.force`/`use.mask`. `USE_EXPAND`
variables (e.g. `CPU_FLAGS_ARM`, `LLVM_TARGETS`) are expanded into flag tokens
by the shell — exactly what portage does.

**Result**: 70 packages (portage: 63). The 7 extras are all BDEPEND old-slot
downgrades that portage satisfies via wrapper packages:
- `autoconf:2.13`, `autoconf:2.69`, `automake:1.17` — satisfied by `autoconf-wrapper`/`automake-wrapper`
- `binutils:2.32`, `gcc:10` — satisfied by `binutils-config`/`gcc-config`
- `rust-bin:1.89`, `rust-bin:1.91` — satisfied by the installed `rust-bin:1.93`

These wrapper/config packages provide symlinks/shims that make any specific-slot
dep resolve to the currently installed version. Teaching our solver about this
would require understanding the wrapper ecosystem, which is out of scope for now.

## Gap 3: Missing transitive deps — under investigation (BLOCKED)

This is the most puzzling gap. Packages like `dev-libs/kpathsea`,
`media-libs/harfbuzz`, `x11-libs/cairo`, `media-libs/fontconfig` are:

- **In the repo** (verified via `ls metadata/md5-cache/...`)
- **Have arm64 keywords** (verified via `grep KEYWORDS`)
- **Individually resolvable** (`em query depgraph dev-libs/kpathsea` → 104 packages OK)
- **Missing as transitive deps** of `app-text/texlive-core`

### What we know

When solving `app-text/texlive-core` directly:
- 186 packages total in output
- Almost all are `virtual/*` packages
- Key concrete deps absent: kpathsea, harfbuzz, cairo, fontconfig, freetype, zziplib, graphite2, potrace, mpfi, ptexenc

When solving each of those directly, they all succeed:
- `dev-libs/kpathsea` → 104 packages ✓
- `media-libs/harfbuzz` → 118 packages ✓
- `x11-libs/cairo` → 118 packages ✓
- `sci-libs/mpfi` → 103 packages ✓

### texlive-core RDEPEND (from metadata cache)

```
sci-libs/mpfi
virtual/zlib:=
>=media-libs/harfbuzz-1.4.5:=[icu,graphite]
>=media-libs/libpng-1.2.43-r2:0=
media-libs/gd[png]
media-gfx/graphite2:=
media-gfx/potrace:=
>=x11-libs/cairo-1.12
>=x11-libs/pixman-0.18
dev-libs/zziplib:=
app-text/libpaper:=
dev-libs/gmp:=
dev-libs/mpfr:=
>=dev-libs/ptexenc-1.4.6
xetex? ( >=app-text/teckit-2.5.10 media-libs/fontconfig )
xindy? ( dev-lisp/clisp:= )
media-libs/freetype:2
>=dev-libs/icu-50:=
>=dev-libs/kpathsea-6.4.0:=
...
```

### Hypotheses (not yet confirmed)

**H1: `:=` slot deps on multi-dep atoms are dropped**

Deps like `>=media-libs/harfbuzz-1.4.5:=[icu,graphite]` combine a `:=` slot
operator with `[icu,graphite]` USE deps. If `portage_atom::Dep::parse` fails on
this combined form, the dep entry would be an `Err` and silently skipped.

The cache parser collects all RDEPEND atoms and does `.collect::<Result<_>>()?` —
a single parse failure drops the ENTIRE package's dep list, not just that one
dep. This could explain why ALL concrete deps of texlive-core vanish simultaneously.

**H2: Version constraint mismatch with `_p` suffix**

Dep: `>=dev-libs/kpathsea-6.4.0:=`  
Available: `kpathsea-6.4.0_p20240311-r1`

In PMS, `_p` (patch) makes a version GREATER than the base. So
`6.4.0_p20240311-r1 > 6.4.0`. If portage-atom's `VersionSet` comparison
disagrees and treats `6.4.0_p20240311-r1 < 6.4.0`, then the constraint has no
satisfying versions and the dep is silently dropped in the post-processing step.

**H3: texlive-core itself fails to parse**

If `cache_entries()` returns `Err(...)` for texlive-core, it gets silently skipped
(`let Ok(entry) = entry else { continue }`). Then texlive-core is absent from
`data.versions`, and the target package produced by `target_package()` is
`PortagePackage::unslotted(texlive-core)` which has no registered versions. The
solver would then find no versions for the root dep — but we DO get output, so
the solver doesn't hard-fail. Something in `resolve_targets` must handle this.

Note: `app-text/texlive-core` is in the "portage but NOT em" list for texlive-core's
own depgraph output — texlive-core itself doesn't appear in our install_order,
which is very unusual since the target package should always be in the output.

### Test results (all hypotheses falsified)

Added targeted tests in `portage-atom` and `portage-metadata`:

**H1 — FALSIFIED**: `Dep::parse(">=media-libs/harfbuzz-1.4.5:=[icu,graphite]")`
parses correctly. All 14 atom forms from texlive-core's RDEPEND parse without
error (`dep::tests::texlive_core_rdepend_atoms_parse` passes). The `:=` combined
with `[use,use]` is handled correctly by the existing parser.

**H2 — FALSIFIED**: `6.4.0_p20240311-r1 >= 6.4.0` is TRUE. The version
ordering tests (`ge_constraint_matches_p_suffix_versions`) confirm that `_p`
with large date numbers correctly sorts above the base version. `12.3.2 >= 1.4.5`
is also TRUE (`ge_constraint_large_major_matches`).

**H3 — FALSIFIED**: The texlive-core cache entry (with SLOT=0/6.4.0, exotic
keywords `~x64-macos ~x64-solaris`, and `:=[icu,graphite]` in RDEPEND) parses
without error (`cache::tests::texlive_core_cache_entry_parses` passes). The
kpathsea entry also parses correctly including the subslot.

### Conclusion: the root cause is elsewhere

All three hypotheses are wrong. The bug must be in `portage-atom-pubgrub`'s
`PortageDependencyProvider`, specifically in how it converts deps or evaluates
version constraints during solving — NOT in the atom/cache parsers.

### Root cause found: BDEPEND bootstrap cycle in `install_order`

The missing packages (kpathsea, harfbuzz, texlive-core) were in the **solution**
and even had **correct edges** in the dependency graph — but they never appeared
in `install_order` because `install_order`'s topological sort deadlocked.

Confirmed via targeted debug instrumentation:

```
debug install_order in_degree[dev-libs/kpathsea:0-6.4.0_p20240311-r1] = 1
debug install_order in_degree[app-portage/elt-patches:0-20250718] = 1
debug kpathsea DEPEND/BDEPEND edge: kpathsea -> app-portage/elt-patches:0-20250718 (class=Bdepend)
debug install_order: 264 in solution, 87 start queue size (in_degree=0)
debug install_order: 78 stuck packages
```

**The cycle:**

- `dev-libs/kpathsea` BDEPENDS on `>=app-portage/elt-patches-20250306`
- `app-portage/elt-patches` BDEPENDS on `app-arch/xz-utils`
- `app-arch/xz-utils` BDEPENDS on `>=app-portage/elt-patches-20250306`

→ `elt-patches ↔ xz-utils` is a classic bootstrap cycle. Portage resolves it
by assuming one of them is already installed on the build host. Our topological
sort deadlocked because both had `in_degree=1` waiting for each other.

**Fix:** after the main topological sort (Kahn's algorithm), any packages
remaining in `key_of` are part of cycles. Instead of silently dropping them,
they are appended to the result in a deterministic (sorted) order. This mirrors
portage's behavior of breaking circular deps rather than refusing to produce
output.

Note: xz-utils-5.8.3 genuinely `INHERIT`s the `libtool` eclass
(`INHERIT=...libtool...`), so its BDEPEND on elt-patches is real, not a
metadata error. The cycle is a real bootstrap cycle. VDB awareness (Gap 1)
would naturally resolve it by recognizing both packages as already installed.

After the fix: 264 packages in output (vs 186 before), including kpathsea,
harfbuzz, cairo, freetype, and texlive-core.

## Gap 3b: USE flag expansion — FIXED

USE_EXPAND flags (`abi_x86_*`, `perl_features_*`, `llvm_slot_*`, etc.) are now
grouped under their key name.  `USE_EXPAND` variable is read from the brush shell
alongside `USE` in `compute_use_env()`, then `format_flags()` splits each IUSE
flag into base USE or the matching USE_EXPAND group.

Example: `USE="-test"  PERL_FEATURES="-debug -ithreads -quadmath"  ABI_X86="-32 -64 -x32"`

Remaining minor diff vs portage: portage uses `(-flag)` for flags added to a
package's effective IUSE by `USE_EXPAND_IMPLICIT` rather than explicit `IUSE`
declaration. We show them without parens since we only see the explicit IUSE from
the metadata cache. This is a display-only difference with no correctness impact.

Also fixed: action tag is now slot-aware. Cross-slot installs (e.g. rust-bin:1.89
alongside installed rust-bin:1.93) correctly show `N` instead of `D`.

## Gap 4: USE dep branch selection — FIXED

### Root cause

OR-group alternatives that carry USE dep constraints (e.g. librsvg's BDEPEND
`|| ( (python:3.14 docutils[python_targets_python3_14(-)]) (python:3.13 docutils[python_targets_python3_13(-)]) )`)
were silently ignoring those constraints during branch selection.

Two concrete bugs:

1. **`convert_choice_group()` leaked USE deps to parent** (`convert.rs`):
   `self.use_deps` was not saved/restored per branch, so USE dep constraints from
   inside an OR branch accumulated in the parent's dep list (where they were
   harmlessly stored but never consulted by the solver).

2. **`choose_version()` didn't check USE dep satisfiability** (`provider.rs`):
   For `Choice` virtual packages, when multiple branches all had installed packages
   (the "all installed" fall-through), the solver returned `max()` = first listed
   alternative without checking whether each branch's USE dep constraints were
   already satisfied by the installed state.

### Fix

- `VirtualChoice` now carries `branch_use_deps: Vec<(Version, Vec<UseDepConstraint>)>`,
  populated per-branch by saving/restoring `self.use_deps` in `convert_choice_group`.
- `InstalledPackage` gains `active_use: Vec<Interned<DefaultInterner>>` (enabled USE
  flags at build time, read from the VDB `USE` file).
- `PortageDependencyProvider` stores `installed_use: HashMap<PortagePackage, Vec<...>>`.
- `register_virtual_choices()` populates `PackageData.use_deps` from `branch_use_deps`
  so per-branch constraints are accessible during `choose_version`.
- `choose_version()` for `Choice` packages: computes `installed_and_use_sat` —
  installed branches where all USE dep constraints pass the PMS 8.3.4 check.
  If only a subset satisfy, the solver picks the highest-version satisfied branch
  instead of falling through to `max()`.

### Why the Firefox gap is now addressed

The earlier analysis assumed our solver always picked python:3.13 (the
"minimize rebuilds" branch).  That was true before `use_dep_branch_satisfied`
considered the USE config's **intended** state.

**Current behavior** — two-phase resolution:

**Phase 1 (branch selection in `choose_version`):**  
`use_dep_branch_satisfied` evaluates each OR-group branch using:
```
dep_effective_enabled = active.contains(flag)
                     || (flag in IUSE && use_config says enabled)
```
The profile's `PYTHON_TARGETS` expands (via `USE_EXPAND`) into the `UseConfig`:
both `python_targets_python3_13` and `python_targets_python3_14` are enabled.
Therefore docutils's python:3.14 branch constraint IS satisfied (even though the
currently-installed docutils was built without `python_targets_python3_14`), and
so is the python:3.13 branch.  Both branches are use-satisfied → falls through
to `max()` → **python:3.14 is selected** (first listed = highest synthetic
version).

**Phase 2 (reinstall detection in `compute_use_flag_requirements`):**  
After picking python:3.14, the virtual Choice node's `use_deps` at that version
contain `docutils[python_targets_python3_14(-)]`.  The post-solve pass evaluates
this against the *current installed* state: docutils is installed without
`python_targets_python3_14` active → **violation detected → docutils flagged `R`**.

The same mechanism fires for any other python ecosystem package in the solution
that has a `[python_targets_python3_14(-)]` USE dep constraint imposed by some
package in the tree.  Whether all 8 portage-reported rebuilds are caught depends
on the tree's specific USE dep edges; constraint-driven detection covers the
packages actually constrained by the resolved set.

The **distinction from a full `--newuse` scan**: our pass only flags installed
packages with explicit USE dep violations coming from other packages in the
solution.  A full `--newuse` scan would also flag installed packages where
`use_config`-enabled flags are absent from `active_use` regardless of whether
any dep chain explicitly requires them.  The constraint-driven approach is
sufficient for correctness; a broader scan is a possible future enhancement.

## What has already been fixed (this session)

- `build_use_config()` — reads global `USE` from make.conf into `UseConfig`
- `report_dropped_deps()` — splits "truly missing" vs "arch-filtered" drops
- `target_package()` — filters by arch-compatible versions when picking slot
  (fixes NoSolution crash on multi-slot packages like gcc)
- `fetch_all()` — parallel downloads with `buffer_unordered(max_concurrent=4)`
- `install_order()` — added cycle detection: after Kahn's topological sort,
  packages stuck in cycles (e.g. elt-patches ↔ xz-utils libtool bootstrap
  cycle) are appended in sorted order rather than silently dropped; freed
  78 packages including kpathsea, harfbuzz, cairo, texlive-core
- **Gap 1 (VDB awareness)**: 264 → 68 packages (portage: 63); see Gap 1 section
  - `load_installed()` reads all VDB entries (cpn, slot, version)
  - Exact-CPV post-filter removes already-installed packages
  - `InstalledPolicy::Favor` makes solver prefer installed versions
  - `add_installed()` now registers the version with empty deps if not in repo,
    preventing PubGrub from rejecting the installed version as Unavailable
  - `get_dependencies()` skips DEPEND/BDEPEND for packages at their installed
    version; only RDEPEND/PDEPEND/IDEPEND are returned (installed packages are
    already built — re-solving their build deps pulls in old toolchain packages)
- **Gap 2 (Profile USE flags)**: now 70 packages (portage: 63); see Gap 2 section
  - `build_use_config()` is now async, delegates to `ProfileStack::configure_shell()`
  - brush (real bash) sources `make.defaults` chain + `make.conf`, applies
    `use.force`/`use.mask`, expands `USE_EXPAND` vars — exactly what portage does
  - `depgraph()` and `run_query()` made async to support this
- **Gap 3b (USE_EXPAND display)**: flags grouped as `PERL_FEATURES="..."`, `ABI_X86="..."` etc.
  - slot-aware action tags: cross-slot `N` (was wrongly `D`); added `R` for reinstall
- **Gap 4 (USE dep branch selection)**: OR-group branches now evaluated for USE dep
  satisfiability; solver prefers branches whose USE dep constraints are already met
  by the installed state rather than blindly picking `max()` (first listed)
  - `convert_choice_group` saves/restores `self.use_deps` per branch (no more leaking)
  - `InstalledPackage.active_use` carries enabled USE flags from VDB; stored in
    `PortageDependencyProvider.installed_use`
  - `eval_violated_use_dep()` / `use_dep_branch_satisfied()` implement PMS 8.3.4
    default semantics `(-)`/`(+)`
  - `choose_version()` for `Choice` packages checks `installed_and_use_sat` and
    prefers the highest-version branch that is both installed and use-satisfied
- **Gap 5 (post-solve reinstall detection)**: after solving, installed packages whose
  USE dep constraints are violated by the resolved set are collected as `R` entries
  - `compute_use_flag_requirements()` walks the full PubGrub solution (including
    virtual choice nodes) to catch per-branch USE dep constraints
  - Called inside `resolve_targets` before virtuals are filtered
  - Exposed via `reinstall_deps()` accessor (returns only the violated-installed subset)
  - `depgraph.rs` appends these to the install order (deduped against packages
    already being upgraded)

## Gap 6: Profile USE accumulation — FIXED

**Symptom**: `net-dns/libidn` missing from `em query depgraph app-text/texlive`.
`ghostscript-gpl` has `unicode? ( net-dns/libidn:= )` in DEPEND/RDEPEND.
Portage enables `unicode`; em did not.

**Root cause**: Portage accumulates `USE=` lines across the profile stack
additively — each `make.defaults` level's `USE=` is a delta, not a bash
assignment. Our original `compute_use_env` sourced all files through a single
brush session, so a later `make.defaults` that set `USE="crypt ipv6 ..."` (without
`${USE}`) overwrote the base profile's `USE="acl bzip2 gdbm unicode"`.

The same problem affected `make.conf`: `USE="npm dist ..."` without `${USE}`
also replaced all profile-derived flags.

**Fix** (`portage-repo/src/build/profile.rs`):

`ProfileStack::profile_env(shell)` — new async method that sources each
`make.defaults` through the **same** brush shell (preserving cross-file variable
visibility for non-incremental vars) but with per-layer USE isolation:

1. Before each file: reset `USE`, `USE_EXPAND`, and all known `USE_EXPAND` key
   vars to empty in the shell (so the file's assignments are its pure delta)
2. Source the file through brush (all bash features available)
3. After sourcing: capture the file's contributions and merge into an external
   accumulator using `merge_flag_lists` (portage-style incremental semantics)
4. Restore the accumulated state into the shell for the next file to reference

The same `source_incremental` helper is applied to `extra_confs` (make.conf),
so `USE="npm dist"` in make.conf correctly ADDS npm/dist to the profile USE
rather than replacing it.

Result: `unicode` (set by `profiles/base/make.defaults`) survives through all
profile levels and `make.conf`. `ghostscript-gpl` shows `unicode` enabled →
`libidn` is included → em=93 = portage=93 ✓

`ProfileEnv` (with per-layer `ProfileEnvLayer`) is kept as a value type for
debugging: callers can inspect which make.defaults file contributed which flags
before the collapse.
