# For Maki — homework: rework `em -t`/`--tree` to be order-driven

**Current pin:** `master` @ `113eb843`

Not yet started — this is a design/implementation task, not a live-sandbox
verification one. Read this whole file before touching code; the comparison
against real `emerge --tree` was already done and shouldn't need redoing.

---

## The gap

Real `emerge --tree` and `em -t`/`--tree` build their tree from genuinely
different data, not just different rendering:

**Real emerge** (`_emerge/resolver/output_helpers.py`, `_tree_display` /
`_ordered_tree_display`, `output.py:818-838`): walks its actual computed
merge **order** (`mylist`, the topologically-sorted install sequence), not
the raw dependency graph. For each package in that order, it checks whether
the package is a child of whatever's on top of the current tree-building
stack — if so, it just extends the current branch. If not, it backtracks
through **parent** edges (`add_parents`, preferring a parent that avoids a
direct cycle) until it reconnects to something already shown, grafts the
node in at that point, then `_prune_tree_display` strips the redundant
`nomerge` filler once the real branch is drawn. Depth is "where does this
fall in the sequence actually built," not "how deep is this in the
dependency graph." Indentation is plain `" " * depth` — no connector lines
at all, the bracket status column stays flush-left.

**`em`** (`portage-cli/src/query/depgraph/output.rs:1695-1807`,
`print_tree`/`Tree::node`): does a straightforward forward DFS from the root
targets over the raw solver `DepEdge` list (the `children` map built from
`edges`), independent of the final install order. A package is visited once
globally (marked `(*)` on repeat, no re-expansion), depth is pure graph
distance from a root, and connectors are `tree`-command-style box-drawing
(`├──`/`└──`/`│`) — a deliberate, already-documented divergence (see the
comment at that line range).

The `[nomerge]` bracket criterion is conceptually aligned both ways
(anything shown only to keep the tree connected, not actually in the merge
plan, renders as nomerge) — that part is fine.

## Why it matters

em's tree reflects "what depends on what" from the targets down; real
emerge's reflects "what got built before what," reshaped into a tree. They
can diverge visibly whenever the solver reorders things — SCC tie-breaks,
soft RDEPEND cycle repair (see `install-order-scc-tiebreak-fix` and the
soft-order work in `for-sonnet.md` — both already-known sources of plan
reordering in this codebase) — since em's tree depth won't track that
reordering at all while real emerge's will by construction.

## Open question (Luca hasn't picked yet)

Two options, not yet decided:

1. **Rework `print_tree` to be order-driven**, matching real emerge's
   algorithm: walk the actual merge `order` (not `roots`+`edges` via
   forward DFS), backtrack through parent edges to graft each node onto the
   tree at the point it reconnects to something already shown, prune
   redundant nomerge filler. Bigger change — reshapes tree output whenever
   the plan gets reordered, which is exactly the point but is also a
   visible behavior change for anyone already using `-t` today.
2. **Leave the graph-DFS approach as-is**, treat the divergence as a
   documented, intentional design choice (already partly is, per the
   existing comment), and only revisit the *connector-style* cosmetic
   (box-drawing vs `emerge`'s plain-space indent) — which is a separate,
   smaller concern already parked under `info-tree-coloring-needs-review`
   in memory (Luca: "the coloring btw is off in some places," no specifics
   yet — that note is about a different, still-vague complaint, not this
   structural one, but the two might get looked at in the same pass).

**Do not pick one and implement it — ask Luca which direction first**, then
implement. This file exists so the comparison work (above) doesn't need
redoing when that conversation happens.

## Where to start reading

- `portage-cli/src/query/depgraph/output.rs:1695-1807` — `print_tree`,
  `Tree::node`, the current implementation.
- `portage-cli/src/query/depgraph/mod.rs` — where `roots`/`edges`/`order`
  get built and handed to `print_tree` (search for `print_tree` call site,
  currently ~line 1384).
- Real portage reference (installed on this box):
  `/usr/lib/python3.13/site-packages/_emerge/resolver/output_helpers.py`
  (`_tree_display`, `_ordered_tree_display`, `_prune_tree_display`) and
  `/usr/lib/python3.13/site-packages/_emerge/resolver/output.py:818-838`
  (`self.indent = " " * depth`, where the walk result actually gets
  rendered).
- `portage_atom_pubgrub::DepEdge` — confirmed this is the full solver
  requirement graph (RDEPEND included because Gentoo `virtual/*` packages
  need it — see the comment at `mod.rs:76`), not limited to the final
  resolved plan's edges, same as real portage's own `digraph`.

## Out of scope for this pass

- The vague "coloring is off in some places" complaint
  (`info-tree-coloring-needs-review` memory) — separate, needs concrete
  examples from Luca first, don't guess-fix it while touching this.
- Rewriting `-p`'s flat list rendering — untouched by either tree algorithm,
  `format_plan_parts` is shared and already correct for both.

## Results

Picked up 2026-08-10. **Direction chosen: Option 1 (order-driven rework),
keeping em's box-drawing connectors** (not switching to emerge's plain-space
indent).

Implemented in `portage-cli/src/query/depgraph/output.rs`:

- `print_tree` now drives the tree off the merge `order` (portage's
  `_ordered_tree_display`), not a forward DFS of the raw `edges`. Same
  signature — it already received `(roots, edges, order)`, which is exactly
  the three inputs the portage algorithm needs (no new resolver coupling; the
  only "resolver" read portage does is `conf.set_nodes`, mapped to em's
  `roots`).
- New `build_ordered_tree` + `WalkState::add_parents`: for each node in
  `order`, find the shallowest open ancestor that depends on it (child-edge
  membership) and extend the branch; if none, backtrack up parent edges to
  graft onto a shown node, emitting grafted filler as `[nomerge]`.
- `prune_tree`: direct port of `_prune_tree_display`, drops redundant
  `[nomerge]` filler once the real branch is drawn.
- `render_tree`: one O(n) pass over the pruned display list. Because the list
  is a pre-order traversal, last-child-ness and the vertical-rail prefix are
  recovered from the depth sequence alone — no tree structure or look-ahead.
- The bracket column stays flush-left (fixed-width, like `-p`); the
  box-drawing connector sits after it. `(*)` repeat-marker semantics and
  `[nomerge]` bracket selection (`ordered` flag) preserved.

Tests (`output::tests::tree_*`): linear chain (depth tracks order), diamond
graft (`order=[A,B,D,C]` → C reconnected under root), root-not-in-order shown
as nomerge filler, redundant-filler pruning, root-never-grafted-under-parent.

**Verification:** `cargo nextest run -p portage-cli` 409 passed / 5 skipped;
`cargo clippy -p portage-cli -- -D warnings` clean; doctests clean. The two
`rustfmt` diffs in the file header (import ordering at lines 5 and 59) are
**pre-existing on master**, not introduced here.

## Live parity check (2026-08-10)

Built `em` (`--profile quick`) and diffed `em -t` against real `emerge --tree`
on this box. **One real bug caught by this** (missed by unit tests alone):

- **`order` must be walked reversed.** Portage's `mylist` for `--tree` is the
  merge list reversed (root first — emerge prints "in reverse order"); em's
  `order` is install order (deps first). Without `order.iter().rev()` the tree
  *explodes*: the root renders as `[nomerge]` filler and every node appears
  multiple times (leaf-first grafting). Fixed: `build_ordered_tree` now
  iterates `order.iter().rev()`. Unit tests updated to feed install order.

After the fix, structural parity confirmed on cases where em and emerge
compute the same plan:

- `app-shells/bash --emptytree`: **identical** structure
  (bash→{readline→{pkgconfig→pkgconf, ncurses}, libintl}), same depths/order.
- `app-shells/dash --emptytree`: **identical** (dash→pkgconfig→pkgconf).
- `dev-libs/glib --emptytree`: 511 rows vs emerge's 461 — same order of
  magnitude; the gap is solver *scope* (em's emptytree rebuilds only the
  target's closure, emerge rebuilds @system/@world), not tree rendering.

Remaining observed differences are all **pre-existing solver/planning
divergences**, not tree bugs: non-emptytree plans often pull different deps
than emerge (e.g. emerge rebuilds `jinja2`/`markupsafe` under `pambase`, em
doesn't — an RDEPEND/changed-deps handling difference). The tree faithfully
renders whatever `order` it's given.
