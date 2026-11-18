# Break a hard dependency cycle by building a package twice: flag off, then flag on

Status: 🔴 designed, not started. Found 2026-11-18 on `em toolchain --local --setup` ([[docbook-catalog-rdepend-order]]).
Written from an Opus plan and a Fable adversarial review; nothing here was built or run. Items marked **unverified** need a check before coding.

## Problem

Some bootstrap cycles have no valid order and no alternative. The live one:

```
docbook-xsl-stylesheets -R-> build-docbook-catalog -R-> util-linux -B(nls)-> po4a -B-> docbook-xsl-stylesheets
```

Both BDEPEND edges are hard, so `repair_soft_inversions` (`portage-atom-pubgrub/src/graph.rs`) drops an RDEPEND edge and the stylesheets' `pkg_postinst`
runs before `build-docbook-catalog` exists. `build-docbook-catalog`'s `|| ( util-linux app-misc/getopt )` has no usable second branch on Linux:
`app-misc/getopt` is keyworded only `~arm64-macos ~x64-macos ~x64-solaris`, so it is masked here and in prefix Linux profiles.
Choosing the other `||` branch (the separate, smaller track below) cannot fix this case; a split build can.

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
- No package ordered before the rebuild demands `P[f]` (`UseDemand`).
- The `!f? ( … )` dependencies the flag-off build gains do not close another cycle.

### 3. Build the two-phase plan (`portage-cli/src/query/depgraph/round/{solve,plan}.rs`)

- Phase A: re-solve with a synthetic `package.use` entry `=cpv -f` (`package_use` is already part of `ProviderKey`; the cosolve fixpoint uses the same route).
- Phase B: solve the original targets again with phase A's packages as a synthetic installed overlay (the same shape as the `provided_avail` →
  `InstalledVersion` injection in `build_provider`). It lists only `P(+f)` and the new closure (po4a), ordered after phase A, `reinstall = true`.
- Concatenate A then B. The re-solve runs inside `resolve_with_repairs`; a changed USE can need new pins, so interleave it with the pin loop or state that it
  does not (**unverified**: `selected_virtuals` is set after the pin loop at `provider/mod.rs` ~1263, so compute the in-loop order from the filtered solution).
- Bound it: at most one split per cycle, `MAX_CYCLE_SPLITS` overall, accepted only when the cycle's bad-edge count drops; otherwise revert and reject that candidate.
- Performance: one extra graph build per `resolve_with_repairs` call, and it is called many times. Skip when no cycle has a hard edge, and cache the
  `InstallOrderResult` for `shape_plan` to reuse. Measure with `portage-bench` / `bench-em-vs-emerge.sh`.

### 4. Plan identity: the same package appears twice

These key by `PortagePackage` or `(pkg, version)` and must key by plan index or instance (**unverified**, from reading `assemble.rs` and `output.rs`):

- `query/depgraph/assemble.rs` `index_of` (feeds `build_blockers` and `hard_cycle_edges`).
- `output.rs` `edge_order_ok` and the JSON edges: add `instance`, `bootstrap_use`, from/to indices; `order_ok` per phase.
- `resume_completed: HashSet<(MergeRoot, String)>`: the rebuild must not be skipped on resume.
- `--jobs`: phase B waits for all of phase A.
- Display: the `USE="-nls" (bootstrap, rebuilt below)` marker and the totals.
- VDB: a same-version replace, the existing `[R]` path.
- Binpkg: do not `--buildpkg` the phase-A copy and do not reuse it through `--usepkg` (**unverified**: whether binpkg selection checks USE).
- `preflight.rs` already evaluates `use_flags` per entry, so duplicates work there.
- `DepEdge` is public in a publishable crate, and `output.rs` builds it as a struct literal, so any new field changes both.

### 5. Risks

- `package.use` applies to both Host and Target instances of one CPV in cross builds.
- A sub-slot change between the two builds triggers slot-operator rebuilds in phase B.
- `--root-deps=rdeps` drops the DEPEND edges, so the feature stays idle there: correct.
- `package.provided` nodes have no edges, so they never appear in a witness.
- The toolchain staged plan resolves each step alone, so the split applies per step; no change in `crossdev/stages.rs` is needed (**unverified**).

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
