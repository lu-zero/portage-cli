# Bunny findings — simplification and performance queue

Status: active implementation queue
Updated: 2026-09-25
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
- [x] **BF-208 — Add a CPV cache index.** `RepoData` now builds an exact-CPV → vector-position index after duplicate collapse; `find_cache`, `desired_use`, `cpns_for`, and live autounmask metadata lookups use it, while intentional version-range scans and raw duplicate data remain unchanged. At 32/128 versions per CPN the indexed lookup fixture is 29.7–40.6%/81.5–88.0% faster; eight-version rows show 65.7–88.3% overhead from the index, which is recorded rather than hidden. Added CPN-scoped unsorted-version and collapse-winner regressions.
- [x] **BF-209 — Reuse dependency graphs.** `install_order_with_stats` returns the graph and order together; the PubGrub solver adapter and CLI depgraph now consume that single result instead of rebuilding the graph. Existing graph/solver/CLI tests pass, and `graph_scaling` confirms one graph build per measured pass; no speedup is inferred without a matched before/after run.
- [x] **BF-210 — Remove graph string-key churn.** Graph ordering now caches one lexical key per node, uses typed `(PortagePackage, Version)` indices for edge/repair lookup, deduplicates ordering adjacency, and reuses generation-stamped reachability marks. The existing soft-cycle/OR `graph_scaling` fixture measures 3.7%/2.1%/1.7% lower order time at 33/65/129 selected nodes; the change is intentionally modest and no large-SCC optimization is inferred.
- [x] **BF-211 — Correct selected virtual-branch expansion.** The provider retains each solver-selected virtual version from the last successful solve, and graph expansion follows that selection (falling back to the supplied full solution or legacy expansion only when needed). OR and solver-decided USE regressions prove independently selected branches no longer create false edges; the pre-fix OR regression fails on `1f72fafc`.
- [x] **BF-212 — Remove avoidable provider post-processing allocations.** Provider construction now detects missing merged/class constraints before materializing OR alternatives or partitioning; complete repositories take the no-drop path, while missing-branch behavior and sibling alternatives remain intact. The synthetic provider benchmark measures 1.8–20.6% lower complete-repository construction time and 15.9–28.7% lower missing-branch time at 128/512/2,048 packages; the regression covers complete OR groups alongside the existing missing-sibling case.

**Phase 2 gate:** closed (2026-09-25). Trim/provider benchmarks improved without plan-set, USE, slot, or cycle regressions; existing resolver parity tests remain byte-identical. **Current batch verification:** 2,202 nextest tests and the plain workspace test suite pass; doctests, clippy, rustdoc, fmt, docs-check, MSRV 1.95 verification, and benchmark smoke checks pass.

## Phase 3 — repository loading and sourcing

- [x] **BF-301 — Reuse one shell per source worker.** `source_parallel_join()` now creates one shell per worker and reuses the existing hermetic baseline reset for each ebuild; `source_single()` still creates a fresh shell. The synthetic `source_reuse` benchmark measures 64.6–74.7% lower sourcing time across 32/128/512 ebuilds at one or four workers, with a two-ebuild hermeticity regression.
- [x] **BF-302 — Resolve distdir writability once per shell/context.** `EbuildShell` now caches the resolved primary/read-only distdir pair and invalidates it when `set_distdir()` changes the shell configuration. The BF-301 source benchmark measures a further 5.3–6.6% reduction at 32/128/512 ebuilds for one or four workers.
- [x] **BF-303 — Share eclass digest/path memo during live sourcing.** Live-sourced entries reuse eclass digests by resolved path and seed the name memo used by cache validation. The fresh-repository `eclass_memo` benchmark is neutral/noisy (+2.5%, -2.4%, +0.2% at 32/128/512 ebuilds), so no end-to-end speedup is claimed; full `portage-repo` tests, clippy, rustdoc, and benchmark smoke checks pass.
- [x] **BF-304 — Avoid repeated ebuild reads.** `source_ebuild` parses the single content read used for EAPI detection, `SourcedEbuild` carries its MD5 into cache construction, and `repo_entries` passes its pre-read bytes into live sourcing. The lock-matched `regen_reads` benchmark measures 6.4%/4.8%/3.6% lower synthetic medians at 32/128/512 ebuilds versus `f898dd99`; this is not a real-tree claim. A BASH_SOURCE/digest regression, full workspace nextest/doctests, clippy, rustdoc, workspace target check, and benchmark smoke check pass.
- [x] **BF-305 — Stream metadata serialization.** `CacheEntry::serialize()` now writes fields directly into its final buffer, including dependency, phase, and eclass formatting, with an exact byte-format regression. The lock-matched `metadata_serialize` benchmark measures a 50.4% lower median (4.3334 µs to 2.1491 µs) versus `774ebef5`; this is a synthetic microbenchmark, not a real-tree claim. Full metadata tests, workspace nextest/doctests, clippy, rustdoc, and benchmark smoke checks pass.
- [x] **BF-306 — Avoid redundant staging directory creation.** `regen_cache` now reuses one shared prepared writer after `stage_dir_target` creates the category directories, and its `put_prepared` path skips per-entry `create_dir_all`. The lock-matched `regen_reads` benchmark versus `b9eb8b11` is mixed (+4.1%, -4.6%, -1.2% at 32/128/512 ebuilds), so no end-to-end speedup is claimed; full workspace nextest/doctests, clippy, rustdoc, and benchmark smoke checks pass.
- [x] **BF-307 — Move cache discovery into blocking work.** `cache_entries_parallel` and secondary-cache reads now run jwalk discovery in `spawn_blocking`, share the normalized worker budget with phase two, and move descriptor chunks instead of cloning them. The real-tree `repository/load/repo_entries` benchmark (33,121 entries) measures a 14.3% lower median (787.35 ms to 674.80 ms) versus `6b76d606`; an async discovery/dedup regression covers the new path, and full workspace nextest/doctests, clippy, rustdoc, and benchmark smoke checks pass.
- [x] **BF-308 — Bound and share worker budgets.** Automatic `SourceOpts` and `CacheReadOpts` workers now use one conservative cap of 16, and `repo_entries` shares that budget across primary/secondary cache reads. The real-tree `cache_workers` benchmark records 160.29 ms baseline vs 197.58 ms current at the default (bounded-concurrency tradeoff), while `repository/load/repo_entries` is 663.10 ms vs 672.48 ms (+1.4%); explicit worker-count results and full gates are recorded, with no speedup claim.
- [x] **BF-309 — Validate `jobs = 0`.** Source and cache worker counts now normalize values below one to one; regressions cover a zero-worker regen publishing a real entry and a zero-worker cache read returning every descriptor instead of dividing by zero or silently producing no work.
- [x] **BF-310 — Reconcile gap-index lifecycle.** The gap fast path no longer duplicates CPVs already served by the primary cache, rewrites the sidecar when coverage changes, and successful repository-target regen records newly covered CPVs and reconciles the sidecar. Failed regen no longer swaps its staging directory, with regressions for duplicate suppression, successful reconciliation, and no partial publication.

**Phase 3 gate:** passed 2026-09-25 — cache-less/overlay and regen benchmarks complete correctly, worker budgets remain bounded, and failed regeneration publishes no partial cache. The recorded batch passed 377 `portage-repo` tests, 2,210 workspace nextest tests, doctests, fmt, clippy, rustdoc, and benchmark smoke.

## Phase 4 — merge, VDB, activity, and binpkg paths

- [x] **BF-401 — Build a merge-batch VDB ownership index.** `OwnershipIndex` scans each VDB once, stores compact path/owner IDs, updates across register/unregister, and carries a metadata stamp; `Vdb::find_collisions()` remains the one-shot compatibility path. `MergeGate` shares the snapshot per root for plans of at least 16 entries, serializes scheduled unmerges with invalidation, rejects merge-lock acquisition failure, and keeps short plans and privilege-worker children on the streaming path. Normal external register/unregister changes invalidate after `.merge.lock`; in-place `CONTENTS` edits require a new batch. Synthetic 128/256/512-package batches measure 88.2%/94.9%/97.3% lower time than the exact `0feec1ea` repeated-scan baseline; the real 727-package VDB build is 3.343 s and no end-to-end speedup is claimed. The live `test-prefix-sandbox.sh` also passed setup, zlib build/install, VDB registration, and active set/env/list checks. Full verification passed: 2,214 nextest tests, doctests, fmt, clippy, rustdoc, benchmark smoke, and docs-check.
- [x] **BF-402 — Reuse preserve-libs state through a merge.** `MergeGate` now retains one registry and incrementally updated `LinkGraph` per VDB root for indexed batches; each replacement removes the old package's records and adds the replacement's, while reclaim/store runs once at batch end under `.merge.lock`. Scheduled unmerges flush and invalidate the state, and short plans/privilege-worker children keep the one-shot path. Added graph-update and deferred-persistence regressions. The matched synthetic comparison versus exact `dc57ead3` measures 92.2%/93.6%/93.7% lower state-workload time at 32/64/128 packages; no end-to-end merge claim is made. Full verification passed: 2,216 nextest tests, doctests, fmt, clippy, rustdoc, benchmark smoke, docs-check, and the live prefix sandbox smoke test.
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
