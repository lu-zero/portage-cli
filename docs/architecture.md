# Architecture

The main architecture reference for this workspace. It describes the crate
ecosystem, the `em -p` resolution pipeline, USE stacking, post-solve
validation, and known divergences from emerge.

> **Slop warning.** This codebase is largely AI-generated. Verify a claim
> against the code before relying on it; update this file when it drifts.

Related: [`docs/testing.md`](testing.md) (how correctness is established),
[`docs/benchmarks.md`](benchmarks.md) (how performance is measured).

## Crate layering

Lower crates know nothing of higher ones. The edges below are the real
`Cargo.toml` dependencies.

```
gentoo-interner ─┐
                 ├─ gentoo-core ──────────── gentoo-stages
portage-atom ────┤
   │             ├─ portage-metadata ─┐
   │             │                    ├─ portage-repo ───── portage-distfiles
   ├─ portage-solver ─┬─ portage-atom-pubgrub ─────────────┐
   │                  └─ portage-atom-resolvo ─────────────┤
   │                                                       │
   │                  portage-binpkg ───────────────────────┤
   │                  portage-vdb ──────────────────────────┤
   └──────────────────────────────────────────────────────── portage-cli (em)
```

`portage-bench` (in `benchmarks/`) depends on both solver bridges plus
`portage-repo` for benchmarking. See [`docs/benchmarks.md`](benchmarks.md)
for how to run benchmarks across the workspace.

## Crate catalog

### Published on crates.io

| Crate | Version | Purpose |
|-------|---------|---------|
| `gentoo-interner` | 0.3.1 | String interning |
| `gentoo-core` | 0.5.1 | Architecture types, variants |
| `portage-atom` | 0.10.0 | PMS atom parsing |
| `portage-metadata` | 0.8.0 | md5-cache entry parsing, EAPI, keywords |
| `portage-atom-pubgrub` | 0.6.0 | PubGrub solver bridge (default in `em`) |
| `portage-atom-resolvo` | 0.7.1 | SAT dependency solver (resolvo bridge) |
| `portage-solver` | 0.1.0 | Solver-agnostic trait and shared vocabulary |
| `portage-vdb` | 0.1.0 | Installed package database (`/var/db/pkg`) |
| `portage-binpkg` | 0.1.0 | GPKG binary package read/write |
| `gentoo-stages` | 0.5.1 | Stage3 tarball fetch/cache |

### Local only (`publish = false` in workspace)

| Crate | Version | Purpose | Blocker |
|-------|---------|---------|---------|
| `portage-repo` | 0.1.0 | Repo layout, ebuilds, profiles, manifests | Depends on `brush-*` (not on crates.io) |
| `portage-distfiles` | 0.1.0 | Source distfile fetching & resolution | Workspace-only for now |
| `portage-bench` | 0.1.0 | Benchmark harness | Dev tool, not a library |
| `portage-cli` | 0.1.0 | The `em` binary | Unpublished binary crate |

## Per-crate public API

### `gentoo-interner` (v0.3.1)

String interning foundation. Default backend is **papaya** (concurrent
hash map); `lasso` and `symbol-table` backends available as feature flags.

- `trait Interner` — `get_or_intern(&str) -> Key`, `resolve(&Key) -> &str`
- `struct Interned<I>` — interned string key, `Deref<Target=str>`, `Display`
- `struct NoInterner` — non-interning fallback (Key = `Box<str>`)
- `struct GlobalInterner` *(feature: interner, default)* — process-global interner
- `type DefaultInterner` — alias: `GlobalInterner`

### `gentoo-core` (v0.5.1)

Architecture and release-variant types.

- `enum KnownArch` — 18 official Gentoo architectures: `as_keyword()`, `parse()`, `current()`
- `struct Arch<I>` — known or exotic architecture: `from_chost()`, `as_str()`
- `type ExoticKey<I>` — alias for `Interned<I>`
- `struct Variant<I>` — release media variant (`arch-flavor`): `parse()`, `flavor()`

### `portage-atom` (v0.10.0)

PMS atom parser — the vocabulary every other crate speaks.

- `struct Cpn` — Category/Package Name (`dev-lang/rust`)
- `struct Cpv` — Category/Package/Version (`dev-lang/rust-1.75.0`)
- `struct Pf` — Package-version string (`rust-1.75.0`)
- `struct Dep` — Full dependency atom with blocker, operator, version, slot, USE, repo
- `enum Blocker` — `Weak` (!) or `Strong` (!!)
- `enum DepEntry` — Dependency tree node: `Atom`, `UseConditional`, `AllOf`, `AnyOf`, `ExactlyOneOf`, `AtMostOneOf`
- `struct Version` — PMS version with suffixes and revision: `glob_matches()`, `base()`
- `struct Revision(u64)` — Package revision (`-rN`)
- `enum Operator` — `<`, `<=`, `=`, `~`, `>=`, `>`
- `struct Suffix` / `enum SuffixKind` — Version suffix segment (`Alpha`, `Beta`, `Pre`, `Rc`, `Post`)
- `struct Slot` — Slot + optional subslot
- `enum SlotDep` / `enum SlotOperator` — `:=`, `:*`
- `struct UseDep` — USE flag constraint
- `enum UseDepKind` — `Enabled`, `Disabled`, `Conditional`, `Equal`, etc.
- `enum UseDefault` — `None`, `Enabled`, `Disabled`
- Builder types *(feature: `builder`)*: `CpnBuilder`, `CpvBuilder`, `DepBuilder`, `SlotBuilder`, `UseDepBuilder`, `SuffixBuilder`, `VersionBuilder`
- Re-exports `gentoo_interner as interner`

### `portage-metadata` (v0.8.0)

Ebuild metadata cache parser.

- `struct CacheEntry<I>` — Parsed md5-cache entry: `parse()`, `from_kv_pairs()`, `serialize()`
- `struct RawCacheEntry<I>` — Unparsed raw cache entry
- `struct EbuildMetadata<I>` — 21 metadata fields (eapi, description, slot, homepage, src_uri, license, keywords, iuse, required_use, restrict, properties, depend, rdepend, bdepend, pdepend, idepend, inherit, inherited, defined_phases)
- `enum Eapi` — EAPI 0–9 with feature-query methods
- `enum Phase` — 15 ebuild phase functions
- `struct Keyword<I>` / `enum Stability` — `Stable`, `Testing`, `Disabled`, `DisabledAll`
- `struct IUse<I>` / `enum IUseDefault` — USE flag with default (+/-)
- `struct LicenseExpr`, `struct RequiredUseExpr`, `struct RestrictExpr`, `struct SrcUriEntry`
- Re-exports `portage_atom::interner`

### `portage-solver` (v0.1.0)

Solver-agnostic vocabulary shared by both solver bridges.

- `trait Solver` — single abstraction both bridges implement
- `trait PackageRepository`, `struct VersionFacts`, `struct PackageDeps` — facts fed to a solver
- `struct UseConfig`, `enum UseFlagState` — per-package resolved USE policy (computed by consumer, not solver)
- `struct SelectedPackage`, `struct DepEdge`, `struct TargetSpec` — solution/plan vocabulary in Portage terms
- `enum RequiredUse` — REQUIRED_USE encoding vocabulary
- Depends only on `portage-atom` and `thiserror`; no pubgrub or resolvo

### `portage-atom-resolvo` (v0.7.1)

SAT-based dependency solver bridge using resolvo.

- `struct PortageDependencyProvider` — Main solver bridge: `new()`, `with_installed()`, `dependency_graph()`, `install_order()`
- `struct PortagePool` — Arena storage for solver IDs
- `struct PackageMetadata` — Per-version metadata (cpv, slot, iuse, use_flags, repo, deps)
- `struct PackageDeps` — 5 dep classes: depend, rdepend, bdepend, pdepend, idepend
- `struct UseConfig` — USE flag evaluation: enabled, disabled, solver_decided
- `enum DepClass`, `struct DepEdge` — Dependency classification and graph edges
- `enum InstalledPolicy`, `struct InstalledSet` — Installed package handling
- `trait PackageRepository` — `all_packages()`, `versions_for()`
- `struct InMemoryRepository` — HashMap-backed test impl
- `fn version_matches()` — PMS version matching

### `portage-atom-pubgrub` (v0.6.0)

PubGrub-based dependency solver bridge — the solver `em` uses by default.

- `struct PortagePackage` — Solver package identity: `Unslotted`, `Slotted`
- `struct PortageVersionSet` — Wraps `Ranges<Version>` for pubgrub's `VersionSet` trait
- `struct PortageDependencyProvider` — Main solver bridge: `new_for_targets()`, `resolve_targets()`, `dependency_graph()`, `install_order()`
- `enum InstalledPolicy`, `struct InstalledPackage`, `struct DroppedDep` — Installed package handling
- `struct UseConfig`, `enum UseFlagState` — Per-package USE configuration
- `struct CededFlag`, `struct UseFlagRequirement` — Level-C autosolve state
- `struct PackageDeps`, `struct PackageVersions` — Per-version facts
- `trait PackageRepository` — `all_packages()`, `versions_for()`, `desired_use()`
- `struct InMemoryRepository` — HashMap-backed test impl
- `enum RequiredUse` — REQUIRED_USE expression for solver encoding
- `struct SlotOperatorBinding` — `:=` binding tracking for rebuild detection
- `enum DepClass`, `struct DepEdge` — Dependency classification and graph edges
- `fn apply_package_use()` — Per-package `package.use` application
- Re-exports `DefaultInterner`, `Interned` from `portage_atom::interner`

### `portage-repo` (v0.1.0)

Repository layout reader — reads a Gentoo repository from disk. The most
complex library crate. Depends on `brush-*` (embedded bash shell) via local
paths for ebuild sourcing and `make.conf` parsing.

- `struct Repository` — Main entry point: `open()`, `name()`, `layout()`, `categories()`, `ebuilds()`, `cache_entry()`, `profiles()`, `arch()`
- `struct Category`, `struct Package`, `struct Ebuild` — Directory hierarchy
- `struct Ebuilds` / `EbuildsIter` — Lazy ebuild discovery with filtering
- `struct LayoutConf` — `metadata/layout.conf` parser
- `struct Manifest` / `ManifestEntry` — `Manifest` file parser (BLAKE2/SHA256/MD5)
- `struct PkgMetadata` — `metadata/pkg_desc_index` + `metadata.xml` parsing
- `struct Profile` / `ProfileDesc` / `ProfileStack` / `ProfileStatus` — Profile resolution
- `struct ProfileEnv` / `ProfileEnvLayer` — Per-layer profile variable tracking
- `struct EbuildShell` — Embedded bash shell via brush for ebuild sourcing
- `struct UseExpand` / `struct UseFlags` — USE_EXPAND handling, effective flag set
- `struct MakeConf` — `make.conf` round-trip editing (byte-precise via comment spans)
- `struct PackageConf` / `PackageToken` — `package.use`/`package.keywords`/etc. parsing
- `struct ReposConf` / `RepoEntry` — `repos.conf` parsing
- Cache module: `regen_cache()`, `cache_entries_parallel()`, `CacheReadOpts`, `RegenOpts`, `RegenStats`
- Source module: `source_ebuild()`, `source_single()`, `source_parallel()`, `SourceContext`, `SourceOpts`
- Re-exports from `gentoo_core`: `Arch`, `KnownArch`, `ExoticKey`

### `portage-vdb` (v0.1.0)

Installed package database reader/writer for `/var/db/pkg`.

- `struct Vdb` — Main entry point: `open()`, `open_default()`, `owner()`, `find_collisions()`, `register()`, `unregister()`, `find_slot_occupant()`
- `struct InstalledPackage` — Rich accessor: cpv, slot, eapi, USE flags, deps, contents, etc.
- `struct ContentsEntry` / `enum ContentsKind` — Parsed CONTENTS entries (obj/dir/sym/fifo/dev)
- `fn format_contents()` — Serialize contents back to VDB format
- `struct Collision` — File collision between planned and installed packages
- `struct MergeSpec` — Specification for registering a new installed package
- Directory iterators: `AllPackages`, `Category`, `Categories`, `Packages`

### `portage-binpkg` (v0.1.0)

Gentoo binary package (GPKG) read/write per [GLEP 78](https://www.gentoo.org/glep/glep-0078.html).

- `fn write_gpkg()` — GPKG container writer (GNU `tar` + `zstd`)
- `fn read_metadata()` — read GPKG metadata without full extraction
- `fn extract_image()` — extract installed image from a GPKG
- `struct GpkgInput` — input specification for writing
- Used by `em` for `-b`/`--buildpkg`, `-k`/`--usepkg`, and `-g`/`--getbinpkg`

### `portage-distfiles` (v0.1.0)

Source distfile fetching and resolution.

- `struct DistfileResolver` — Resolves `SRC_URI` entries to `Distfile` structs with mirror expansion
- `struct Distfile` — A single distfile: filename, URLs, fetch restriction
- `fn collect_filenames()` — Extracts filenames from `SRC_URI` + USE flags
- `struct Fetcher` — Downloads distfiles (builtin HTTP or external command)
- `struct FetchConfig` / `enum FetchStrategy` / `enum FetchStatus` — Fetch configuration and result

### `gentoo-stages` (v0.5.1)

Stage3 tarball fetch and cache management.

- `struct Stage3` — Stage3 image info: `is_cached()`, `file_path()`
- `struct Client` / `ClientBuilder` — HTTP client for mirror listings
- `struct Cache` — Local filesystem cache

## Target derivation: argv → request

A command's targets are lowered to a single canonical **request**: a synthetic
`Root` package whose dependencies are the resolved target atoms, plus a **mode**.
A single target is just the one-element case:

| invocation        | request                                            |
|-------------------|----------------------------------------------------|
| `em -p gcc`       | `Root([sys-devel/gcc], Default)`                   |
| `em -p gcc clang` | `Root([sys-devel/gcc, llvm-core/clang], Default)`  |
| `em -up …`        | `Root([…], Update)`                                |
| `em -ep …`        | `Root([…], EmptyTree)`                             |

The request is resolved by **one joint solve** over Root's dependencies — not by
solving each target separately. For independent targets the plan is the union of
the per-target plans (verified for `-p` and `-up`); when targets share a dep with
conflicting constraints the joint solve reconciles them. This matches emerge.

Two stages produce and consume the request:

- **input → request** (portage-cli, `portage-atom` types): expand `@sets`,
  disambiguate each token to a canonical `Dep` (category + slot + version + USE),
  resolve it to a precise package identity, attach the mode and per-target
  disposition.
- **request → resolver query** (`portage-atom-pubgrub`): Root's atoms go through
  the *same* `convert_deps` as ebuild dependencies, so slot/version/USE-dep
  semantics are identical to any other edge.

Intended target semantics (all match emerge):

- An **explicit target pulls the best in-slot version** even without `-u`
  (`em -p gcc` → newest accepted `gcc:16`, listed `[U]`), and is reinstalled when
  already at best (`[R]`). A *dependency* on the same atom instead favours the
  installed version.
- A **bare command-line target denotes the best accepted version** of the matched
  set = its newest accepted slot (`em -p python` ≡ `em -p python:3.14`; `python:*`
  likewise). Multi-slot is not an ambiguity; it is a deterministic best-slot pick.

### Ambiguity and partial-failure policy (intentional divergences)

- **Category ambiguity** — a bare name matching several categories (e.g. `clang`
  → `dev-python/clang`, `llvm-core/clang`): install-type operations error and
  list the candidates (`ResolveMode::Error`); update operations (`-u`) take the
  installed candidate when exactly one is installed
  (`ResolveMode::PreferInstalled`, with a warning). **emerge always errors** on an
  ambiguous short name regardless of what is installed — em is deliberately more
  lenient under `-u`.
- **Multi-target with one unresolvable atom** — em drops the bad atom with a
  warning and proceeds with the rest, erroring only when *all* fail. **emerge
  aborts the whole command.**

Slot/version-qualified targets (`em -p python:3.13`, `=python-3.13*`) honour the
qualifier: `target_package` (`repo.rs`) resolves the target slot from the newest
accepted version that `matches_cpv` the atom, so a bare name / `:*` picks the
newest slot while `:slot` / `=…-ver*` pin the matching one.

## The `em -p` / `em query depgraph` pipeline

`em -p` and `em query depgraph` share one path (`query/depgraph/mod.rs`).
Stages, in order:

1. **Load facts** (`repo.rs`) — parse the repo's md5-cache into `RepoData`
   (CPN → versions → `CacheEntry`), filtered by keywords/mask/license.
2. **Build the USE environment** (`use_env.rs` → `portage-repo`) — see
   [USE stacking](#use-stacking-precedence) below. Produces the global
   `UseConfig`, `package.use`, `USE_EXPAND` groups, masks, `ACCEPT_KEYWORDS`,
   `ACCEPT_LICENSE`.
3. **Load installed set** (`installed.rs`) — the VDB, used for `InstalledPolicy`
   (`Favor`/`Lock`, or `Rebuild` under native `--emptytree`), action tags
   (`N`/`R`/`U`/`D`), and reverse-dep checks. Under `--emptytree` the real VDB
   stays loaded (for tags/display) but the solver sees an empty installed set so
   target packages are re-selected as rebuilds.
4. **Build the provider** (`PortageDependencyProvider::new_for_targets(adapter, seeds)`)
   — the cli `Adapter` implements `PackageRepository`, handing the solver each
   version's facts (`versions_for`) and its resolved **desired** USE (`desired_use`).
5. **Resolve** (`resolve_targets`) — PubGrub selects one version per package,
   modelling OR/`^^`/`??` groups, slots/subslots, USE-conditional deps, and
   USE-dep constraints (the latter via virtual `UseDecision` packages). When the
   post-solve pass decides to upgrade an installed package to a newer version
   (`upgrade_to`), `resolve_targets` pins that version and re-solves to a
   (bounded) fixpoint.
6. **Slot-operator rebuild detection** (`subslot.rs`) — VDB-recorded `:=` bindings
   of installed consumers are checked against the plan; a dependency moving across
   a subslot boundary pulls the consumer in as a same-version rebuild.
7. **Post-solve checks** — see [Post-solve validation](#post-solve-validation).
8. **Install order** (`install_order`) — SCC condensation (iterative Tarjan) +
   lexicographic Kahn; hard (DEPEND/BDEPEND) edges before soft (RDEPEND); cycles
   broken on soft edges. Explicitly-requested targets are listed last when
   nothing depends on them (emerge convention).
8b. **Post-order rewrite** — for everything except native `--emptytree`,
    `--with-bdeps` triggers the within-run BDEPEND trim (`bdepend_trim.rs`),
    dropping edges already satisfied on BROOT or by earlier plan entries. Native
    `--emptytree` skips the trim: the provider returns the full deep closure
    straight from the solve (`rebuild_tree` ⇒ un-pruned `vd.merged`), so there is
    no post-solve re-list (see `todo/em-emptytree.md`).
9. **Render** (`output.rs`) — `pretty` (emerge `-p`/`-pv`), `json`, or `tree`.
   Verbose `-pv` also shows per-package download size and a "Size of downloads"
   total (`download_size.rs`): distfiles from each package's `Manifest`,
   restricted to what `SRC_URI` needs for the effective USE, minus those already
   in `DISTDIR`, deduplicated across the plan.
10. **Advisory warnings** — emitted *after* the plan (emerge lists issues at the
    bottom), see [Post-solve validation](#post-solve-validation).

Stages 1–3 run concurrently via `tokio::join!`.

## USE stacking precedence

This is the part most easily gotten wrong, so it is pinned here. `em` resolves a
package's effective USE in the same incremental order Portage does
(low → high precedence; later layers override earlier, `-flag` removes) —
matching Portage's `USE_ORDER` (`env` beats `pkg`):

1. IUSE defaults (`+flag` / `-flag` on the ebuild)
2. `make.globals` + profile `make.defaults` + `make.conf` (`pre_env` fold)
3. `package.use` (profile + `/etc/portage/package.use`)
4. **the `USE` environment variable** (and each `USE_EXPAND` key from the
   process env, e.g. `PYTHON_TARGETS=...`) — env wins over package.use
5. **post-fold** `use.force` / `use.mask` and per-package
   `package.use.force` / `package.use.mask` (plus `*.stable.*` for
   stable-keyword merges) — unconditional, not smuggled through package.use
6. Level-C ceded flags under `--autosolve-use` (also post-fold)

Portage also appends **`/etc/portage/profile/`** as a *site-local profile layer*
on top of the resolved `make.profile` chain (portage(5),
`LocationsManager`'s `CUSTOM_PROFILE_PATH`) — a flat node whose own `parent` file
is not followed. `ProfileStack::with_user_profile` folds it in, so its
`make.defaults` (layer 2), `package.use` (layer 5) and `use.force`/`use.mask`
(layer 6) all take effect at the *highest* priority. Per PMS 5.2.4 any of these
profile files may be a *directory* whose regular files are concatenated in
filename order (`/etc/portage/profile/package.use.mask/<name>` is the common
case); `read_lines` handles both forms.

The `pre_env` fold (layer 2 above) is computed in `portage-repo`'s
`resolve_use_flags` (`build/profile.rs`): the profile chain and `make.conf` are
sourced through the embedded shell. Process-env `USE`/`USE_EXPAND` is applied
later as layer 4 inside `resolve_effective_use` (not baked into `pre_env`), so
`USE="-X" em -p www-client/firefox` enters the stack, and force/mask (layer 5)
can still pin a flag back on afterward.

Within the profile/`make.conf` portion of `pre_env`, `USE_EXPAND`/
`USE_EXPAND_UNPREFIXED` variable values are translated into USE tokens
(`ELIBC="glibc"` → `elibc_glibc`, `ARCH="amd64"` → `amd64`) and folded at that
layer's position, exactly as Portage's `config.py` `regenerate()` does. This is
how the implicit `elibc_*`/`kernel_*` flags and profile defaults like
`python_targets_*` reach every per-package `resolve_effective_use` fold.

Layer 3 (`package.use`) is applied **per package** at solve/display time.
Layer 5 force/mask is applied as a true post-fold step by `force_mask.rs`
(`ForceMask::apply`): force enables, mask disables (mask wins), overriding
package.use and env. The `*.stable.*` sets apply only when the version is
"merged due to a stable keyword" (`AcceptKeywords::is_stable`). The same
`apply_force_mask` path is used by `Adapter::desired_use` (solver),
`effective_use::effective_use` (display / `PlannedMerge` / REQUIRED_USE /
download-size), so they cannot disagree. The solver itself never recomputes any
of this; it consumes the resolved `desired` set (see the
[USE/solver boundary doc](../portage-atom-pubgrub/docs/use-and-solver-boundary.md)).

**Layer 6 (`em`-only, no PMS equivalent): `--autosolve-use` ceded flags.**
When a package's `REQUIRED_USE` is violated, `cede_required_use` hands its
flags to the solver as preferences (`UseFlagState::SolverDecided`); once the
solve picks a final value, `effective_use::apply_ceded` applies it as a
**final, unconditional override** — sitting above even layer 6, applied
*after* the whole layers-1–6 fold (including any `-*`) has already run, at
every place that needs the real post-solve USE (the merge plan, the
`REQUIRED_USE` check, download-size, and the `-p` display). This has to sit
above the fold rather than be folded in as a `package.use` entry (layer 5):
a ceded flag's entire purpose is to repair a violation an env-level `-*`
caused, so if it were subject to the same fold, that same `-*` would wipe it
right back out — which is exactly the bug this layer was added to fix
(`em stages --stage1`'s `USE="-* build"` silently defeated `--autosolve-use`
for every package it touched until this landed, 2026-07-12).

## Post-solve validation

The solver decides *versions*; several constraints are intentionally **not**
modelled inside it and are checked after a solution exists. All of these are
**advisory** (the plan is still produced) and are printed *after* the merge list,
so the plan reads first and the caveats follow — as emerge does. Some live in the
solver crate (they read its `VersionData`), some in the cli (they need only a
package's own facts):

| Check | Where | Notes |
|---|---|---|
| USE-dep constraints (`[flag]`, `[flag?]`, `[flag=]`) | crate `validate.rs` | `check_use_deps` |
| Blockers (`!foo` / `!!foo`) | crate `validate.rs` | `check_blockers`; evaluates the blocker's own USE condition to avoid false positives |
| `::repo` constraints | crate `validate.rs` | `check_repo_constraints` |
| Reverse-dependency conflicts | cli `conflicts.rs` | complete-graph check (every installed pkg's deps vs the plan) that a default `emerge -p` skips; advisory, reported as "Dependency constraint conflict" |
| `REQUIRED_USE` | cli `required_use.rs` | **Level A** — see below |

### REQUIRED_USE: Level A vs Level C

`REQUIRED_USE` (`^^`/`??`/`||`/`flag? ( … )`) is handled at two possible levels:

- **Level A — validate & report (default).** `RequiredUseExpr::is_satisfied` /
  `unsatisfied` (in `portage-metadata`) evaluate each planned package's
  constraint against its effective USE; violations are reported as an advisory
  warning, matching Portage's default "fix your USE flags" behaviour. This is a
  purely local, post-solve check, so it lives in the cli (`required_use.rs`)
  beside `conflicts.rs` — it needs no solver state, and therefore the solver
  crate does **not** depend on `portage-metadata`.
- **Level C — solver auto-satisfaction (`--autosolve-use`, opt-in).** With the
  flag, `REQUIRED_USE` is encoded as relations between `UseDecision` packages so
  the solver *picks* satisfying flags (biased toward the configured value); the
  choices fold back into the displayed USE via synthetic `package.use`, and any
  flips are reported in a dedicated per-package report that cites the driving
  `REQUIRED_USE` clause (`output::report_autosolved_use`). Nested groups under a
  ceded guard (`a? ( ^^ ( b c ) )`) are encoded by gating, nested ceded-guard
  chains (`a? ( b? ( c ) )`) as escape clauses (`¬a ∨ ¬b ∨ c`), and choice branches are
  ordered toward the configured value so already-valid packages are left
  untouched. The cli cedes a package's flags **only when its `REQUIRED_USE` is
  actually violated**, and never cedes a flag pinned by `package.use` or by any
  force/mask (`ForceMask::pins`: `use.force`/`use.mask`, `package.use.force`/`mask`,
  and the `*.stable.*` variants) — so settled USE_EXPAND flags are not re-decided
  and profile-forced flags are never flipped. Intra-package only so far. It is
  **off by default**
  so default `em -p` keeps matching `emerge -p` (which does not auto-satisfy
  `REQUIRED_USE`). Concern split, the PubGrub encoding, and remaining phases are
  in [required-use-level-c.md](../portage-atom-pubgrub/docs/required-use-level-c.md).

Keeping Level A in the cli is deliberate: the `portage-metadata → portage-atom-pubgrub`
dependency is a Level-C cost, not a Level-A one.

## Solvers are interchangeable

Both solver bridges expose a `PackageRepository` trait and a provider over the
same facts; `em` defaults to PubGrub. This lets a plan be cross-checked between
two independent algorithms. The boundary rule for both: **facts in (deps, slots,
versions, IUSE names) and resolved policy in (desired USE via `desired_use`);
the solver computes the *needed* set and never resolves policy.**

## Known divergences from emerge

The plan (package set + versions) matches `emerge -p` on the test basket. The
useful way to read the remaining gaps is by **handling tier** — the guarantee a
constraint gets — not by feature, since almost everything is "handled outside the
PubGrub core" in *some* way:

- **Tier 1 — solved (enforced).** The solution provably satisfies it: version
  ranges, slots/subslots, `||`/`^^`/`??` groups, USE-*conditional* deps
  (`flag? ( dep )`), slot-operator `:=` subslot-change rebuilds, and Level-C
  `REQUIRED_USE` (opt-in, `--autosolve-use`).
- **Tier 2 — advisory.** Checked post-solve; the plan is still emitted even when
  violated, and the caveat is printed after it (as emerge does):
  - blockers (`!foo`/`!!foo`) — reported, not used to exclude/replace;
  - `::repo` constraints;
  - `REQUIRED_USE` Level-A (the default);
  - reverse-dependency conflicts — an *enrichment* a default targeted `emerge -p`
    hides (every installed package's constraints checked against the plan);
  - cross-package `[flag]` USE-deps — surfaced as autounmask `package.use`
    suggestions by default, but **co-solved** (promoted to Tier 1) under
    `--autosolve-use` by `package_use::cosolve_use_deps` (C7).
- **Tier 3 — invisible.** Not detected; the plan can silently differ from emerge
  with no warning:
  - old-slot wrapper/shim packages (`autoconf-wrapper`, `gcc-config`).

Plus two **intentional** cosmetic divergences: install-*order* positions (valid
topological order, different scheduler — emerge: target-driven DFS; here: SCC
condensation + lexicographic Kahn) and the `:slot` suffix on autounmask
`package.use` atoms. Severity tracks the tier: Tier 3 (silent) is the priority to
fix, Tier 2 is a deliberate "report don't block" stance (some intentional like
reverse-deps, some pending promotion like blockers and cross-package `[flag]`).
The running per-item list lives in the
[`portage-atom-pubgrub` README](../portage-atom-pubgrub/README.md) "Known
limitations" section and `docs/required-use-level-c.md` (§6, C7).
