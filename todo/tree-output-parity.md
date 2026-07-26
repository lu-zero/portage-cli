# `--tree`/`-t` is a different artifact from emerge's

STATUS: **found 2026-07-26 comparing `em -tpuD rust` vs `emerge -tpuD rust`;
nothing implemented.**

## Symptom

emerge's `-t` is the **merge list, indented by depth** — every column of the
normal `-p` row is kept, and `[nomerge]` rows are added for the packages that
are only there to explain the tree:

```
[ebuild     U  ]  llvm-core/llvm-common-22.1.8 [21.1.8]
[nomerge       ] llvm-core/llvm-21.1.8
```

em's `-t` is a **bare dependency tree** — no status column, no version being
replaced, no USE, no size, no download total:

```
dev-lang/rust-1.97.1
├── app-arch/xz-utils-5.8.3
│   └── app-portage/elt-patches-20250718
```

Measured on this host: `em -tpuD rust` emits **0** `[ebuild` rows,
`emerge -tpuD rust` emits **108**.

So `-t` currently *replaces* the plan instead of annotating it, and `-tp`
loses everything plain `-p` would have told you. That is the opposite of
emerge, where `-t` is strictly additive.

## Why it matters

`-t` is the tool you reach for when a plan contains something surprising and
you want to know who pulled it in. Losing the action tags and versions at
exactly that moment inverts its purpose: you can see the shape of the graph
but not what will happen to any node in it.

Related: emerge forces `--tree` on internally when printing a circular-dep
subgraph (`depgraph.py:10198-10212`) — see the measurement trap in
[[selective-resolution]]. Any rework here should keep that shape recognisable.

## Shape of the fix

Render the existing plan rows (`output.rs`'s row formatter, the one `-p` uses)
with a depth indent, rather than emitting a separate tree of bare cpvs. The
depth relation already exists — `DepgraphOutcome.build_blockers` carries in-plan
build-dep edges, and the solver's `DepEdge` set (already consumed by
`package_use::build_comments`, `portage-resolve/src/package_use.rs:301`) has the
full parent → child relation with the gating USE flag.

Open questions before implementing:

- emerge shows `[nomerge]` rows for already-satisfied parents so the chain is
  connected. em has no `[nomerge]` concept in its plan output at all; decide
  whether to synthesise those rows or to draw the tree only over planned nodes
  (which would disconnect it in the common case).
- emerge prints a node once per *path* (with `(*)` back-references for repeats);
  em's current tree already does the `(*)` part, so that convention can stay.
- `--verbose` interaction: emerge's `-t` forces verbose-ish output. Check
  against [[no-emerge-equivalents-in-help]] before wording any new flag help.

## Verification

Per [[live-verify-full-pretend-output]], diff the *whole* output against
`emerge -tpuD rust`, not just the target rows. Row count is a cheap first
check (`grep -c '\[ebuild'`) but not sufficient — the indent depth and the
`[nomerge]` placement are the substance.
