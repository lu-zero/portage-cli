# Break a hard dependency cycle by building a package twice: flag off, then flag on

Status: 🔴 designed, not started. Found 2026-11-18 on `em toolchain --local --setup` ([[docbook-catalog-rdepend-order]]).
Written from an Opus plan and a Fable adversarial review; nothing here was built or run. The review's unverified items were checked against the code on 2026-11-18; the findings are inline.

## Problem

Some bootstrap cycles have no valid order and no alternative. The live one:

```
docbook-xsl-stylesheets -R-> build-docbook-catalog -R-> util-linux -B(nls)-> po4a -B-> docbook-xsl-stylesheets
```

Both BDEPEND edges are hard, so `repair_soft_inversions` (`portage-atom-pubgrub/src/graph.rs`) drops an RDEPEND edge and the stylesheets' `pkg_postinst`
runs before `build-docbook-catalog` exists. `build-docbook-catalog`'s `|| ( util-linux app-misc/getopt )` has no usable second branch on Linux:
`app-misc/getopt` is keyworded only `~arm64-macos ~x64-macos ~x64-solaris`, so it is masked here and in prefix Linux profiles.
Choosing the other `||` branch (the separate, smaller track below) cannot fix this case; a split build can.

Dropping the other RDEPEND edge does not help either. Host `emerge -pe app-text/docbook-xsl-stylesheets` does exactly that (`build-docbook-catalog`, then
the stylesheets, `po4a`, `util-linux` last). On a fresh root `build-docbook-catalog`'s `src_prepare` then finds no util-linux
(`has_version sys-apps/util-linux || sed … getopt-long`), so its script calls a `getopt-long` nobody installs and the stylesheets' postinst still dies.

The fix is the Gentoo bootstrap pattern, done by the resolver for any cycle and never per package: build one package with a USE flag off so the loop opens,
then rebuild it later with the flag on. Portage does not do this itself; its circular-dependency message only suggests the USE change
(`lib/_emerge/resolver/circular_dependency.py`). No per-package lists, no `package.use` workaround in the toolchain plan.

## Behaviour

For `em -p app-text/docbook-xsl-stylesheets` on a bootstrapped `--local` root:

```
[ebuild  N    ] sys-apps/util-linux-2.42.3  USE="-nls" (bootstrap, rebuilt below)
[ebuild  N    ] app-text/build-docbook-catalog-2.4
[ebuild  N    ] app-text/docbook-xsl-stylesheets-1.79.1-r4
[ebuild  N    ] app-text/po4a-0.74-r1
[ebuild   R   ] sys-apps/util-linux-2.42.3  USE="nls"
```

Every `--json` edge has `order_ok: true`; `hard_cycle_edges` is empty. Without the feature the plan is unchanged.

## Design

### 1. Detect the cycle and pick the flag (`portage-atom-pubgrub/src/graph.rs`)

- A witness is a cycle left with at least one inverted RDEPEND edge after `repair_soft_inversions`, plus a path through the cycle containing a DEPEND/BDEPEND edge.
  Pure-soft cycles (gtk+ ↔ icon-theme) are out of scope and stay as today.
- Candidates are the hard or soft edges on the cycle with `via_use_flag = Some(f)` (already recorded on `DepEdge`), as `(package, version, f)`; prefer BDEPEND edges.
- Rank deterministically: fewest nodes the split adds, then package key. Never by name.
- Mechanism 1's `via_choice` edge field (below) is independent; this piece only needs `via_use_flag`.

### 2. Validate the flag (new `portage-resolve/src/cycle_split.rs`)

Turning `f` off for `P` is allowed only if all of these hold, else try the next candidate; with none left keep today's plan and say so:

- `f` is not forced or masked (`force_mask.rs`) and is not `-f` already.
- `P`'s REQUIRED_USE still holds (`required_use.rs`).
- No package ordered before the rebuild demands `P[f]`. This is checked after phase A solves (§3), because the cosolve fixpoint quietly turns `f` back on.
- The `!f? ( … )` dependencies the flag-off build gains do not close another cycle.

### 3. Build the two-phase plan (`portage-cli/src/query/depgraph/round/{solve,plan}.rs`)

- Where: in `solve_round` (`round.rs`), between `ctx.solve` and `ctx.shape_plan`, not inside `resolve_with_repairs`. The install order and edges come from
  `install_order_with_stats` in `shape_plan` (`round/plan.rs`), after the solve and its pin loop have finished, so there is no pin-loop interleaving.
  A different `package_use` is a different `ProviderKey`, so phase A is a fresh `ctx.solve` that runs the whole cosolve fixpoint and pin loop itself.
- Phase A: `ctx.solve` with `=cpv -f` appended to the base `package_use` that `cosolve_use_deps` starts from. The fixpoint appends forced `[f]` entries
  after the base, so a `P[f]` demand turns the flag back on without any error. After the solve, check that `P`'s solved USE really has `f` off. If not, reject
  the candidate. This check replaces a separate `UseDemand` pre-check.
- Phase B: solve the original targets again with phase A's packages as a synthetic installed overlay (the same shape as the `provided_avail` →
  `InstalledVersion` injection in `build_provider`). It lists only `P(+f)` and the new closure (po4a), ordered after phase A, `reinstall = true`.
- Concatenate A then B.
- Bound it: at most one split per cycle, `MAX_CYCLE_SPLITS` overall, accepted only when the cycle's bad-edge count drops; otherwise revert and reject that candidate.
- Performance: detection reads the edges `shape_plan` already builds, so a plan without a witness costs nothing extra. A split costs two more full solves
  (A and B), each with its own provider build. Measure with `portage-bench` / `bench-em-vs-emerge.sh`. The detection step needs the order before
  `shape_plan` trims it, so split `shape_plan` or have it return the witness rather than computing the order twice.

### 4. Plan identity: the same package appears twice

These key by `PortagePackage` or `(pkg, version)` and must key by plan index or instance (checked):

- `query/depgraph/assemble.rs:521` `index_of: HashMap<PortagePackage, usize>`, without even the version, so a second entry silently overwrites the first
  (feeds `build_blockers` and `hard_cycle_edges`).
- `output.rs:1767` `edge_order_ok` keys by `(&PortagePackage, &Version)`, and the JSON edges do too: add `instance`, `bootstrap_use`, from/to indices;
  `order_ok` per phase.
- `resume_completed: HashSet<(MergeRoot, String)>` (filtered in `depgraph/mod.rs:716`): the rebuild must not be skipped on resume. The `String` is a CPV;
  make it `Cpv` while changing the key.
- `--jobs`: phase B waits for all of phase A.
- Display: the `USE="-nls" (bootstrap, rebuilt below)` marker and the totals.
- VDB: a same-version replace, the existing `[R]` path.
- Binpkg: no change needed, provided each `PlannedMerge.use_flags` holds its own phase's USE. Reuse (`find_binpkg_reuse` → `use_compatible` in
  `portage-binpkg/src/index.rs`) requires USE∩IUSE to match, and `--buildpkg` writes a new build-id instance (`reserve_container`).
  The `-f` binpkg is never picked for the `+f` rebuild, and a later bootstrap can reuse it for its own phase A.
- `preflight.rs` already evaluates `use_flags` per entry, so duplicates work there.
- `DepEdge` is public in a publishable crate, and `output.rs` builds it as a struct literal, so any new field changes both.

### 5. Risks

- Cross builds: configs are per root, but the provider evaluates one `package_use` per solve, and `set_cross_active` points each Host node at its
  Target node's `VersionData` (`host_aliases`). So `=cpv -f` reaches both instances. Accept a candidate only when `P` sits at one merge root in the plan.
  (Whether Host nodes evaluate their USE conditionals under the target's config today is not checked, and is out of scope here.)
- A sub-slot change between the two builds triggers slot-operator rebuilds in phase B.
- `--root-deps=rdeps` drops the DEPEND edges, so the feature stays idle there: correct.
- `package.provided` nodes have no edges, so they never appear in a witness.
- The toolchain staged plan needs no change in `crossdev/`. Each step goes through `crate::emerge_atoms` (`crossdev/mod.rs` ~486) and the normal depgraph,
  so the split applies per step. A step's `use_override` is a make.conf-layer USE, and the split's `package.use` entry overrides it.
  `--nodeps` steps never build a graph, so they never split.

## Tests

- Unit, `portage-atom-pubgrub/src/provider/tests.rs` (`InMemoryRepository`): dxsl R bdc; bdc R `|| ( util-linux getopt )` with getopt absent (genuinely absent,
  not merely unlisted); util-linux IUSE `nls` on, BDEPEND `nls? ( po4a )`; po4a B dxsl. Expect `util-linux(-nls) … po4a, util-linux(+nls)`.
- Variants that must block the split: flag forced, REQUIRED_USE conflict, a `[nls]` demand from an earlier package, a flag-off build that closes another cycle.
- A pure-soft cycle is unchanged; the same plan comes out for shuffled insertion order.
- `portage-cli/src/query/depgraph/tests.rs::fixture`: every JSON `order_ok` is true, `build_blockers` valid, resume does not skip the rebuild.
- Live: `em -p --json --local DIR app-text/docbook-xsl-stylesheets` with `jq '.edges[]|select(.order_ok==false)'` empty; then a real
  `em toolchain --local --setup` passes the docbook packages; then a `portage-bench` run.

## Order of work (one commit each, tests with the code)

1. refactor(cli): plan identity by index, not by `(pkg, version)`; no behaviour change.
2. feat(pubgrub): cycle witness with candidate `(package, version, flag)` edges.
3. feat(resolve): `cycle_split.rs` validation.
4. feat(cli): the two-phase solve and concatenation, behind a flag at first.
5. feat(cli): display, JSON, resume, jobs and binpkg handling.
6. perf: order cache reuse in `shape_plan`, if the bench shows a cost.
7. docs: `docs/design/architecture.md` (resolution stages, divergences), the user docs for the new row marker, and this file moved to `todo/done/`.

## Separate smaller track: cycle-aware `||` choice

Portage's `dep_zapdeps` demotes an any-of branch that recreates a known circular dependency unless an installed version satisfies it. It does not fix the live
case (getopt is masked on Linux) but is general infrastructure. Constraints from review, if it is built:

- Never demote toward an empty or dropped branch: `provider/mod.rs` ~774-797 strips an unknown dependency out of a `Choice`, so a masked branch becomes empty and
  always satisfied, and the loop would report a clean order for a plan with neither `util-linux` nor `getopt`. Add a test where getopt is genuinely absent.
- Choice nodes are created in `portage-atom-pubgrub/src/convert.rs` (`convert_choice_group`, ~551), which already demotes circular build-time `||` branches at
  conversion (~557-570); keep the two consistent.
- Carry the `Choice` on the edge (`via_choice: Option<PortagePackage>`, set only when every path to the target goes through a `Choice`) instead of a
  `via_any_of` flag plus an owner map. Clear the avoid set at the start of each `resolve_with_repairs`.
- The demotion in `choose_version` (`provider/solve.rs`) is a preference placed after the installed-branch heuristics and must never return `None`.
