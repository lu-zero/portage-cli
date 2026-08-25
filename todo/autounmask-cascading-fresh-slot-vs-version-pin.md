# `em`'s autounmask doesn't discover a masked candidate buried multiple hops deep

Status: 🟡 **implemented + live-verified 2026-08-25** — solver-core
widening, third-tier ordering, Choice demotion, two-phase solve, and the
persistence policy (see § Persistence below) all landed. Root cause
confirmed by direct evidence 2026-08-24; found during the crossdev-stages
replacement retest, testing `--ex-pkg sys-devel/clang-crossdev-wrappers`
(i586 pilot sandbox). Opus design review done; design settled with Luca —
see § Settled decisions below.

## Persistence policy (settled with Luca 2026-08-25) — verified live

Invocation-mode-gated, not flag-gated; identical for **every** subcommand:

| invocation | disk behaviour |
|------------|----------------|
| `em -p`    | never writes — widened picks exist in memory only; report says "applied in memory … --autounmask-write persists them"; exit 0 |
| `em -a`    | confirm prompt: yes → writes (slot-scoped), no → ">>> Quitting." |
| real run   | writes unconditionally, then proceeds to merge |

Implementation: `DepgraphOpts::autounmask_persist` (`Never`/`Ask`/`Always`),
translated by the merge path from `-p`/`-a`; read-only queries and the
internal gcc-version probe pass `Never`. Widened selections never set the
non-zero exit nor block the merge — only DroppedDep advisories do.
Widened entries persist **slot-scoped** (`cat/pkg:SLOT **`, chosen over
exact-version pins: pre-release ecosystems churn too fast for exact pins);
the DroppedDep path keeps portage-parity exact pins.

Live matrix on `pilot-i586-em`: `-p` → exit 0, config byte-identical;
`-a`+no → Quitting, unchanged; `-a`+yes → 14 accept_keywords files +
llvmgold unmask written slot-scoped; real run → same writes, merge
proceeds into real builds (timeout-killed).

## Benchmarks (2026-08-25, built-in resolve timing)

| case | time |
|------|------|
| strict phase-1 success (`cross-i586…/gcc`) | 0.11 s |
| widening armed but unused (`--target @system`) | 0.10 s |
| phase 2 fires (clang-crossdev-wrappers) | 0.61 s |
| same target after persisting | 0.14 s |

Not the same bug as `todo/done/autounmask-convergence.md` (resolved
2026-06-19) — that one covers the *soft-drop* case (USE/license/keyword
advisories from `DroppedDep`), confirmed converging correctly in a
single non-`-p` invocation via the existing solve→collect→re-solve
fixpoint (`query/depgraph/autounmask.rs`/`mod.rs`). This bug is a
different, disconnected code path: a *hard* PubGrub solve failure
(`choose_version` returning `None`), which that fixpoint never touches
at all since it only ever looks at gracefully-dropped deps.

This file went through two wrong analyses before landing here (kept
out of the file now, not worth preserving): first an elaborate
"resolver semantics" theory built on bad test invocations, then a
"`host_arch_keyword_line` should prefer keyword-visible over
newest-on-disk" framing. Luca's correction: **`em crossdev` reaching
for the newest version and unmasking it is exactly the intended
behavior** — that's the whole point of crossdev pulling in bleeding-edge
toolchain bits. The real problem is one level deeper.

## Confirmed root cause

`cross-i586-pc-linux-gnu/clang-crossdev-wrappers:23` depends on
`llvm-core/clang:23`, which (in its `23.1.0_pre20260724` /`_rc2`/`_rc3`/
`.9999` point releases) depends on a matching `llvm-core/clang-common`
version. Checked the actual md5-cache entry directly in the
`pilot-i586-em` sandbox:

```
/var/db/repos/gentoo/metadata/md5-cache/llvm-core/clang-common-23.1.0_pre20260724
```

Full entry present, fully parsed (BDEPEND/IDEPEND/PDEPEND/etc. all
populated) — but **no `KEYWORDS=` line at all**. This isn't a cache gap
or an em bug — it's genuine upstream reality: LLVM's live/pre-release
snapshots routinely ship parts of the dependency chain (here,
`clang-common`, three hops down from the `--ex-pkg` atom the caller
actually named) with no keywords whatsoever, inconsistently with
sibling packages at the same point release. That's the "bogus
keywording" — not a policy mask like `clang-crossdev-wrappers` itself
carries, just upstream incompleteness on one piece of a fast-moving
dependency graph.

Ran `em -p --autounmask --autounmask-write
cross-i586-pc-linux-gnu/clang-crossdev-wrappers` **five times in a
row** (simulating repeated passes the way autounmask cascades are
normally driven to convergence) — byte-identical failure every time,
zero new suggestions surfaced:

```
Because there is no version of llvm-core/clang-common:0 in
>=23.1.0_pre20260724, <23.1.0_rc2 and there is no version of
llvm-core/clang-common:0 in >=23.1.0.9999, ... is forbidden.
...
And because cross-i586-pc-linux-gnu/clang-crossdev-wrappers:23 23
depends on llvm-core/clang:23 and the requested targets depends on
cross-i586-pc-linux-gnu/clang-crossdev-wrappers:23, the requested
targets is forbidden.
```

No `--autounmask-write` suggestion for `clang-common` ever appears —
not "accept `~x86`", not a `package.mask` bypass line, nothing. Contrast
with the earlier (now-corrected) finding that a *shallow* masked
dependency (one hop from the requested atom) does get a real, correct
per-edge autounmask suggestion each pass. Here the masked candidate is
three hops down inside a compound PubGrub derivation spanning several
of `clang:23`'s own point-releases at once (`is forbidden (1)`,
`is forbidden (2)`-style chained clauses) — and autounmask's discovery
doesn't walk that far / doesn't unpack a compound derivation to find
the single relaxation that would satisfy it. It just reports the raw
unsatisfiability as a hard failure.

## What's NOT the bug

- `host_arch_keyword_line` reaching for newest-on-disk (23) instead of
  newest-keyworded (21) — **intended**, not a bug. Crossdev's job is to
  push past exactly this kind of incomplete upstream keywording.
- The blanket `**` override for `clang-crossdev-wrappers` itself —
  correct and necessary (it's permanently policy-masked regardless of
  slot).

## Opus design review (2026-08-24) — corrections + settled design

Full independent review: read the actual code, reproduced the bug live
against the release binary, and A/B'd against real portage on the same
scratch config. Corrected several things in the working diagnosis above
and produced a concrete design. Key points (see session transcript for
the full report if more detail is needed):

**Corrections to the diagnosis above:**
- The soft/hard boundary isn't "shallow vs deep" — it's whether the dep
  *node* is absent (`DroppedDep`) vs *present but the requested version
  range is empty*. `clang-common` has plenty of accepted versions (17–22);
  only the exact range `clang:23` needs is empty. Depth was a red
  herring — a one-hop dependency hitting this same shape would fail
  identically.
- `depend_trim.rs`/`bdepend_trim.rs` are post-solve *plan* trims, not
  the soft-drop mechanism — that part of the earlier diagnosis was wrong.
- **A separate, more likely culprit for the exact `--ex-pkg` failure**:
  `em -p sys-devel/clang-crossdev-wrappers` (a directly-named masked
  root target) already prints correct reasons today (`all ebuilds
  masked ... (missing keyword, package.mask)`, via root-target
  pre-classification in `query/depgraph/mod.rs`) — but those `reasons`
  are never merged into `autounmask_candidates`, so `--autounmask-write`
  silently writes nothing for a masked root target. Independent,
  standalone bug, fixable separately from everything below.

**Validated against real portage** (same scratch config,
`emerge -p --autounmask --autounmask-write=n llvm-core/clang:23`):
portage builds a *complete plan* in one pass — the whole chain
(`clang` → `clang-common` → `compiler-rt` → `compiler-rt-sanitizers` →
`clang-runtime` → `clang-rtlib-config`, ~10+ rows), masked/unstable
picks marked `*` in the status column — then derives the keyword-change
list *from the plan*, not from failure analysis. This confirms "widen
candidate supply, tag, scan the result" over "walk failure derivations
deeper."

**Architectural finding that settles where the fix plugs in**: live
widening inside `choose_version` is **unsound** — PubGrub permanently
stores `NoVersions` incompatibilities as it solves, so introducing a
candidate later corrupts the incompatibility store (and
`PortageDependencyProvider` doesn't even retain the repo to do this).
Widening must happen at provider-construction time, **unconditionally
per-package** (not range-scoped — construction time can't know which
ranges will be queried), with strict tiering enforced in
`choose_version` itself (accepted candidates always preferred; tagged
ones only chosen when no accepted candidate is in range).

**Real correctness risk found**: `||`-group (`Choice`) node branches are
numbered by listing order, not acceptance. Under naive widening, a
first-listed branch whose only candidate is masked would beat a
second-listed branch with a visible candidate, and PubGrub wouldn't
backtrack to catch it (both branches are satisfiable) — a spurious `**`
with no failure to surface it. Needs explicit demotion logic in
`register_virtual_choices` (locally decidable: check whether the
branch's target package `has_accepted`).

**Correction to this file's own earlier claim**: `host_arch_keyword_line`
should **not** be removed if this lands. It exists specifically to avoid
preferring live `9999` ebuilds — which is exactly what real portage
picked under `**` in the A/B (`clang-23.1.0.9999`, `clang-common-
24.0.0.9999`) and what naive widening would reproduce for `em crossdev`.
The two mechanisms never overlapped anyway: `host_arch_keyword_line`
only ever writes lines for `cross-<tuple>/*` packages, never for a
transitive host dependency like `llvm-core/clang-common`.

### Concrete design (types + insertion points)

- `PackageVersions`/`VersionData` gain a `needs_unmask: bool` field
  (false everywhere except the real `Adapter`).
- `PackageData` gains a cached `has_accepted: bool` so the common case
  (no widening needed) short-circuits with one bool check.
- `Adapter` gains an `autounmask: bool` mode flag (same shape as the
  existing `autosolve_use`), gating whether `versions_for`/`slots_for`
  include tagged candidates at all — **default resolve stays
  byte-identical and cost-identical to today**; only `--autounmask`
  (currently a dead flag, defined but threaded nowhere — free to give it
  real meaning) pays the widened cost.
- Two-phase solve under `--autounmask`: try the normal filtered solve
  first; only build the widened provider and re-solve if phase 1 fails
  or a root target is fully masked. Confines the ~1.76x candidate-count
  cost (measured on this tree: 32,800 cache entries, 18,609
  keyword-accepted) to runs that would otherwise hard-fail.
- `choose_version`: one tiering insertion right after the empty-check —
  filter to non-tagged candidates when any exist in range, else fall
  through to the full (tagged) set. This single change covers every
  `max()`-based heuristic downstream for free; two other `max()` sites
  (`post_solve.rs`'s `upgrade_to`, `validate.rs`'s held-back-target
  check) need their own explicit `!needs_unmask` guard since they don't
  route through `choose_version`.
- `slots_for`: rank tagged-only slots strictly below every accepted
  slot (extends `rank_slots_by_version`'s existing ascending order —
  "prepend tagged slots below the accepted ones").
- Reporting: after solve, scan the final solution for any `needs_unmask`
  selections and convert each to an `AutounmaskCandidate` via the
  existing `filter_reasons_for` (no new reason logic needed) — but do
  **not** apply the existing `DroppedDep` post-filter
  (`!solution_cpns.contains(cpn) && new_needed_cpns.contains(cpn)`) to
  these; that filter exists to prune drop-noise and would discard every
  tagged selection, since by construction they're in the solution.
- Keep `find_autounmask_candidates`/`DroppedDep` as-is — it's the only
  mechanism when `--autounmask` is off (must stay unconditional per
  today's design) and handles `||`-with-alternatives suppression the
  plan-scan can't see. The two compose, don't conflict.
- Plan display: `status_field`'s unused byte index 6 is exactly where
  portage puts `*` — trivial to wire up for `[ebuild  NS   *]` parity.

#### Settled with Luca (2026-08-24, after the Opus review) — supersedes the open questions below

1. **Widening is internal, crossdev-scoped.** Not wired to the user-facing
   `--autounmask` flag: that would change every plain merge's semantics and
   force designing flag "levels" (shallow vs deep) — deferred. The resolve
   layer gains an adapter-level widened mode; `em crossdev`'s own flows turn
   it on (`--ex-pkg` first; base toolchain steps ride the same mechanism).
   Two-phase still applies inside those flows: normal solve first, widened
   re-solve on failure.
2. **Third tier lands now**, not deferred: accepted > tagged-release >
   tagged-live. `host_arch_keyword_line` stays (it pins cross packages;
   the tiers govern what happens when nothing is pinned).
3. **Choice-node demotion** uses "branch has an accepted *or*
   release-tagged candidate" — a live-only branch loses to a later branch
   with anything real.
4. **Root-target phase-2 participation**: include, but gated on proper
   testing first — small synthetic repo + VDB driving a fully-filtered
   bare atom end-to-end as an integration test.
5. **The standalone root-target `reasons`-merge fix is parked** — its
   motivating example was wrong: bare `sys-devel/clang-crossdev-wrappers`
   can never install (cross-only package, host category is not a valid
   install target), so "report it correctly" is answering a request that
   should fail anyway. Revisit if a real non-cross case appears.
6. **Benchmark before profiling**: measure widened-vs-normal resolve cost
   on the existing benches before any dhat/peak-heap work.
7. **Version pinning** (`[[crossdev-gcc-version-flag]]`) is a separate
   second task; don't fold it into this one.

## Open questions (from the review — only 4 remains live)

1. ~~Does widening also need to reach the pre-solve root-target
   classification path?~~ — settled: yes, but behind the synthetic
   repo+VDB integration test (settled #4).
2. ~~Third tier or defer?~~ — settled: third tier now (settled #2).
3. Should the `cross-<tuple>/*` alias category continue to dodge
   `package.mask` (today's `mask_matches` compares whole CPNs, so
   Gentoo's `sys-devel/clang-crossdev-wrappers` mask atom can't match
   the aliased `cross-i586-pc-linux-gnu/clang-crossdev-wrappers` cpv) —
   possibly correct parity with real crossdev's own overlay symlinking,
   but currently an emergent accident, not a decision anyone made.
4. Provenance chains (portage prints `# required by <parent>::<repo>`
   above each change) — `AutounmaskCandidate` has no parent field;
   buildable from `RoundOutcome`'s `edges`, but a separate, orthogonal
   change from this one.
5. ~~dhat peak-heap check before landing?~~ — settled: benchmark first,
   profile only if the numbers say memory matters (settled #6).
6. `--autounmask-keep-masks` (portage's escape hatch for when a
   blanket `**` chain is too dangerous to write) — not required for
   this change, but likely to be asked for once `em` starts emitting
   long `**` chains.

## How to attack (revised per the settled decisions)

1. ~~Implement the widening design~~ — **landed 2026-08-24**: adapter-level
   `autounmask_widen` flag (`portage-resolve`), `needs_unmask` tag through
   `PackageVersions`/`VersionData`, tiering in `choose_version`
   (`filter_to_preferred_tier`), branch-tag propagation
   (`propagate_needs_unmask`), two-phase retry in `depgraph()`'s solve round,
   `emerge_atoms` widens automatically under `--target`.
2. ~~Third-tier ordering + Choice demotion~~ — landed in the same change.
3. ~~Post-solve scan~~ — landed: widened selections merge into the autounmask
   report after the dropped-dep post-filter (`widened_autounmask_candidates`).
4. Bench widened-vs-normal resolve before considering any memory work;
   dhat only if the numbers say so.
5. Root-target phase-2 participation after a synthetic repo+VDB
   integration test exists (settled #4).
6. Regression-test: `em -p cross-i586-pc-linux-gnu/
   clang-crossdev-wrappers` under an active `--target` in `pilot-i586-em`
   should converge in one pass to a real plan, matching real
   crossdev/emerge for the same package today.
