# `--jobs`: a backwards RDEPEND edge is silently dropped from `build_blockers`

Status: ✅ landed 2026-08-24. Found 2026-08-23 during the first genuine,
full, real (non-pretend) `em --local` toolchain bootstrap run
([[local-bootstrap-provided]], `--jobs 48`).

**Two wrong diagnoses on the way here, both corrected same-session**:
first filed as "python-exec wrapper gap" (every wrapper file was actually
present and correct — the real interpreter just hadn't been built yet);
then, while planning the fix, initially attributed purely to
`build_blockers`'s `--jobs` scheduling — wrong altitude. The isolated
3-package resolve (`python`+`meson-format-array`+`gentoo-functions`
alone) got the *correct* order, proving the edge extraction and
cross-SCC topological sort were both sound; the bug only showed up
inside the full closure, meaning the real defect was in
`install_order`'s within-cycle repair logic, not the `--jobs` layer.

## What was actually found (enriched `--json`, `portage-cli/src/query/depgraph/output.rs`)

Added `order_ok`/`hard_cycle_edges`/`has_hard_cycle` to `--json` output
specifically to diagnose this (see `feat(depgraph): expose
order-violation info in --json output`). Used live against the real
repro and found:

1. **A genuine soft-cycle starvation defect** in
   `repair_soft_inversions` (`portage-atom-pubgrub/src/graph.rs`): it
   processes RDEPEND inversions in an arbitrary `(consumer, dep)` index
   order, greedily locking whichever don't immediately conflict with
   what's already accepted. A promotion that's genuinely valid on its
   own can get starved simply because an unrelated inversion was
   processed first and locked a path that blocks it. Fixed by
   prioritizing an inversion whose consumer is itself a BDEPEND target
   elsewhere (a build tool something else invokes) — those have a real
   "run me before my own runtime deps exist" failure mode an ordinary
   soft RDEPEND cycle doesn't. Real, tested improvement (167/167
   `portage-atom-pubgrub` tests still pass, including the exact
   regression tests from the earlier "soft-order #3" saga in
   `todo/done/for-sonnet.md`) — but did **not** fix this specific case.
2. **A genuine, irreducible 3-way bootstrap cycle**, found by tracing the
   still-violated edge after fix #1: `dev-build/meson-format-array`
   RDEPENDs on `python` (needs it to run); `python` RDEPENDs on
   `app-arch/zstd` (needs it at runtime); `zstd` BDEPENDs on
   `meson-format-array` (needs it to build). No linear order can satisfy
   all three — this only exists when bootstrapping all three from an
   empty prefix at once (on any mature system all three are already
   installed, so the cycle never manifests in practice).
3. **A separate, real inconsistency** found while verifying the fix for
   #2: `dependency_graph()` (`graph.rs`) walked every solved package's
   *full raw metadata* unconditionally, without applying the same
   "kept, not being rebuilt → skip build-time deps" (or "Provided → skip
   everything") filtering `compute_dependencies` (the actual solver)
   already uses. So even after registering `python`/`zstd` as
   `InstalledPolicy::Provided` (correctly making the *solver* skip their
   own BDEPEND), the *display/ordering* graph still manufactured a
   phantom `zstd → BDEPEND → meson-format-array` edge from raw metadata,
   independently recreating the same conflict the solver no longer
   needed. Fixed by applying the identical filter in `dependency_graph`.

## The fix (four parts, all landed)

- `graph.rs`: `repair_soft_inversions` prioritizes BDEPEND-target
  consumers' RDEPEND promotions (real improvement, general).
- `graph.rs`: `dependency_graph()` now skips DEPEND/BDEPEND (or
  everything, for `Provided`) for a kept/not-rebuilt package, matching
  `compute_dependencies` — general consistency fix, not specific to this
  bug.
- `portage-cli/src/query/depgraph/output.rs`: `--json` gained
  `order_ok`/`hard_cycle_edges`/`has_hard_cycle` (kept — general
  diagnostic value, not a throwaway).
- `portage-cli/src/setup/provided.rs`: added `dev-lang/python` and
  `dev-build/meson-format-array` to `TIER1`. Same
  bootstrap-scaffolding pattern already used for glibc/gcc
  ([[local-bootstrap-provided]]): both are explicit `toolchain_plan`
  steps (`python` is one directly; `meson-format-array` never is, only
  ever reached transitively), so `InstalledPolicy::Provided`'s
  root-target override already lets the real "python" step build a real
  interpreter despite it being provided for everyone else — the same
  mechanism that already lets the libc step build a real glibc.

## Verified

Live, `em -p --json --local DIR dev-lang/python sys-apps/gentoo-functions`
(the representative shape — `meson-format-array` reached transitively,
never named as an explicit target): the `meson-format-array → RDEPEND →
python` edge that was `order_ok: false` before every fix is gone from
the violated list entirely after all four land. `zstd` itself no longer
appears in the plan at all (correctly satisfied via `Provided`, dropped
by the existing already-installed plan-membership filter). Full
workspace: `cargo nextest run --workspace` clean, clippy/fmt clean.

**Not yet done**: a full real (non-pretend) end-to-end bootstrap through
all 6 `toolchain_plan` steps still hasn't completed in one run — the
earlier real attempt separately hit `app-arch/unzip`'s unrelated GCC-16
prototype-conflict bug ([[local-bootstrap-unzip-gcc16-prototype]]) before
reaching this far. A fresh full run should get further now but hasn't
been confirmed to complete.

## Residual: a separate, similar-shaped finding, not yet investigated

Same diagnostic pass also flagged three `RDEPEND → app-text/
build-docbook-catalog` edges as `order_ok: false`, unconfirmed whether
this is a real problem or benign — see
[[docbook-catalog-rdepend-order]].
