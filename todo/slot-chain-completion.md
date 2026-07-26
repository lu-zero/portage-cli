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

## Very likely the same fix as the docutils/sphinx gap

Both are "an installed package's constraints are checked after the solve
instead of participating in it":

- `llvm-core/lldb-22.1.6` pins `~llvm-core/llvm-22.1.6`; plan takes 22.1.8;
  lldb never pulled in → conflict reported.
- `dev-python/sphinx-9.0.4-r1` requires `<dev-python/docutils-0.23`; plan takes
  0.23; sphinx never pulled in → conflict reported. `emerge -pu` resolves it by
  pulling `sphinx-9.1.0-r1` alongside.

Mechanism, confirmed: `PortageDependencyProvider::add_installed`
(`portage-atom-pubgrub/src/provider/mod.rs:744-766`) records a version stub,
`installed_use` and `installed_iuse` — **never the installed package's
dependency constraints**. The only installed dep data reaching the provider is
blockers, via `add_installed_blockers` (:723-730), which is the precedent for
the API shape a fix would need.

Treat these as one problem with one fix, not two.

## What portage does

`_complete_graph` plus the slot-operator probe, not either alone:

- `_complete_graph` (`depgraph.py:8398`) auto-enables when any merge node
  differs in version, slot/sub-slot or USE from its installed instance
  (:8428-8481), loads the whole VDB (:8484), and makes every installed
  package's deps real constraints.
- `depgraph.py:8590-8641` drains `_unsatisfied_deps`: if the *installed* VDB
  satisfied the atom before the plan, the installed package is added to the
  graph — *"An scheduled installation broke a deep dependency. Add the
  installed package to the graph so that it will be appropriately reported as a
  slot collision (possibly solvable via backtracking)."* Deps that were never
  satisfied are ignored; portage only protects what already worked.
- `_slot_operator_update_probe` (`:2399`) then looks for a replacement *parent*
  via `_iter_similar_available`, validated by
  `_slot_operator_check_reverse_dependencies` (:2295).
- `_slot_operator_update_backtrack` (:2223-2274) sets `_need_restart`;
  `_gen_reinstall_sets` (:5297-5321) turns the result into a synthetic
  `__auto_slot_operator_replace_installed__` set with `force_reinstall=True`,
  so the dependent becomes a target on the next backtrack round.

Termination, four independent bounds: `--backtrack` 20 / depth 10
(`:11971-11975`); `reset_depth=False` on the synthetic set (:5316-5319);
`_eliminate_rebuilds` (~:3686-3790), which drops rebuilds whose installed
instance is already identical; `prune_rebuilds` (:5600-5615), which discards
the whole set if it caused missed updates. It advances one hop per restart.

## Design constraint specific to em

A prior investigation recommended **against** porting `_complete_graph`
wholesale: PubGrub has no backtracking-policy hook, so adding installed
constraints without a repair strategy converts today's advisory warnings into
hard `NoSolution` failures (`portage-atom-pubgrub/src/lib.rs:100`). Its
suggested shape was a bounded post-solve repair loop reusing the existing
fixpoint in `resolve_targets` (`provider/mod.rs:954-1044`,
`MAX_RESOLVE_ITERS = 4`, `upgrade_pins` at :1003-1013) — repair targets have
the same shape as upgrade pins and belong in the same loop.

**That recommendation predates this reframing — re-evaluate it.** If the intent
is to complete chains rather than to avoid starting them, more of the graph
legitimately needs to move, and the iteration bound becomes the thing deciding
how far a chain may propagate. Open: whether 4 iterations covers a real family
(llvm → clang → lldb → …), and whether em needs an `_eliminate_rebuilds`
equivalent to stop the repair set growing without bound.

## Adjacent, but a different defect

[[deep-in-slot-upgrades]]: em requires `--deep` for `prefer_update` where
portage arms the slot-operator update probe on `--update` alone
(`depgraph.py:3630-3636`) — `em -pu @world` 73 rows vs `emerge -pu @world` 182.
Same two knobs (`set_prefer_newest_slot` / `set_prefer_update`,
`portage-cli/src/query/depgraph/mod.rs`), different defect. Measure both row
counts across any change to either.

## Verification

- `em -tpuD rust` pulls `llvm-core/lldb` into the plan alongside `llvm`/`clang`
  and reports no conflict.
- `em -puD @world` pulls `dev-python/sphinx` alongside `docutils-0.23`.
- `em -pu @world` / `-puD @world` row counts must not regress; compare against
  `emerge -pu --exclude app-containers/incus @world` (182 rows, rc=0). Read the
  measurement trap in [[selective-resolution]] first — a failing
  `emerge -p @world` prints the circular-dep subgraph, not the plan.
- Whole-output diff per [[live-verify-full-pretend-output]], not just the llvm
  rows.
