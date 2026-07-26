# Finish the update chain instead of stopping halfway

STATUS: **found 2026-07-26 comparing `em -tpuD rust` vs `emerge -tpuD rust`;
reframed the same day (see "Direction"); nothing implemented.**

## Symptom

Building `rust` on this arm64 host:

| | emerge `-tpuD rust` | em `-tpuD rust` |
|---|---|---|
| `llvm-core/llvm` | `[nomerge] llvm-core/llvm-21.1.8` — installed slot 21 satisfies | `[ebuild U] llvm-core/llvm-22.1.8 [22.1.6]` |
| `llvm-core/clang` | not touched | `[ebuild U] llvm-core/clang-22.1.8 [22.1.6]` |
| `llvm-core/lldb` | not touched | **not in the plan** |

em then correctly reports that installed `lldb-22.1.6` carries a
`~llvm-core/llvm-22.1.6` pin the plan breaks.

## Direction — em is not being too aggressive

The first draft of this note called em's behaviour overreach and proposed
backing off to emerge's conservative choice. **That is not the intended
direction** (user, 2026-07-26). Moving the llvm family forward to `:22` is a
fine thing to want: under `-uD` the user asked for a deep update, and keeping
`llvm:21` merely because it happens to satisfy the dep leaves the system on an
old slot.

The defect is not that em *starts* the chain — it is that it **stops halfway**.
`llvm` and `clang` move; `lldb`, which pins them at `~<version>`, is left
behind. A chain that moves must move whole.

So go **deeper**: pull the retained installed dependents the plan would break
into the plan as upgrade/rebuild targets, until the `~`-pinned family is
consistent again. `lldb-22.1.8` exists in the tree, so the chain can complete.

## CORRECTION — this is *not* the same as the docutils/sphinx gap

An earlier draft claimed both were one problem rooted in `add_installed`
(`portage-atom-pubgrub/src/provider/mod.rs:744-766`) not recording installed
dependency constraints. **Both halves of that were wrong** (investigated
2026-07-26, key facts independently re-confirmed):

- **docutils/sphinx is not a resolver defect at all.** It is
  [[md5-cache-blind-spot]]: `sys-fs/btrfs-progs-7.1` has BDEPEND
  `|| ( ( python:3.14 sphinx[…] sphinx-rtd-theme[…] ) … )`, but that ebuild has
  no md5-cache entry, so em cannot see the version, never walks the BDEPEND,
  and sphinx is never a graph node. Verified: `em -p '=sys-fs/btrfs-progs-7.1'`
  → "no ebuilds"; `emerge -p` the same atom → rc=0. Fix the cache blindness and
  this case vanishes with no chain machinery.
- **`add_installed` is not the blocker even for llvm/lldb.** PubGrub assigns no
  version to a package with no *incoming edge*; lldb is absent because nothing
  requires it, not because its constraints are missing — recording them without
  adding the node is a no-op. Worse, both installed CPVs (`lldb-22.1.6`,
  `sphinx-9.0.4-r1`) have been removed from the tree, so `add_installed` inserts
  an empty-dep stub (`mod.rs:749-754`) for want of repo metadata. Constraints
  would have to come from `VdbEntry::deps` in portage-resolve regardless.

## This is a new policy, not portage parity

Portage does **not** complete `~`-pinned chains either. Forced into the same
corner it backs off or fails:

```
emerge -p1 '=llvm-core/llvm-22.1.8'  → rc=1, slot conflict; clang/lldb NOT pulled
emerge -p1 dev-python/docutils       → rc=0, docutils stays 0.22.4 [R]
```

The reason is the guard at `depgraph.py:8607-8614`: the update-probe escape
requires `dep.atom.soname or (dep.atom.package and dep.atom.slot_operator_built)`.
A `~llvm-core/llvm-22.1.6` or `<dev-python/docutils-0.23` atom is neither, so it
falls through to `_add_pkg(installed)` → slot conflict → backtracking.
`_slot_operator_update_probe` / `_gen_reinstall_sets` /
`__auto_slot_operator_replace_installed__` (`:2399`, `:5296`) are the
`:=`/soname path **only**.

`lldb` survives under `@world` merely because it is a world member
(`/var/lib/portage/world:63`); under `rust` it is unreachable.

Consequence for this ticket: completing the chain is a legitimate thing to want,
but **there is no reference implementation and no parity oracle**. The
verification criterion "`em -tpuD rust` pulls lldb and reports no conflict"
cannot be checked against emerge — emerge produces neither answer.

## Recommended mechanism

**Reject** reusing `resolve_targets`' fixpoint / `upgrade_pins`
(`provider/mod.rs:954-1044`): `upgrade_pins` (`solve.rs:63-67`) pins the version
of a node *already in the graph*, whereas repair must **add nodes** — which
changes the synthetic root's dependency set, i.e. that function's *input*. The
provider also has no VDB dep strings.

**Model it on the existing subslot splice instead.** `subslot::find_rebuilds`
(`portage-resolve/src/subslot.rs:68`) plus `mod.rs:921-952` already *is* em's
`__auto_slot_operator_replace_installed__`: post-solve, CLI-level,
position-aware, sets the `r` marker. The only thing it cannot do is move the
version — exactly what llvm/lldb needs.

```
loop (≤3):
  solve
  new = find_conflicts(...) |> retained owners (owner_replaced_by.is_none())
                            |> minus already-repaired, minus --exclude
  if new.empty(): break
  repair_targets += new;  re-solve;  on Err → discard round, keep last good
```

### Gated behind `--complete-graph` (decided 2026-07-26)

`--complete-graph` is declared at `portage-cli/src/cli/merge_flags.rs:143`,
copied through `maint/resume.rs:416` and `crossdev/mod.rs:139`, and **never
read** — a dead flag. This feature gets it.

Why an explicit opt-in rather than on-by-default:

- There is **no parity oracle** (see "new policy" above), so this cannot be
  validated against emerge the way the rest of the resolver is. A flag keeps an
  unvalidatable policy out of everyone's default plans.
- The policy is "the chain moves whole or not at all", so it can **revert an
  upgrade the user would otherwise have got** when a dependent has no satisfying
  version. That is a surprising default; it is a reasonable opt-in.
- It gives the loop a natural kill switch while the bounding behaviour is still
  being characterised on real closures.

Deviation to note: portage **auto-enables** its `_complete_graph` whenever any
merge node differs from its installed instance (`depgraph.py:8428-8481`), so
gating only on the flag is deliberately more conservative than portage. Given
portage's complete-graph does not actually complete `~`-chains anyway
(`:8607-8614`), the name is being reused for a related-but-different job —
document that in the flag's help, and describe it on its own terms rather than
as "emerge's `--complete-graph`" (see [[no-emerge-equivalents-in-help]]).

Revisit auto-enabling once the loop has proven itself; the flag then becomes the
override rather than the switch.

Repair targets go into `resolve_targets`' target vector (so
`root_targets.contains()` at `solve.rs:106` gives newest-in-range) but **not**
into `root_cpns`/`root_pkgs` — otherwise they are labelled `(argument)` in
autounmask output, forced to `[R]` by the `!selective` clause (`mod.rs:778-781`),
and dropped by `--onlydeps` (`mod.rs:918`).

Validated by spike (extra explicit target ≈ what the loop injects):

| run | rows | broken conflicts |
|---|---|---|
| `em -puD rust` | 175 | llvm + clang |
| `em -puD rust llvm-core/lldb` | 177 | **none** |
| `em -puD @world` | 288 | docutils/sphinx |
| `em -puD @world dev-python/sphinx` | 300 | **none** |

Cost: `em -puD @world` 1.02 s → 1.04 s. (`emerge -puD`: 50 s.)

## Bounding

- **Do not reuse `MAX_RESOLVE_ITERS = 4`** — it bounds a different fixpoint, and
  `cosolve_use_deps` already loops 8× inside it; nesting gives 32 solves.
- Measured chain **depth** is 1 repair round for both families. The growth axis
  is **breadth** (`dev-lang/perl` alone drags ~60 `dev-perl/*` in one round),
  which is bounded by |installed|. Cap 3 as a safety net, not a policy.
- **No `_eliminate_rebuilds` equivalent needed**: em's `order` filter
  (`mod.rs:743-783`) already drops a target that resolves back to its installed
  version.
- **A `prune_rebuilds` equivalent is needed**, and the idiom exists —
  `Err(_) => break` at `provider/mod.rs:1026`.
- **When the chain cannot complete**, PubGrub handles it without special-casing:
  if the upgrade is only *preferred* it backtracks and reverts it (llvm stays
  22.1.6 — emerge's answer, reached automatically). That must be **reported**,
  not silent: "the chain moves whole or not at all" means a correct
  implementation will sometimes back off as an emergent consequence.

## `find_conflicts` as loop input: right partition, insufficient payload

`owner_replaced_by.is_none()` is the correct retained-owner set and the right
trigger (landed this session, unit-tested). Two gaps:

1. **`Conflict` carries no slot** (`portage-resolve/src/conflicts.rs:14-31`);
   the loop needs `entry.slot` to build `PortagePackage::slotted(...)`. One
   field, trivially testable.
2. **`find_conflicts` is blind to USE-deps and subslots.** `Dep::matches_cpv`
   (`portage-atom/src/dep.rs:117-150`) compares cpn, slot name and version only.
   Measured consequence: installed `sphinx-rtd-theme-3.1.0` has
   `>=dev-python/sphinx-6[python_targets_python3_13(-)]` and the plan installs
   sphinx with `python3_14`; emerge pulls `sphinx-rtd-theme` and
   `sphinxcontrib-jquery`, em reports nothing. So a conflict-driven loop will
   not reach emerge's answer. Use it as-is for v1, but do not claim parity —
   and extend by **unifying** with `subslot::find_rebuilds` rather than growing
   a second reverse-dep check.

## Adjacent, but a different defect — and the earlier citation was wrong

[[deep-in-slot-upgrades]] is **`STATUS: done 2026-07-18`** and describes `-uD`
in-slot upgrades; it contains no 73-vs-182 measurement. That figure lives in
[[selective-resolution]], which diagnoses it correctly: the whole difference is
emerge's slot-operator forced rebuilds (`r`), which `subslot::find_rebuilds`
fires on none of. Re-measured 2026-07-26:

| | rows | `r`-marked |
|---|---|---|
| `em -pu @world` | 73 | **0** |
| `emerge -pu --exclude app-containers/incus @world` | 182 (rc=0) | **87** |
| `em -puD @world` | 288 | 66 |
| `emerge -puD --exclude … @world` | 305 (rc=0) | — |

Root cause: ~60 of the 109 missing CPNs are `dev-perl/*` bound to
`dev-lang/perl:0/5.42=`, and em's `-pu` plan contains no `dev-lang/perl` at all,
so `find_rebuilds`' `planned_slots` has no trigger. Orthogonal to chain
completion (which cannot see subslot bindings), sharing only the *output*
mechanism — an argument for one unified reverse-dep pass, not for merging the
tickets.

## Risks

- **Gate on `update` for v1.** With `selective_no_update` false, a repair target
  takes the newest, so `em -p1 docutils` would upgrade sphinx where emerge backs
  docutils off. Revisit the non-update path separately.
- **`--emptytree`**: guard with `!empty`, mirroring `mod.rs:927` — under `-e`
  every unreachable installed package is still "retained".
- **cross / `--root`**: repair targets must be `MergeRoot::Target`. Input is
  already Target-filtered (`mod.rs:1142-1150`). Bare `--root` has an empty VDB.
- **Most dangerous integration point**: a repair target with no acceptable
  version must never reach `classify_root_target`'s `Fatal` branch
  (`mod.rs:410-414`) — that would abort the run on an advisory condition.
- **`--exclude`/`resume_completed`** filters run post-solve (`mod.rs:813-845`),
  after the loop would inject; excluded atoms must not become repair targets.

## Landing sequence

0. **[[md5-cache-blind-spot]] first** — it removes the docutils/sphinx case from
   this ticket entirely and is higher value for less work.
1. Add `slot` to `Conflict` + a `retained_owners()` accessor. Pure unit test.
2. The repair loop, gated `complete_graph && update && !empty`, cap 3,
   discard-on-failure, targets kept out of `root_cpns`/`root_pkgs`/`Fatal`.
   Target derivation is unit-testable; convergence is live-only. Regression
   gate: with the flag off, `em -pu @world` stays 73 and `em -p @world` stays
   181 — the flag makes this trivially provable, since every existing
   invocation takes the old path unchanged.
3. Report both directions: "pulled in to complete the update chain" **and**
   "chain could not complete, upgrade reverted".
4. Unify `find_conflicts` with `subslot::find_rebuilds` so USE-dep and subslot
   breakage feed one loop. Only then is `sphinx-rtd-theme`-class parity reachable.

`--complete-graph` stops being a dead flag as of step 2 — see the gating
section above.

## Verification

- `em -tpuD rust` pulls `llvm-core/lldb` alongside `llvm`/`clang`, no conflict.
  **No emerge oracle** for this — see "new policy" above.
- `em -pu @world` / `-puD @world` row counts must not regress; compare against
  `emerge -pu --exclude app-containers/incus @world` (182 rows, rc=0). Read the
  measurement trap in [[selective-resolution]] first — a failing
  `emerge -p @world` prints the circular-dep subgraph, not the plan.
- Whole-output diff per [[live-verify-full-pretend-output]], not just the llvm
  rows.
