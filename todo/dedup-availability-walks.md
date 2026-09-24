# Dedup the availability walks — consolidation plan

Follow-up to the `dev-perl/Digest-HMAC` duplicate-plan-entry incident
(`todo/root-topology-refactor.md`, "Pre-flight dependency check failure",
fixed in `5989eb1`). That fix made `host_copies::compute` seed itself from the
solver's own `MergeRoot::Host` output instead of re-deriving it — a correct
patch, but it left the underlying triplication in place. This file is the
design proposal for reducing it. Step 1's shared BROOT/prefix snapshot landed
as BF-205; broader resolver/closure/preflight snapshot sharing remains BF-404.

## Why

Three places each implement a close variant of "walk a package's
DEPEND/BDEPEND/(IDEPEND) edges, track a growing per-root availability set
seeded from a VDB, decide what's missing":

- `bdepend_avail::Avail` and its consumers (`preflight::check`, the two
  post-solve trims);
- `host_copies::compute` (the native-offset host build-copy walk);
- the solver's own dual-root `(package, merge_root)` expansion in
  `portage-atom-pubgrub`.

The incident was exactly the failure mode this invites: two of the three
(host_copies and the solver) fired for the same crossdev host-arch scenario,
each with its own availability view and its own version picks, and neither
knew about the other. Reading all three for this plan also turned up a
**fourth, hidden copy**: the VDB *seeding* (BROOT VDB + `--prefix` weave) is
implemented twice — `Avail::initial_bdepend` and
`installed::load_host_installed` — and the same bug (#28/#30, "read the given
BROOT, not the bare host") was found and fixed **separately in each**. That
drift-by-duplication is the cheapest, most valuable thing to close.

## The implementations, precisely

### A. `bdepend_avail::Avail` — the checking primitive

`portage-cli/src/bdepend_avail.rs`. Data model: `Avail` is a flat
`Vec<AvailEntry { cpv, slot, use_info }>` (`bdepend_avail.rs:16-37`), where
`use_info` is `Some` only for VDB-backed entries so `atom_satisfied` can
verify simple `[flag]`/`[-flag]` USE-dep brackets; `[flag?]`/`[flag=]` forms
are conservatively satisfied (`bdepend_avail.rs:164-199`).

- **Seeding**: `initial_bdepend` = BROOT VDB
  (`roots.satisfaction_root(DepClass::Bdepend)`) plus the prefix VDB under
  `--prefix` (`bdepend_avail.rs:60-66`); `initial_depend` =
  `VDB(base) ∪ VDB(target)` (`:69-75`); `initial_sysroot_depend` (`:79-81`);
  `from_cpvs` for explicit sets.
- **Growth**: `record_provided` / `record_merge_bdepend` /
  `record_target_merge` / `record_merge` (`:101-139`) — within-run plan
  merges, slot/USE unknown by design (the solver already validated those).
- **Queries**: `atom_satisfied` (`Dep::matches_cpv` + USE brackets, `:141`),
  `collect_unsatisfied` (`:266`), `unsatisfied_cpns` (`:231`),
  `has_unsatisfied_atom_for_cpn` (`:149`).

Consumers (all read-only checks; none picks versions):

1. **`preflight::check`** (`portage-cli/src/preflight.rs:45-104`) —
   *validates* an already-decided plan: one sequential pass, two `Avail`s
   (DEPEND view + BDEPEND view), each grown as entries are scanned; output is
   `Ok(())` or an error naming every unsatisfied atom. It is the guard rail
   that caught the incident. Called from `emerge.rs:297-299` on real (non
   `-p`) runs only.
2. **`bdepend_trim::trim_within_run_bdepend`**
   (`portage-cli/src/query/depgraph/bdepend_trim.rs:41-72`) — *removes* plan
   entries whose only reason to exist is a BDEPEND already satisfied on BROOT
   or by an earlier kept entry. Rebuilds `Avail::initial_bdepend` from
   scratch per consumer scan (`avail_for_consumer`, `:149-163`) — O(n·m)
   VDB-entry copying, a known inefficiency but not a correctness issue.
3. **`depend_trim::trim_sysroot_satisfied_depend`**
   (`portage-cli/src/query/depgraph/depend_trim.rs:22-54`) — same shape
   against the sysroot VDB for DEPEND.

### B. `host_copies::compute` — the deciding walk

`portage-cli/src/query/depgraph/host_copies.rs`. Job: for a **native offset**
(`cross.active && !cross.is_cross_arch()`, guard at `:78`) decide *what to
add* — `MergeRoot::Host` build copies of target packages' build edges the
host lacks — spliced onto the plan front at `mod.rs:821-824`.

Since `5989eb1` it already reuses A for availability tracking:
`Avail::initial_bdepend(roots)` baseline (`:99`), seeded with the solver's
own `@host` entries from `target_order` (`:105-112`), gaps found via
`unsatisfied_cpns` (`:151`). What remains bespoke:

- `Ctx`/`Walk` structs (`:53-63`) — trivial, fine.
- The recursive deps-first walk `visit_unsatisfied` (`:128-164`): per node,
  `repo::find_cache` + `effective_use::effective_use` +
  `DepEntry::evaluate_use` over `[depend, bdepend, idepend]` (`:135-149`).
  This *walk shape* is the genuine job (ordering matters: each copy is
  appended only after its own edges), but the cache/effective-USE/evaluate
  triple is copied in four other places (see Step 2).
- `resolve` (`:170-186`): Target-plan version reuse, else newest
  `Adapter::version_accepted` repo version + slotted/unslotted derivation —
  a hand-rolled subset of what `repo.rs`'s `versions_for`/`slots_for` already
  do with the same filter (`repo.rs:533`, `:684`, `:708`).

Genuine difference from A: it makes choices (versions, insertion order), not
just satisfaction checks. Genuine difference from C: it is a plain closure
walk over the md5-cache metadata — no version-set constraints, no
backtracking, no USE-dep co-solving.

### C. The solver's dual-root expansion — the real resolution

`portage-atom-pubgrub`. Trigger chain: `CrossContext::active`
(`portage-cli/src/query/depgraph/root_aware.rs:71-111` — any of dual-root,
cross-arch, or offset build) → `provider.set_cross_active(cross.active)`
(`depgraph/mod.rs:402`) → `ensure_host_instances`
(`provider/mod.rs:762-773`) aliases every Target package key to a Host key
sharing the *same* `PackageData` (`host_aliases`, `provider/mod.rs:227`,
lookups via `package_data_key` `:775-780`).

- **Availability**: the `host_installed` map (`provider/mod.rs:219`), fed by
  the caller at `depgraph/mod.rs:456-463` from
  `installed::load_host_installed` (`installed.rs:90-97`) — which implements
  the **same BROOT + prefix weave** as `Avail::initial_bdepend`,
  independently (the fourth copy). Note a real semantic divergence: the
  weave here is last-wins (`HashMap::insert`, prefix overrides host per
  package), while `Avail`'s is a union (`Vec` + `any()` match — either entry
  satisfies). Documented as intentional on the solver side
  (`installed.rs:84-89`); `Avail`'s union bias is the permissive-guard-rail
  choice. Any shared loader must preserve both consumers' semantics.
- **The `@host` decision**: made per-edge during dependency expansion, not as
  a separate pass. For a *built* Target package under `cross_active`,
  `compute_dependencies` (`solve.rs:341-355`) calls
  `cross_target_runtime_deps` (`solve.rs:402-434`): DEPEND/RDEPEND/PDEPEND
  are stamped `MergeRoot::Target` (`:414-421`), and each BDEPEND/IDEPEND edge
  **not satisfied on the host** is stamped `MergeRoot::Host`
  (`append_unsatisfied_broot`, `:526-538`). A Host node being built expands
  via `host_native_deps` (`:437-451`, gated on `with_bdeps` at `:356`),
  stamping its whole closure `@host` — this is what emitted the incident's
  Block B.
- **Satisfaction primitive**: `host_satisfied_on_broot` (`solve.rs:489-507`)
  + `host_use_dep_satisfied` (`:513-524`) — version-set containment over
  converted `convert::Req` edges plus full USE-dep evaluation *including*
  parent-conditional forms (`[flag?]`/`[flag=]`), via the parent's `desired`
  UseConfig and the shared `eval_violated_use_dep`. Strictly more precise
  than `Avail::atom_satisfied`, and operating on entirely different types
  (interned `Req`/`PortageVersionSet` vs `portage_atom::Dep`/`DepEntry`).

**"Did this solve introduce Host copies for CPN X?"** — there is no separate
provider record; the `@host` decision only exists as the `MergeRoot::Host`
identity baked into the solution's `PortagePackage` keys (`package.rs:28`,
`:111-114`). So scanning `target_order` for `MergeRoot::Host`, the way the
`5989eb1` fix does, is not a post-hoc heuristic — it *is* reading the
solver's decision. And it is actually **more correct** than any provider-side
API would be: `bdepend_trim` runs between the solve and `host_copies`
(`mod.rs:736` before `:821`) and can legitimately drop solver-emitted `@host`
entries, so a pre-trim "the solver scheduled X@host" record would overstate
what the plan still contains. Verdict: no new provider API needed for this;
the seed-from-`order` approach is the right primitive and should stay.

## Verdicts per pair

**A ↔ B: keep separate walks, share the remaining helpers.** Post-`5989eb1`
they already share the availability model, the seeding, and the
gap-detection query. The validator (single pass, no choices) and the
generator (recursive, picks versions) are genuinely different jobs; forcing
one "walk framework" over both would add indirection to ~60-line functions.
What's left is small, mechanical duplication: the
`find_cache`+`effective_use`+`evaluate_use` triple (5 sites) and the
"newest accepted version + slot derivation" pick (Steps 2-3).

**B ↔ C: cannot merge today; convert the overlap into a checked invariant.**
Eliminating `host_copies.rs` means the solver scheduling native-offset
DEPEND host-copies itself — the exact thing that was tried and reverted:
routing unsatisfied DEPEND `@host` inside `get_dependencies` ballooned the
Target solve (curl 12 → ~120 packages) because `host_aliases` shares
`PackageData` between the two roots (`todo/nonemptytree-bdeps-gap.md:41-48`).
The real fix is independent per-root `PackageData` (true dual-root per
`root-model.md`) — a structural `portage-atom-pubgrub` redesign with its own
perf/parity bar. Out of scope here (Step 5). What *is* actionable: under
`cross.active`, the solver already covers the BDEPEND/IDEPEND portion of B's
job (`append_unsatisfied_broot` fires for every built Target package), so
B's top-level scan finding a BDEPEND/IDEPEND gap should be rare — either the
two availability views disagree (a drift bug worth knowing about) or the
trim dropped something. Make that overlap observable before narrowing it
(Step 4). B's DEPEND coverage is irreducible: the solver stamps Target
DEPEND onto the Target root only (`solve.rs:414-421`), never `@host`.

**A ↔ C: do not unify the satisfaction check; do unify the seeding.** The
two satisfaction primitives live in different type worlds
(`Dep`/`DepEntry`/string USE vs interned `Req`/`PortageVersionSet`/
`UseConfig`), across a crate boundary the solver deliberately keeps
VDB-agnostic (it is *fed* `host_installed`, it never reads a VDB), and their
USE-dep semantics differ on purpose (A is conservative on parent-conditional
forms it has no context for; C evaluates them exactly). A shared "is this
atom satisfied" would need a common representation neither side wants.
But the *inputs* — "what is installed at BROOT, with the prefix weave" —
should come from one loader (Step 1); that is where the same bug was fixed
twice.

## Update 2026-07-11: the performance motivation is now handled separately

A user-flagged performance regression (`em -p www-client/firefox` ~1.73× →
~2.1s, only ~1.7× faster than `emerge -p`) traced back to eager USE/IUSE
reads across exactly the duplicated scans this file describes (`Avail::
initial_bdepend`/`initial_depend`, `load_target_installed`/
`load_host_installed`). Fixed *without* doing Step 1's structural loader
unification: `portage-vdb/src/field_cache.rs` adds a process-wide,
path-keyed cache underneath `InstalledPackage::read_field`, so redundant
reads across independent scans of the same VDB root become free regardless
of whether the loaders themselves are unified. `em -p firefox` is back to
~1.15s (3.17× faster than emerge). Full writeup:
`todo/root-topology-refactor.md`'s "Performance regression" entry
(2026-07-11).

This resolves Step 1's *performance* motivation. Step 1's other motivation —
eliminating "the same bug fixed twice" drift risk from having the BROOT+
prefix weave logic implemented independently in two places — is unaffected
and still worth doing; the cache is a transparent perf layer underneath both
copies, not a replacement for unifying them.

## Staged plan

Each step is independently landable. Baseline verification for every step:
`cargo fmt --check`, `cargo clippy -D warnings`,
`cargo test --workspace --exclude portage-bench`; live: in the
`crossdev-stages` sandbox (aarch64-20260618T101350Z), rerun
`em --prefix /opt/xp --target riscv64-unknown-linux-gnu crossdev --setup`
as `-p` (each package listed exactly once, dependency order, solver's
versions: `git-2.53.0` not `-9999`, `Crypt-URandom-0.540.0`) and then real
(preflight passes, first build starts). Release binary, `pgrep em` first.

### Step 1 — one BROOT-availability loader (portage-cli only)

- **What changes**: extract a single "installed at BROOT, prefix-woven"
  enumeration — e.g. `installed::broot_vdb_entries(roots) ->
  Vec<BrootEntry { cpv, slot, use, iuse }>` — and make both
  `Avail::initial_bdepend` (`bdepend_avail.rs:60-66` +
  `vdb_avail_entries`, `:205-224`) and `load_host_installed`
  (`installed.rs:90-131`) consume it, each converting to its own entry type
  and keeping its own merge semantics (union for `Avail`, last-wins insert
  for `add_host_installed` — the loader returns host entries before prefix
  entries so both behaviours fall out of iteration order, as today).
- **What gets deleted**: the duplicated weave logic and one of the two raw
  VDB readers (`vdb_avail_entries` and `load_host_installed_at` collapse to
  one reader + two thin adapters).
- **Risk**: low. Pure portage-cli refactor; both call sites have regression
  tests written against the exact bugs this class of drift produced
  (`initial_bdepend_reads_the_given_root_not_the_bare_host`,
  `initial_bdepend_weaves_in_the_prefix_vdb_under_overlay` and the
  `load_host_installed_*` trio in `installed.rs:222-330`). All must pass
  unchanged against the shared loader. Watch the interning boundary: the
  solver side wants `Interned` flags, `Avail` wants strings — convert at the
  adapter, don't force interning into `Avail` (or do, but then measure; see
  the package-star-interning session before changing hot-path types).
- **Verify**: baseline suite + live `-p` diff (byte-identical plan expected).

### Step 2 — one "evaluated deps for (pkg, ver)" helper in depgraph

- **What changes**: a small helper (natural home: `effective_use.rs` or
  `repo.rs`), roughly `evaluated_deps(data, use_config, package_use, pkg,
  ver) -> Option<EvaluatedDeps>` wrapping `find_cache` + `effective_use` +
  `DepEntry::evaluate_use` per class, replacing the copies at
  `host_copies.rs:135-150`, `bdepend_trim.rs:77-91` and `:130-140`,
  `depend_trim.rs:98-119` (which evaluates four classes on the same
  cache/effective pair).
- **What gets deleted**: five inline copies of the triple; `TrimCtx` gains
  nothing new (it already carries all inputs).
- **Risk**: minimal, mechanical. No behaviour change intended — evaluate
  laziness carefully in `depend_trim::should_keep` (it evaluates four
  classes but only conditionally uses three; keep that shape or accept the
  negligible cost, but say which in the commit).
- **Verify**: baseline suite; the trims' regression tests
  (`already_installed_package_excluded_from_order_still_pins_its_rdepend`,
  `no_op_when_with_bdeps_off`, `no_op_when_sysroot_equals_target`) pin the
  observable behaviour.

### Step 3 — shared "newest accepted version" pick on `Adapter`

- **What changes**: `Adapter::newest_accepted(&self, cpn) ->
  Option<(&Cpv, &CacheEntry)>` (filter `version_accepted`, max by version —
  the pattern at `repo.rs:708` and `host_copies.rs:174-178`), plus a tiny
  "package identity from cache slot" helper for the slotted/unslotted
  derivation duplicated at `host_copies.rs:179-184` and the four
  `match slot ... slotted/unslotted` sites in `mod.rs` (`:413-416`,
  `:421-424`, `:446-449`, `:793-796`). `host_copies::resolve` shrinks to
  target-plan reuse + one call.
- **What gets deleted**: `resolve`'s inline filter/max/slot block; the
  `mod.rs` sites can migrate opportunistically (they build from VDB slots,
  not cache entries — only fold them in if the helper fits without
  contortion; don't force it).
- **Risk**: low. The `5989eb1` incident already proved what happens when
  this pick drifts from `version_accepted` (the `git-9999` selection);
  centralizing it prevents the next copy from repeating that.
- **Verify**: baseline suite + the live crossdev `-p` check (versions must
  stay the solver's). Add a unit test for `newest_accepted` masking/keyword
  behaviour if `repo.rs` doesn't already cover it via `versions_for`.

### Step 4 — make the B↔C overlap observable, then (maybe) narrow it

- **4a, landed.** `host_copies::visit_unsatisfied` now takes a `top_level`
  flag (`true` only for the direct per-Target-package calls from `compute`,
  `false` for recursion into a copy's own edges) and prints an `eprintln!`
  (no logging framework exists in this codebase — matches the plain
  `eprintln!`-for-anomalies convention already used in `overlay.rs`/
  `mod.rs`) whenever a *top-level* BDEPEND/IDEPEND gap is found, naming the
  class, the CPN, and the consumer. Added
  `does_not_duplicate_a_solver_seeded_host_entry`, a unit test pinning the
  `5989eb1` seeding behaviour (a `target_order` with a solver-emitted
  `@host` entry must yield zero copies for that CPN) — that fix previously
  had no test of its own, verified live only.
- **4b: decided, based on real evidence — do NOT narrow, and don't expect
  to ever revisit this.** The live crossdev sandbox rerun
  (`em -p --prefix /opt/xp --target riscv64-unknown-linux-gnu crossdev
  --setup`) fired the new warning three times, not zero:
  ```
  !!! host_copies: top-level BDEPEND gap for dev-vcs/git (from cross-riscv64-unknown-linux-gnu/binutils:9999) — ...
  !!! host_copies: top-level BDEPEND gap for dev-vcs/git (from cross-riscv64-unknown-linux-gnu/gcc:17) — ...
  !!! host_copies: top-level BDEPEND gap for dev-vcs/git (from cross-riscv64-unknown-linux-gnu/gcc:17) — ...
  ```
  `dev-vcs/git` (the live `9999` ebuilds' own git-r3-eclass BDEPEND) is a
  real gap the solver's own dual-root `append_unsatisfied_broot` expansion
  does not cover for these two cross-category-aliased packages — only
  `host_copies`'s own top-level scan finds and correctly schedules it
  (`git-2.53.0` still appears exactly once, correctly ordered, in the
  live-verified plan). This is exactly the outcome the plan's own risk
  note anticipated ("if the counter fires in practice, 4b is wrong and B's
  re-scan is load-bearing as a drift catcher") — confirmed, not
  hypothetical. **B's top-level BDEPEND/IDEPEND re-scan stays permanently;
  it is not redundant with C, it is catching something C misses for
  cross-category aliased live ebuilds.** Not investigated further *why*
  the solver's dual-root expansion misses this specific case (a
  reasonable next question — possibly something about how
  `Location::Alias`-derived cross-category metadata reaches the two
  `PackageData` views differently — but out of scope for this dedup pass;
  it doesn't change the "keep B's full scan" verdict either way).
- **What gets deleted**: nothing — 4b (narrowing the top-level scan to
  DEPEND-only) is not happening, so `host_copies.rs`'s class loop stays as
  written.
- **Verify, done**: the new unit test passes; `cargo fmt --check`/
  `clippy -D warnings`/`cargo test --workspace --exclude portage-bench` all
  clean; live crossdev `-p` rerun (above) — plan unchanged from Step 3,
  now with the diagnostic confirming the gap it's filling is genuine.
  (The plain native-offset `net-misc/curl` parity check from the original
  plan wasn't run — the crossdev case already gave a real, load-bearing
  answer to the only open question 4b was gated on.)

### 2026-07-16: `--local` was engaging this machinery too, and shouldn't have

Testing `em --local /root/local-test toolchain --setup -p` against a
completely fresh, empty `--local` prefix fired the Step 4a `eprintln!`
warning **dozens of times** (`virtual/pkgconfig`, `app-arch/unzip`, a very
deep `dev-perl/*` module chain, `dev-python/jinja2`, `dev-build/meson`, …) —
far beyond anything Step 4b anticipated (3 hits, for a real cross build).
The final plan for one step contained the *same* package listed twice,
non-adjacently (`dev-perl/Capture-Tiny`, `Config-AutoConf`,
`Class-Inspector`, `List-MoreUtils`, `Params-Util`, `File-ShareDir-Install`
all doubled), and the real (non-pretend) run failed `preflight::check` for
~20 packages needing `sys-devel/gettext`/`sys-devel/m4`/`dev-libs/gmp`/
`dev-build/meson` that were in the plan but not correctly ordered before
their consumers.

Root cause (confirmed with Fable's review, `todo/PENDING.md` links the
session): `CrossContext::detect()` (`portage-resolve/src/root_aware.rs`)
computed its `active`/dual-root flag from `offset_build = target != "/"` —
true for `--root`/`--prefix`/cross **and** `--local` alike, since all four
have a non-`/` target. But `--local`'s BROOT is the *same* prefix as the
target (`Cli::base_roots()`'s `--local` branch sets base == target == broot
== the one prefix path) — structurally identical to the bare invocation
(broot == target == `/`), just at a different path. `host_copies`'s Tier-1
walk (designed for an already-populated real host, missing only a handful
of build tools) ran against `--local`'s own, initially-*empty* BROOT VDB,
found *everything* "missing," and inserted a parallel `@Host` copy of
nearly the whole closure interleaved with the regular `@Target` one — the
duplicate-entries and preflight-ordering-confusion evidence above.

**Fixed**: `detect()` now computes dual-root activation from
`broot_differs = roots.broot().is_some_and(|b| b != target)` instead of
`offset_build`. Verified per topology: bare (`/ == /`, inactive, unchanged),
`--local` (prefix == prefix, **now inactive**, the fix), `--root`/
`--prefix`/`--target` (broot genuinely differs, active, unchanged — spot-
checked live post-fix against already-built sandboxes, no regression).
Companion fix: `detect()`'s inactive return used to hardcode
`sysroot`/`target` to `/`, which would have silently broken `--local`'s own
`-p` ` to <prefix>/` display annotations once it stopped being "active" —
now populates them truthfully regardless of `active`, and `display_root`'s
Target arm no longer special-cases `active` at all (it already suppressed
the `to .../` suffix whenever the resolved path is literally `/`, so bare
stays visually unchanged). New tests: `local_shaped_roots_are_not_active_
but_still_report_the_real_target`, `bare_invocation_is_not_active`;
`host_entry_displays_as_landing_on_the_real_host_under_offset` retargeted
from `Roots::for_test` (actually `--local`-shaped) to
`for_test_root_with_broot` (genuinely `--root`-shaped) for honesty.

This fully closes the duplicate-entries/spurious-warnings problem for
`--local`. It does **not** close a second, separate issue the fix exposed
underneath: bootstrapping `--local`'s *entire* BDEPEND closure still hits
real ordering gaps in the native path itself (`elt-patches`/`gettext`/
`meson`/`python`/`xz-utils`/`rsync`/`glibc[cet]` needed before consumers
that aren't getting them). This is not about the target being empty —
`--root`'s and `--prefix`'s own toolchain bootstraps already work into an
equally-empty target, because both borrow an already-populated real host
as BROOT. `--local`'s BROOT has nothing to borrow from (its own prefix,
standalone) — that's the actual distinguishing fact, not "freshness."
A from-scratch bootstrap-ordering problem, not a dual-root one, and out
of scope for this fix. Tracked as a new open item; see `todo/
select-toolchain.md`/`todo/PENDING.md`.

**Follow-up, fixed** (`97e5f1b`): the from-scratch ordering gap traced to a
real bug in `install_order`'s SCC tie-break (`portage-atom-pubgrub/src/
graph.rs`'s `order_cycle`), Fable-reviewed. One Tarjan pass over combined
hard+soft edges can't distinguish "genuinely in an irreducible hard cycle"
from "merely pulled into a bigger SCC by an unrelated RDEPEND cycle
elsewhere" — an incidental RDEPEND cycle among core system packages folded
114 of 229 packages into one SCC, and the flat `indeg_hard`/`indeg_all`
tie-break inside it is a *preference*, not a *gate*: `sys-libs/gdbm` was
emitted before its own direct BDEPEND `app-portage/elt-patches` purely
because both had `indeg_hard == 1` and the tie-break's other term favored
`gdbm`. Fixed by running a local hard-only Tarjan restricted to each
component's members to find the real hard sub-groups, then gating the
existing heuristic so a hard predecessor outside a member's own hard-group
is always respected (it's provably not part of any real cycle). Verified
live: `gdbm` now correctly orders after `elt-patches`/`xz-utils`.

This exposed one further, larger genuine hard cycle beyond the known
`elt-patches`<->`xz-utils` 2-cycle: an 11-node bootstrap cycle among
`sys-devel/gcc` (BDEPENDs `texinfo` for docs) -> `sys-libs/glibc` ->
`dev-libs/libgcrypt` -> `dev-libs/libxslt` -> `app-text/po4a` ->
`sys-apps/util-linux` -> `dev-lang/python` (needs util-linux's libuuid) ->
`dev-build/meson` -> `dev-libs/libxml2` -> `sys-devel/gettext` ->
`sys-apps/texinfo` (closes the loop via its own BDEPEND on gettext).
Confirmed against the real `::gentoo` ebuilds — every edge is real. A true
cycle has no valid total order by definition; `order_cycle`'s heuristic
picks the best available tie-break but some edge in a real cycle is
unavoidably violated, same as the existing `elt-patches`/`xz-utils` case.
This is *not* an `em` bug — it's the actual reason real Gentoo bootstraps
from staged tarballs (a pre-existing toolchain/python/meson already present)
rather than from absolute zero; `--local`'s from-scratch attempt hits this
real constraint directly. The remaining `preflight::check` failures seen in
this session's `--local` testing (`pax-utils`/`libxml2`/`zstd`/`pam` all
needing `meson`; `gcc` needing `glibc[cet]`; `glibc` needing `python`;
`attr`/`texinfo` needing `gettext`; `xz-utils` needing `elt-patches`) are
all internal-to-this-cycle violations, not distinct bugs.

### Step 5 — out of scope: true dual-root solve

Deleting `host_copies.rs` entirely requires the solver to schedule
native-offset DEPEND host-copies itself, which requires per-root
`PackageData` (independent constraint spaces for `pkg@Host` vs
`pkg@Target`) instead of `host_aliases` sharing one. That is a
`portage-atom-pubgrub` redesign: it re-opens the Tier-1 blowup
(`nonemptytree-bdeps-gap.md:41-48`), touches `ensure_host_instances`/
`package_data_key` and every aliasing-bug guard around them
(`provider/mod.rs:191-201`), and needs its own parity/perf campaign
(emerge counts, solve-time benchmarks). Not hand-waved into this plan;
if/when `root-model.md`'s dual-root scheduling happens, `host_copies.rs`
and the 4a instrumentation are the first things it deletes.

Also explicitly *not* proposed: merging `preflight::check` into anything.
It validates the composed output of A+B+C after all splices and trims — its
independence from the plan-building code is exactly why it caught the
incident. If `install_order` plus the post-passes ever guarantee a valid
deduplicated topological order by construction, preflight becomes a pure
invariant guard; that's a reason to keep it cheap, not to consolidate it.

## Noticed while reading (not part of this plan)

- `solve.rs:356` gates a built Host node's `host_native_deps` expansion on
  `with_bdeps`; with `with_bdeps=false` and `cross_active`, a Host node
  falls through to `broot_filtered` (`:372-377`), whose kept DEPEND edges
  retain their converted (Target-flavored) identity — a Host package's
  build deps possibly stamped onto the Target root. Crossdev `--setup`
  runs with bdeps on, so this path may be unreachable in practice; worth a
  targeted look, separately.
- `bdepend_trim::avail_for_consumer` (`bdepend_trim.rs:149-163`) rebuilds
  the full VDB-seeded `Avail` per consumer scan — O(n·m). Fine today;
  Step 1's shared loader makes a cached seed trivial if it ever shows up
  in a profile.
