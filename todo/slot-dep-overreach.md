# em bumps a `:*` slot dep emerge leaves satisfied

STATUS: **found 2026-07-26 comparing `em -tpuD rust` vs `emerge -tpuD rust`;
nothing implemented.** This is the root cause of the one *real* llvm conflict
em reports — fix it and the conflict stops existing.

## Symptom

Building `rust` on this arm64 host:

| | emerge `-tpuD rust` | em `-tpuD rust` |
|---|---|---|
| `llvm-core/llvm` | `[nomerge] llvm-core/llvm-21.1.8` — installed slot 21 satisfies | `[ebuild U] llvm-core/llvm-22.1.8 [22.1.6]` |
| `llvm-core/clang` | not touched | `[ebuild U] llvm-core/clang-22.1.8 [22.1.6]` |
| `llvm-core/llvm-common` | `[ebuild U] 22.1.8 [21.1.8]` | — |

emerge keeps the **installed** `llvm:21` because rust's slot dep is already
satisfied there. em instead pulls `llvm:22` forward, drags `clang:22` with it,
and then correctly reports that installed `lldb-22.1.6` (which is *not* in the
plan) has a `~llvm-core/llvm-22.1.6` pin that the plan breaks.

The conflict report is right. The plan that creates it is what is wrong.

## Suspected mechanism

`portage-cli/src/query/depgraph/mod.rs` sets

```rust
provider.set_prefer_newest_slot(deep || emptytree_native);
```

so `--deep` alone bumps every `:*` any-slot dep to the newest slot rather than
keeping a satisfying installed one. Portage's equivalent is far more
conservative: `_select_pkg_highest_available_imp` returns the installed instance
when `parent is not None and not self._want_update_pkg(parent, pkg)`
(`depgraph.py:8305-8311`), and `_want_update_pkg` (`:7137-`) consults
`--deep=<depth>` bounds, not a blanket "newest slot wins".

Not yet confirmed — establish empirically which of `prefer_newest_slot` /
`prefer_update` actually moves llvm here before changing either. Both are
implicated and they are separate knobs.

## Related, but distinct

[[deep-in-slot-upgrades]] tracks the *opposite-direction* gap: em requires
`--deep` for `prefer_update` where portage arms the slot-operator update probe
on `--update` alone (`depgraph.py:3630-3636`), so `em -pu @world` = 73 rows vs
`emerge -pu @world` = 182. Same two knobs, opposite errors — em is too
conservative about in-slot upgrades under `-u`, and too aggressive about slot
selection under `-D`. Fixing either in isolation risks making the other worse;
measure both row counts before and after.

Also distinct from the docutils/sphinx case (installed dependents' constraints
never enter the solve, `provider/mod.rs:744-766`) — that one is a genuine
solver gap, not a selection-policy overreach.

## Verification

- `em -tpuD rust` should keep `llvm-core/llvm-21.1.8` unmerged and drop the
  `llvm-core/lldb` conflict entirely.
- `em -pu @world` / `em -puD @world` row counts must not regress; compare
  against `emerge -pu --exclude app-containers/incus @world` (182 rows, rc=0).
  Read the measurement trap in [[selective-resolution]] first — a failing
  `emerge -p @world` prints the circular-dep subgraph, not the plan.
- Whole-output diff per [[live-verify-full-pretend-output]], not just the llvm
  rows.
