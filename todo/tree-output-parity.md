# `--tree`/`-t` is a different artifact from emerge's

STATUS: **landed 2026-07-27, with a deliberate style deviation from emerge
(see "What shipped").** Found 2026-07-26 comparing `em -tpuD rust` vs
`emerge -tpuD rust`.

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
  (which would disconnect it in the common case). ✅ Synthesised — see below.
- emerge prints a node once per *path* (with `(*)` back-references for repeats);
  em's current tree already does the `(*)` part, so that convention can stay.
  ✅ Kept.
- `--verbose` interaction: emerge's `-t` forces verbose-ish output. ✅ Not
  changed — Tree reads the same `verbose` field Pretty does (size column,
  `:slot::repo` suffix), no new forcing added.

## What shipped

**Deliberate deviation from emerge's own rendering** (user call, 2026-07-27,
after seeing a real `emerge -tep @world` capture): emerge's `-t` drops the
box-drawing connectors entirely and indents with one plain space per depth
level. The user liked em's existing `├──`/`└──` tree shape and asked to
*keep* it, just add the missing columns — so the fix is "annotate the plan"
in the sense the symptom section asked for, but with em's own tree-drawing
style, not a byte-for-byte copy of emerge's indent convention. If exact
parity with emerge's plain-indent style is wanted later, that's a separate,
smaller follow-up (swap the connector-building in `Tree::node` only — the row
content itself is already shared and would need no change).

Mechanism: the whole per-row string emerge's `-p`/Pretty builds (bracket
status, `cpn-ver`, old-version column, USE flags, size, destination suffix)
was extracted out of `print_pretty_with_roots` into `format_plan_row`
(`output.rs`), taking an `in_plan: bool`. `print_tree`'s existing
box-drawing DFS (`Tree::node`) now calls it for every node instead of
printing a bare `cpn-ver`: a node present in the actual merge `order` gets
its real computed action bracket; anything else (a dependency-graph node
reached only to connect the tree — already satisfied, nothing to do) gets a
fixed-width `[nomerge]` bracket, padded to the same 14-char width `ebuild `/
`binary ` plus their status field already have. Both branches compute
USE/old-version/size identically — matching the real `emerge -tep @world`
capture, where a `[nomerge]` perl row still shows its full USE string and
`[5.42.0-r1...]` old-version bracket, not a bare cpv.

`depgraph()` (`mod.rs`) now builds one `PrettyCtx` (and one `sizes` map)
before the `format` match, shared by both the `Pretty` and `Tree` arms,
instead of constructing it only inside the `Pretty` arm.

## Verification

- ✅ `em -tp rust`: root `dev-lang/rust` keeps its real `[ebuild NS]` bracket
  with old-versions/USE; every dependency reached but not in the plan (xz,
  sqlite, curl, zlib, perl, glibc, gcc, ...) shows as `[nomerge ...]` with its
  own USE/old-version, `(*)` on repeat visits (e.g. `libunistring`, `zlib`,
  `perl` each appear multiple times in the graph and are marked on the
  second+ occurrence) — 286 total rows in the full transitive walk vs 2 in
  the flat `-p rust` plan, i.e. `-t` is additive as intended.
- ✅ `em -tp --complete-graph rust`: llvm shows as `[nomerge] llvm-core/llvm-22.1.6`
  (correct — without `-uD` nothing moves, so `--complete-graph` is a no-op
  here too, consistent with [[slot-chain-completion]]'s own verification).
- ✅ fmt, clippy (workspace, all-targets), full test suite green.
- ✅ Timing: `em -tp @world` ~1.1s, in line with plain `-p @world`'s
  0.97-1.03s baseline — walking the full solved graph (not just the merge
  plan) added no material cost.
- Per [[live-verify-full-pretend-output]]: whole-output diff against a real
  `emerge -tep @world` capture (not just row counts) confirmed the row
  *content* (bracket, USE, old-version) now matches column-for-column; the
  *indent style* intentionally does not (see "What shipped").
