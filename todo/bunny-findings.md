# Bunny findings — simplification and performance queue

Status: active implementation queue
Updated: 2026-09-24
Scope: workspace-wide review of `em`, `portage-resolve`, `portage-repo`, solver bridges, VDB, binpkg, and supporting libraries.

This file is the source of truth for the review findings. Work one phase at a time; do not bundle a broad architectural rewrite with a correctness fix.

Status markers: `[ ]` open · `[~]` in progress · `[x]` complete · `[!]` blocked/deferred

## Baseline and measurement rules

- Host used for the initial measurements: AmpereOne, 128 CPUs, 255 GiB RAM.
- Repository: 33,121 md5-cache files / 33,122 ebuilds.
- VDB: 727 package directories / about 157 MB of `CONTENTS` data.
- Initial sequential release timings: firefox `1.082 s`, gcc `0.878 s`, libreoffice `1.373 s`.
- Initial `dhat` run (`em -p sys-devel/gcc`): 1.22 GB total allocation, 387 MB peak.
- Existing `clippy` and `fmt` checks were clean at review time.
- Commands that require configuration changes intentionally exit non-zero; timing comparisons use `--ignore-failure`.
- Record before/after measurements in the relevant item and do not claim a speedup from a profile alone.

## Phase 0 — correctness blockers

- [x] **BF-001 — Unify version acceptance filtering.** `Adapter::version_accepted` and `target_package` now call the complete `version_accepted_for` predicate, including PROPERTIES and RESTRICT. Added `target_package_uses_properties_and_restrict_acceptance`; it fails on the pre-fix code and passes after the repair. Verified with all 160 `portage-resolve` tests, targeted clippy, fmt, and `git diff --check`.
- [x] **BF-002 — Remove process-global CWD mutation from parallel phases.** `run_phase` now sets the brush shell's working directory instead of calling `std::env::set_current_dir`; phase commands remain shell-local and the process cwd is unchanged. Added `run_phase_keeps_the_process_working_directory_unchanged`, which fails on the pre-fix mutation and verifies the phase still runs in its work directory. Verified with all 534 `portage-repo` tests, targeted clippy, fmt, and `git diff --check`.
- [x] **BF-003 — Make repository cache invalidation eclass-aware.** `repo_entries` now validates covered primary, secondary, and gap-index entries with a shared eclass digest memo before trusting them; stale entries fall through to the normal suspect/source chain. Added `a_changed_eclass_invalidates_an_otherwise_covered_cache_entry`, which exercises the durable sidecar path and fails on the pre-fix behavior. Verified with all 533 `portage-repo` tests, targeted clippy, fmt, and `git diff --check`.
- [x] **BF-004 — Replace second-resolution sync stamps.** `Repository::sync_stamp()` now includes nanosecond mtimes and the `timestamp.chk` content digest, while retaining the repository-root mtime component. Added `sync_stamp_tracks_same_length_marker_changes_at_the_same_mtime`, which fails on the old seconds-plus-length stamp. Verified with all 535 `portage-repo` tests, targeted clippy, fmt, and `git diff --check`.
- [x] **BF-005 — Make dependency-tree traversal consistent.** Added `DepEntry::walk_atoms` and reused it for CPN, blocker, and repository dependency walks. Availability now treats `ExactlyOneOf` like an at-least-one choice, keeps `AtMostOneOf` optional, and no longer keeps an `AnyOf` branch when another branch is available. Added regressions for all three group forms and nested traversal. Verified with all 157 `portage-atom` and 163 `portage-resolve` tests, targeted clippy, fmt, and `git diff --check`.
- [x] **BF-006 — Do not swallow source-worker panics.** Eclass parsing now returns a structured error, raises the existing die signal, and metadata sourcing checks both shell status and that signal. `source_parallel_join` collects worker `JoinError`s as `Error::SourceWorker`; the stream wrapper logs the fatal pool error. Added malformed-eclass regen and worker-panic regressions. Verified with all 537 `portage-repo` tests, targeted clippy, fmt, and `git diff --check`.

**Phase 0 gate:** targeted regressions fail on the pre-fix code and pass after the owner-boundary repair; `cargo nextest run -p portage-resolve -p portage-repo -p portage-cli` plus clippy/fmt are clean. **Passed 2026-09-24:** 1,364 tests, 0 failures, 8 ignored.

## Phase 1 — make the hot path measurable

- [x] **BF-101 — Add phase timings/counters for resolve.** `depgraph` now records repository/VDB/use-env loading, duplicate collapse, slot-map construction, install ordering, trims, and dependency-graph timings, plus solve-round/graph/trim counters; provider and solver iterations emit per-call debug timings. Verified with all 664 `portage-cli` tests, clippy, fmt, and `git diff --check`.
- [x] **BF-102 — Add a repository-load benchmark.** Added `portage-bench --bench repo_load`, which exercises real-tree `repo_entries()` and reports entry count; the 33,121-entry Gentoo tree measured 0.766 s median on 2026-09-24. The benchmark and docs compile cleanly and skip cleanly when no metadata tree is available.
- [x] **BF-103 — Add trim scaling fixtures.** Added `trim_scaling` for 16/32/64/128-package plans with matching synthetic VDBs; it reports pair counts, evaluated-cache universe, and steady-state allocation counts. The 128-package fixture covers 8,128 BDEPEND and 16,256 DEPEND consumer checks; current medians are 0.54 ms and 0.18 ms, with about 2,225 and 270 allocations per iteration respectively.
- [x] **BF-104 — Add graph/order measurements.** `GraphStats`/`install_order_with_stats` now report graph builds, selected nodes, edges, virtual expansions, SCC count/size, and soft-repair reachability work. `graph_scaling` covers 32/64/128-package cyclic plans; the 128-package fixture reports 129 nodes, 129 edges, one virtual expansion, largest SCC 128, and one repair probe over 127 nodes.

**Phase 1 gate:** a phase profile can explain a complete `em -p` invocation, and every later performance item has a reproducible before/after benchmark. **Passed 2026-09-24:** repository-load, availability, trim, and cyclic graph fixtures are registered and documented.

## Phase 2 — resolver and solver performance

- [x] **BF-201 — Cache evaluated dependencies by `(PortagePackage, Version)`.** `EvaluatedDeps` now memoizes each dep class with `OnceCell`, and each trim pass snapshots one evaluated entry per plan package before candidate scans. The 128-package benchmark improves from 3.64 ms (prefix view without cache) to 0.40 ms for DEPEND before the consumer index, while retaining group-aware behavior; resolver tests and clippy pass.
- [x] **BF-202 — Index trim consumers by CPN.** BDEPEND and DEPEND trims now build a reverse CPN-to-consumer index from evaluated dependency trees, while retaining group-aware satisfaction checks. The 128-package benchmark drops DEPEND from 0.39 ms to 0.18 ms and BDEPEND from 0.57 ms to 0.54 ms; resolver tests and clippy pass.
- [x] **BF-203 — Index `Avail` by CPN.** `Avail` maintains a CPN-to-entry index while preserving lazy USE/IUSE reads and mutation/clone semantics. `avail_lookup` remains flat at 16.82/16.83/16.85 ns for 100/1,000/10,000 entries; the prefix view avoids reintroducing per-consumer index clones in trim. Resolver tests, clippy, fmt, and benchmark compilation pass.
- [x] **BF-204 — Replace per-consumer `Avail` clones with views.** `AvailPrefix` shares the immutable base and exposes only the sorted kept-CPV prefix for each consumer; no mutable availability set is shared. The 128-package BDEPEND benchmark drops from about 198 ms to 3.09 ms before dependency caching, and the prefix invariant has focused coverage.
- [x] **BF-205 — Share raw per-invocation VDB snapshots.** `BrootSnapshot` now captures ordered BROOT/prefix rows once and feeds host-installed, BDEPEND availability, trims, root closure, and preflight; availability keeps the union while keyed host insertion keeps the prefix row last.
  Owner-boundary regressions remove the roots after capture to prove the adapters do not rescan. The 128/512/2048-host-entry benchmark measures 32.5%/25.0%/3.5% lower median load-and-derive time than two independent scans; the 2,048-entry result is close to the noise floor, so no broad end-to-end speedup is inferred. Broader root-specific sharing remains BF-404. Linked design: `todo/dedup-availability-walks.md`.
- [x] **BF-206 — Prepare policy-scoped version facts.** `PolicyFactsCache` now keys acceptance, stable-keyword status, and pre-cede effective USE by CPV and `package.use` generation, with ceding applied to a cloned base after lookup; duplicate-repo collapse deliberately bypasses the cache until one CPV entry remains. Generation, conditional-license invalidation, ceding, and duplicate-collapse regressions cover the owner boundaries. The `policy_facts` benchmark shows 61.9–65.9% lower warm-path time and 58.4–60.8% lower four-read time on its 128/512/2,048-CPN fixture; cold-cache overhead and multi-generation cold rows are reported separately, with no end-to-end claim.
- [x] **BF-207 — Index policy lists.** `AcceptKeywords`, the shared accept-set overlay engine, and final mask/unmask lists now bucket matching entries by CPN while preserving original order and the existing version/slot/repository predicates. The full-filter benchmark measures 58.3%/85.6%/96.0% lower time at 128/512/2,048 noise entries; owner tests cover token/overlay order and mask/unmask behavior.
- [ ] **BF-208 — Add a CPV cache index.** Replace repeated linear `find_cache()` version searches with a per-invocation index or equivalent typed lookup.
- [x] **BF-209 — Reuse dependency graphs.** `install_order_with_stats` returns the graph and order together; the PubGrub solver adapter and CLI depgraph now consume that single result instead of rebuilding the graph. Existing graph/solver/CLI tests pass, and `graph_scaling` confirms one graph build per measured pass; no speedup is inferred without a matched before/after run.
- [ ] **BF-210 — Remove graph string-key churn.** Use typed node IDs or cached node indices; deduplicate adjacency; reuse reachability marks instead of allocating a `Vec<bool>` per query. Treat large-SCC optimization as measurement-driven.
- [ ] **BF-211 — Correct selected virtual-branch expansion.** Retain selected virtual versions for graph construction so unrelated independently selected branches do not create false edges. Add OR/USE-decision regressions.
- [ ] **BF-212 — Remove avoidable provider post-processing allocations.** Use a no-drop fast path instead of `partition` when all targets are known; avoid materializing pairwise OR alternatives unless needed. Measure before broad changes.

**Phase 2 gate:** trim and provider benchmarks improve without plan-set, USE, slot, or cycle regressions; existing resolver parity tests remain byte-identical. **Current batch verification (2026-09-24):** 2,197 nextest tests and the plain workspace test suite pass; doctests, clippy, rustdoc, fmt, docs-check, MSRV 1.95 verification, and benchmark smoke checks pass. The gate remains open pending the remaining provider/repository items.

## Phase 3 — repository loading and sourcing

- [ ] **BF-301 — Reuse one shell per source worker.** `source_parallel_join()` currently constructs a shell per ebuild through `source_one()`. Use the existing hermetic baseline behavior to reuse a worker-local shell; keep `source_single()` fresh-shell semantics.
- [ ] **BF-302 — Resolve distdir writability once per shell/context.** Avoid repeated probe-file create/write/remove operations for every sourced ebuild and phase. Invalidate only when configuration changes.
- [ ] **BF-303 — Share eclass digest/path memo during live sourcing.** Use the existing AST/digest caches for newly sourced entries and avoid repeated directory probes when ASTs are already cached.
- [ ] **BF-304 — Avoid repeated ebuild reads.** Carry pre-read bytes or MD5 through EAPI detection, sourcing, and cache construction where practical; measure the regen I/O change.
- [ ] **BF-305 — Stream metadata serialization.** Replace nested temporary `String`/`Vec<String>` construction in `portage-metadata` with direct `fmt::Write` or a prepared writer.
- [ ] **BF-306 — Avoid redundant staging directory creation.** Reuse prepared category directories and a prepared staging writer instead of creating a new `DirMetadataCache` and calling `create_dir_all` per entry.
- [ ] **BF-307 — Move cache discovery into blocking work.** Do not perform synchronous jwalk before the first await; use a shared job budget and move descriptor chunks instead of cloning them.
- [ ] **BF-308 — Bound and share worker budgets.** Stop defaulting every cache/source stage to all available CPUs. Benchmark conservative worker counts and one budget across overlapping stages.
- [ ] **BF-309 — Validate `jobs = 0`.** Reject or normalize zero workers in source and cache paths; add a regression for the current divide-by-zero/deadlock behavior.
- [ ] **BF-310 — Reconcile gap-index lifecycle.** Ensure successful regen updates/removes the sidecar and does not append already-covered CPVs.

**Phase 3 gate:** cache-less/overlay and regen benchmarks complete correctly with lower system time/allocation, and no partial cache is published after worker failure.

## Phase 4 — merge, VDB, activity, and binpkg paths

- [ ] **BF-401 — Build a merge-batch VDB ownership index.** Replace per-package full `CONTENTS` scans in `Vdb::find_collisions()` with a run-scoped path-to-owner index. Define external-writer coherence behavior.
- [ ] **BF-402 — Reuse preserve-libs state through a merge.** Avoid rebuilding the link graph/registry for every replacement and avoid duplicate load/store/reclaim scans.
- [ ] **BF-403 — Scope VDB field caching.** Replace the global raw-string mutex cache with a run-scoped/shared parsed cache or equivalent; avoid full-cache `retain()` invalidation and repeated typed parsing while preserving external-writer policy.
- [ ] **BF-404 — Share VDB snapshots across resolver/closure/preflight.** Remove repeated directory enumeration and package construction without collapsing intentionally different root-selection semantics.
- [ ] **BF-405 — Make activity history bounded/indexed.** First avoid loading history when no orphan path needs it; then measure real history size before choosing rotation, tail reads, or an index. Link `todo/activity-storage-format.md`.
- [ ] **BF-406 — Throttle live activity writes/queues.** Measure phase-transition atomic rewrites and unbounded background queue growth before changing durability/latency tradeoffs.
- [ ] **BF-407 — Avoid duplicate binpkg work.** Skip build-environment key calculation before installed-package skips; compute reuse/classification once and pass it to the action path.
- [ ] **BF-408 — Make binhost cache keys collision-safe and writes atomic.** Hash the complete URL, include port/scheme/query, and use same-directory temporary files plus rename.
- [ ] **BF-409 — Stream GPKG verification/decompression.** Avoid complete member/image buffering and repeated tar/zstd subprocess passes for binpkg-heavy workflows.

**Phase 4 gate:** merge/preserve-libs and binpkg tests cover multi-package and concurrent cases; activity history behavior remains correct with bounded resource use.

## Phase 5 — simplification and architecture decisions

- [ ] **BF-501 — Decide the `portage-solver` abstraction direction.** Either make both bridges consume the shared model and implement `Solver`, or remove the unused facade/models after checking publishable external intent.
- [ ] **BF-502 — Centralize PMS version matching.** Make `Dep::matches_cpv`, mask matching, and Resolvo matching use one `portage-atom` primitive; avoid revision-cloning comparisons.
- [ ] **BF-503 — Centralize effective-USE construction.** Remove the duplicated pre-cede folds in filtering, solver ingestion, and policy evaluation.
- [ ] **BF-504 — Reuse typed maintenance parsers.** Wire `maint moveinst` to `portage-repo`'s tested `ProfileUpdates` parser.
- [ ] **BF-505 — Unify binpkg entry construction.** Make scan and regen share `BUILD_ID` and other metadata fallback behavior.
- [ ] **BF-506 — Remove dead unpublishable surface.** Remove the unused `portage-solver` dependency from `portage-repo` and audit unused public wrappers/reexports before deleting.
- [ ] **BF-507 — Remove duplicate SRC_URI traversal.** Use `SrcUriEntry::collect_filenames` from the canonical implementation and delete unused copies.
- [ ] **BF-508 — Fix latent type contracts.** Decide and repair `UseDep` ordering, `Version` raw/field consistency and documented numeric limits, and `SlotDep` impossible states.
- [ ] **BF-509 — Split god modules only after seams stabilize.** Consider `repo`, `depgraph`, and `ebuild` splits after correctness and performance ownership is clear; do not mix this with behavior changes.
- [ ] **BF-510 — Reconcile stale durable documentation/comments.** Update claims about solver sharing, REQUIRED_USE ownership, binhost settings, and current placeholders; keep implementation history out of current invariants.

**Phase 5 gate:** API/dependency cleanup is behavior-neutral, documented, and covered by the owning crate tests; no speculative performance claims are used as justification.

## Explicitly deferred until measured

- [ ] **BF-901 — Version representation/boxing.** Measure `size_of::<Version>()`, parse allocations, and comparison cost before changing the public representation.
- [ ] **BF-902 — Alternative interner backends/features.** Do not prune or redesign backend features based on speculation; current profiles do not show an interner bottleneck.
- [ ] **BF-903 — Full solver-bridge migration.** Do not merge PubGrub and Resolvo storage models until the default PubGrub path is stable and the shared API is proven useful.
- [ ] **BF-904 — Full activity format replacement.** Keep JSONL until real history measurements justify rotation/indexing or a new format.

## Verification gates for every completed item

- [ ] Focused owner-boundary tests added only when they protect an observable contract and fail on the pre-fix code.
- [ ] `cargo nextest run --workspace --exclude portage-bench`.
- [ ] `cargo test --workspace --exclude portage-bench --doc`.
- [ ] `cargo clippy --workspace --exclude portage-bench -- -D warnings`.
- [ ] `cargo fmt --all -- --check`.
- [ ] For performance items: before/after benchmark and allocation note.
- [ ] For behavior-sensitive items: targeted parity/live check where the task is not purely internal.
