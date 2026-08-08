# portage-atom-pubgrub

[![LICENSE](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE-MIT)
[![Build Status](https://github.com/lu-zero/portage-atom-pubgrub/workflows/CI/badge.svg)](https://github.com/lu-zero/portage-atom-pubgrub/actions?query=workflow:CI)
[![Crates.io](https://img.shields.io/crates/v/portage-atom-pubgrub.svg)](https://crates.io/crates/portage-atom-pubgrub)
[![docs.rs](https://docs.rs/portage-atom-pubgrub/badge.svg)](https://docs.rs/portage-atom-pubgrub)

A Rust library that bridges [portage-atom](https://crates.io/crates/portage-atom) types with the [PubGrub](https://pubgrub-rs.github.io/pubgrub/) dependency solver, implementing the [Package Manager Specification (PMS) 9](https://projects.gentoo.org/pms/9/pms.html).

> **Warning**: This codebase was largely AI-generated and has not been thoroughly
> audited. It may contain bugs, incomplete PMS coverage, or surprising edge-case
> behaviour. Use at your own risk and please report issues.

## Overview

`portage-atom-pubgrub` provides a `DependencyProvider` implementation for the
PubGrub version solver that understands Gentoo Portage dependency semantics:

- **`PortagePackage`** — a PubGrub `Package` backed by a `Cpn` + optional slot
- **`PortageVersionSet`** — a PubGrub `VersionSet` mapping PMS operators (`>=`,
  `~`, `=*`, etc.) to `Ranges<Version>`
- **`PortageDependencyProvider`** — a PubGrub `DependencyProvider` over a
  package repository, with support for:
  - All five PMS dependency classes (DEPEND, RDEPEND, BDEPEND, PDEPEND, IDEPEND)
  - OR groups, exactly-one-of, at-most-one-of modelled as virtual choice packages
  - Slot and subslot operators (`:=`, `:*`)
  - USE-conditional dependencies with hybrid evaluation (eager for user-decided,
    virtual packages for solver-decided flags)
  - USE-dep constraints (`[ssl]`, `[-debug]`, `[ssl?]`, `[ssl=]`)
  - Repository constraints (`::gentoo`)
  - Installed package tracking (favored / locked)
  - Blocker detection and post-solve validation
  - Dependency graph with labeled edges and topological install ordering
  - Filtering of dependencies referencing packages absent from the repository

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
portage-atom-pubgrub = "0.8"
```

## Usage

```rust
use portage_atom_pubgrub::{
    PackageRepository, PortageDependencyProvider, PortagePackage, PortageVersionSet,
};

// Implement PackageRepository for your data source. The repository is also
// responsible for resolved policy: `desired_use(&Cpv)` hands the solver the
// fully-resolved USE state (profile ∘ make.conf ∘ package.use ∘ IUSE-defaults).
// The solver never recomputes it — see docs/use-and-solver-boundary.md.
let repo = MyRepository::new();
let mut provider = PortageDependencyProvider::new(repo);

// Register installed packages (favored or locked), if any:
// provider.add_installed(installed_package);

// Resolve a set of target atoms in one call:
let solution = provider.resolve_targets(vec![
    (target_package, target_version_set),
])?;
```

See `examples/resolve.rs` and `examples/resolve_conflicts.rs` for complete
examples (`InMemoryRepository::set_use_config` shows how `desired_use` is fed).

## PMS Operators to Version Sets

| PMS Operator | Version Set |
|---|---|
| `>=V` | `Ranges::higher_than(V)` |
| `>V` | `Ranges::strictly_higher_than(V)` |
| `<=V` | `Ranges::lower_than(V)` |
| `<V` | `Ranges::strictly_lower_than(V)` |
| `=V` | `Ranges::singleton(V)` |
| `=V*` (glob) | `Ranges::between(V, next_after_glob(V))` |
| `~V` (approximate) | `Ranges::between(V_norev, V_rev_bumped)` |

## Post-Solve Validation

The provider exposes methods for checks that happen after resolution:

- **`check_blockers()`** — validates that no installed packages conflict with
  resolved blockers
- **`check_use_deps()`** — validates USE-dep constraints against the solution
- **`check_repo_constraints()`** — validates repository constraints
- **`dependency_graph()`** — returns labeled edges with dependency class and
  topological install order (PDEPEND edges deferred)

## Performance

The numbers below compare the reference consumer — `em -p <atom>` from
[portage-cli](https://github.com/lu-zero/portage-cli), which drives this crate —
against Portage's own `emerge -p <atom>` for the same targets, producing the same
package set and versions.

| Target | Packages | `em -p` | `emerge -p` | Speedup |
|---|---|---|---|---|
| `www-client/firefox` | 78 | 0.80 s | 3.63 s | 4.5× |
| `app-office/libreoffice` | 140 | 0.91 s | 3.99 s | 4.4× |
| all five large targets at once* | 186 | 0.98 s | 4.82 s | 4.9× |
| `em -s gcc` (search) | — | 0.09 s | 0.82 s | 8.4× |

\* `em -p libreoffice qtwebengine thunderbird qemu firefox` — multiple
targets cost almost nothing extra (the md5-cache parse dominates).
Provider ingestion is closure-seeded: only packages reachable from the
targets and the installed set are converted, so the autounmask-preview
fixpoint's extra solve iterations cost milliseconds, not re-ingestions.
Reproduce with `benchmarks/bench-em-vs-emerge.sh` from portage-cli.

Wall-clock means from `hyperfine --warmup 2` (8 runs for `em`, 5 for `emerge`).
Most of `em`'s time is ebuild-metadata (md5-cache) parsing and profile/USE
evaluation, not the solve itself.

**Reproducing.** Resolution depends on the tree snapshot and the installed set,
so results are only comparable against a pinned tree:

- ::gentoo tree: [`gentoo-mirror/gentoo`](https://github.com/gentoo-mirror/gentoo)
  at commit `713f24e3bbbbad76b2c983ecf9659821355f0ba0` (branch `stable`, 2026-06-02)
- Installed packages: 708 · Profile arch: `arm64` (`ACCEPT_KEYWORDS="arm64 ~arm64"`)

```sh
git -C /var/db/repos/gentoo checkout 713f24e3
hyperfine --warmup 2 'em -p www-client/firefox' 'emerge -p www-client/firefox'
```

## Known limitations / divergences from emerge

- **Install-order positions** differ from `emerge`. Both orders are
  topologically valid (every dependency precedes its dependents); the sequence
  differs because the schedulers differ (emerge: target-driven DFS; here:
  SCC condensation + lexicographic Kahn). The *set* and *versions* match.
- **Reverse-dependency consistency is checked, and reported.** Unlike a default
  targeted `emerge -p` (which does not pull reverse deps into the graph), the
  consumer checks every installed package's constraints against the plan and
  emits an advisory "dependency constraint conflict" when a proposed upgrade
  would violate one (e.g. upgrading `docutils` past an installed package's `<`
  bound). This is a non-fatal warning — the plan is still produced and is
  identical to emerge's; the breakage is simply surfaced rather than hidden.
- **`REQUIRED_USE`: Level A by default, Level C opt-in.** `^^`/`??`/`a? ( b )`
  are parsed and evaluated (`portage-metadata`); by default the consumer reports
  any unsatisfied constraint post-solve as an advisory warning, matching
  Portage's "fix your USE flags" behaviour (**Level A**). With the consumer's
  `--autosolve-use` the constraint is encoded over `UseDecision` nodes and the
  solver *chooses* satisfying flag values, biased toward the configured value
  (**Level C**, intra-package); see `docs/required-use-level-c.md`. Level C now
  also encodes **nested groups under a ceded guard** (`a? ( ^^ ( b c ) )`) by
  gating their constraints behind the guard, **nested ceded-guard chains**
  (`a? ( b? ( c ) )`) as preference-ordered escape clauses (`¬a ∨ ¬b ∨ c`),
  orders choice branches toward the
  configured value to avoid gratuitous flips, and (in the consumer) cedes a
  package's flags only when its `REQUIRED_USE` is actually violated and the flag
  is not pinned by `package.use` or any force/mask (`use.force`/`use.mask`,
  `package.use.force`/`mask`, and the `*.stable.*` variants) — so autosolve never
  re-decides settled USE_EXPAND flags or flips a profile-forced flag. Flips are
  surfaced in a per-package report citing the driving clause, and flags are
  only ceded for packages actually being built — a version staying installed
  keeps its build-time USE. `UseDecision`
  nodes are per-`(cpn, flag)` — a decision spanning several in-plan slots of one
  package is surfaced as an advisory rather than solved per-slot.
- **Cross-package `[flag]` USE-deps are co-solved by default** (emerge preview
  parity): the consumer forces the demanded flags on real-IUSE, non-user-pinned
  targets and re-solves to a fixpoint, so `em -p`'s displayed graph — like
  `emerge -p`'s — is the one the *adjusted* configuration would produce. The
  required changes are printed as a mandatory "USE changes are necessary to
  proceed" block after the merge list and the run exits `1`, exactly as emerge
  does. One deliberate divergence: the fixpoint follows the adjustment cascade
  to completion (`gtk+[wayland]` → `mesa[wayland]` → `libglvnd[X]`), where
  emerge's backtracking sometimes stops partway on multi-target invocations and
  shows a graph missing the cascade's tail.
- **Slot-operator (`:=`) rebuilds are implemented.** The VDB-recorded
  `:slot/subslot=` bindings of installed consumers are checked against the
  plan; a dependency moving across a subslot boundary pulls the consumer in as
  a same-version rebuild right after its trigger, with portage's lowercase `r`
  marker on both ends (`[ebuild r U ]` provider / `[ebuild rR ]` consumer).
- **Upgraded versions are re-solved.** When a forced rebuild is favoured up to a
  newer version (`upgrade_to`), `resolve_targets` pins that version and re-solves
  to a fixpoint (bounded), so the upgraded version's full dependency closure
  (including any deps the installed version lacked) is part of the plan rather
  than an unaccounted approximation. If a re-solve cannot be satisfied it falls
  back to the last good solution instead of erroring.

## Related Projects

- [portage-atom](https://crates.io/crates/portage-atom) — Portage package atom parser
- [portage-atom-resolvo](https://crates.io/crates/portage-atom-resolvo) — Bridge to the resolvo SAT solver
- [pubgrub](https://crates.io/crates/pubgrub) — PubGrub version solving algorithm
- [PMS 9](https://projects.gentoo.org/pms/9/pms.html) — Package Manager Specification

## License

[MIT](LICENSE-MIT)
