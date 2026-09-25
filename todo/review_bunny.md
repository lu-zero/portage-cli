# Second opinions wanted — 2026-09-25 session

Scratch notes for a reviewer (Grok). Nothing here is a claim I've verified; each
item is somewhere I was genuinely unsure and would rather a second pair of eyes
looked than have me grade my own homework. Ordered by how much I want the review.

Commit range: `54a9e67d^..4eea048f` (58 commits). Gates were green throughout —
2,239 nextest, doctests, clippy `--all-targets`, fmt, rustdoc, bench-smoke,
docs-check — but gates are necessary, not sufficient.

## 1. The perf work has no aggregate measurement (highest value)

Fifteen-odd `perf(...)` commits, each paired with a `bench(...)` recording a
micro-benchmark in `benchmarks/BENCHMARKS.md`: source worker shell reuse, eclass
digest memos, exact cache indexing, VDB snapshot sharing, GPKG streaming, and so
on. Every individual bench showed a win on a synthetic fixture.

**I never measured whether they compose.** There is no before/after on a real
`em -p @system` wall-clock across the session. I have been careful not to *claim*
end-to-end wins — `AGENTS.md` forbids it from synthetic benches, and I agree
with that rule — but the honest converse is that I also can't show the session
made anything faster overall. Some of those fifteen could be noise, or could
interact badly, and my methodology wouldn't have caught it.

Concretely: I'd like a before/after on the real tree. I can produce a binary at
`54a9e67d^` and one at `4eea048f` and time `em -p --emptytree @system` against
`/var/db/repos/gentoo`, interleaved, multiple runs. I didn't do it because it
needs a release-ish build of both and that's a long build — but it's the obvious
next measurement and I'd rather someone else decide it matters.

## 1b. BF-405's code change is unmeasured at runtime (much smaller than I first claimed)

**Correction.** I initially wrote that `1a52b6b1` and `0c30acb1` shipped with "no
measurement at all." That was wrong — I grepped only `benchmarks/BENCHMARKS.md`
and `benchmarks/benches/` and inferred absence from that narrow search. Both
findings carry real measurements, recorded in `todo/bunny-findings.md`:

- BF-406 measured a slow-sink 100,000-event burst: producer enqueue time went
  17.98 ms → 6.65 s while still delivering every event, which is the argument
  for the bounded channel. It also adds a blocked-sink regression proving the
  queue applies backpressure *without drops* — which directly answers the
  correctness worry I raised below.
- BF-405 measured 38 real history files (1,886 lines / 1,234,068 bytes) and
  used that to defer BF-904, which is exactly what its title promised.

The genuine, much narrower gap: **BF-405's actual code change** — lazily loading
`DurationStore` only when an orphaned `AfterBlocker` needs the `job_id` set — has
no runtime measurement. The finding says so itself ("No runtime speedup or
allocation claim is made"). The reasoning is sound and the change is on a rare
path, so this is a documented absence rather than an oversight, but it is the
one perf change today whose justification is argument rather than number.

## 2. `depgraph()` refactor — oracle coverage is narrower than it looks

BF-511, six phases, 1,890 → 1,663 lines, all four `capture-plan.sh` cases
byte-identical against a pre-refactor baseline. The oracle is decent (negative
control, byte-stability across repeated runs, a stale-binary guard added after
that gap bit me), but:

- **No case passes `--autounmask-widen`.** I confirmed this directly. So
  `widened_autounmask_candidates`, extracted in phase 3, is verified only by
  compiling and by the *other three* cases being unchanged — never by its own
  output. That phase is the weakest link in the chain.
- **Four scenarios is four scenarios.** Byte-identical output proves the refactor
  didn't perturb those paths. It doesn't prove behavior is preserved for inputs
  the cases never generate. A reviewer who spots a path with no case coverage
  would be more useful here than I am.

Fix for the first bullet is cheap (one line in `CASES`) — I just never did it.

## 3. The u64 version-component reversal

`ba14cf1b` widened parsing to accept components past `u64::MAX`. `c722c1c8`
reverted it. `203ff850` then recorded "reject rather than truncate" as a
*deliberate* documented deviation from PMS 3.2.

A direction reversal inside one day on a parsing-boundary decision is exactly
what deserves a second read. The question: is "reject" actually the principled
landing spot, or did I flip-flop and stop somewhere merely defensible? The
argument for reject-over-truncate is real (a silently shortened component
mis-compares against a package spelling it in full), but I'd want that reasoning
independently checked, and I'd want the deviation to be discoverable where a
reader hits the boundary — not only in a scratch file.

## 4. Two "rare"/"only" claims that carry unverified weight

- `apply_order_filters` (`c890e1e4`) re-appends reinstalls the solver never
  routed through `install_order`, commented `(rare)`. Is it reachable at all? If
  it's dead code that survived three refactors, delete it. If it's live, `(rare)`
  is doing unverified work.
- `cb046ebc` fixed container facts "in one place". Is there genuinely no second
  path that could resolve them differently? That's the same shape of claim I
  believe after any dedup, and I have been wrong about it before.

## 5. `c735f182` binhost index cache-key collision

Security-adjacent: a collision could serve the wrong package. I believe the key
is now injective, but "I believe" is the whole of the confidence here and the
blast radius is a wrong binary. Worth an adversarial read.

## 6. Not started, and I'd like input before writing it

The co-solve fixpoint — the last piece of BF-511. `build_and_solve` captures
~20 run-scoped locals; a free function would be a ~20-arg signature, so it needs
a context struct. I stopped rather than guess the boundary. My instinct is to
bundle only what `build_and_solve` needs and leave per-round state (`final_policy`,
`round_metrics`, graph/edges) outside, but the alternative — one struct for the
whole run — is more uniform. A reviewer's view on which is better would save a
churn cycle.

## What I do *not* think needs review

- The `ebuild/` module split (three commits, one per module, pure code motion).
- BF-512's `Adjacency` → `BTreeMap`: measured over 12 interleaved runs, 792 vs
  782 ms median with identical minimums. Below the noise floor, recorded as "no
  measurable cost" — a determinism win claimed as nothing more.
- The doc/claim sweep (`8d9d9bbe`) — durable-docs reconciliation, no behavior.
- `4d961d22`, the clippy `--all-targets` borrow fix. Mechanical.

## One process note against myself

I nearly committed a false pass. A build failed, the capture still reported "all
four identical" because it ran the *previous* binary, and I only caught it
because a clippy error had scrolled past. `7be564ea` now makes `capture` exit 2
on a stale binary. Worth a reviewer confirming that guard actually fires in the
ways I'd expect — I tested it, but I tested my own test.

---

# Hostile self-review (subagent pool, 2026-09-25)

Six focused adversarial reviews, each told to report only defects it could point
at in code. Every item below I then verified by hand. Three are regressions
introduced by this session's own commits.

## HIGH-1 — `9bc6c785`: preserve-libs state is never invalidated (key mismatch)

`MergeBatchState.preserve` is keyed by **merge root** (`preserve_mut(root, vdb)`
→ `entry(root.to_owned())`, `ebuild/mod.rs:496`), but `take_or_build` calls
`finish_preserve` with `vdb.root()` (`mod.rs:476,482`).

`vdb_root_for` (`mod.rs:3017-3023`) returns `<root>/var/db/pkg`, so for EROOT `/`
the insert key is `/` and the removal key is `/var/db/pkg`. They can never match;
`HashMap::remove` always misses, so the stamp guard the doc comment advertises is
a **no-op for the preserve half**. The ownership half is unaffected — it really is
keyed by `vdb.root()` (`mod.rs:513`).

Consequence: a stale `MergePreserveState` (pre-invalidation `LinkGraph` +
in-memory `PreservedLibsRegistry`) survives and is handed to the next merge.

## HIGH-2 — WITHDRAWN: the batch-boundary lock is the same inode qmerge's

**This finding was wrong, and I published it as fact.** It claimed
`finish`/`with_invalidation` locked `work_base/.merge.lock` while qmerge locked
`work_base/<root-key>/.merge.lock`, so the two had "zero mutual exclusion" and two
concurrent `em` runs could have one `prune_unneeded` unlink a library the other
needed.

`lock_merge_flock` (`flock.rs:57-61`) pops three parents off
`$work_base/<root-key>/<category>/<pf>` — four components — which lands on
`work_base`, not on `<root-key>`. Two parents would be needed for `<root-key>`.
Verified by running the real `lock_merge_flock` against a real
`package_work_dir`:

```
work_base = /tmp/.tmpo7NKss/work
work_dir  = /tmp/.tmpo7NKss/work/host/dev-libs/zlib-1.3.1
3 parents = /tmp/.tmpo7NKss/work          <- what lock_merge_flock uses
2 parents = /tmp/.tmpo7NKss/work/host
```

So the batch flush already took the same lock qmerge does, and there was no bug.
`portage-cli/src/ebuild/mod.rs` now carries a regression test asserting the two
resolve to the same file, so the off-by-one cannot be reintroduced by the next
reader who counts parents wrong.

What survives from the original report: `acquire_flock` returning `None` was
swallowed by `let _merge_lock = ...`, so a failed acquire silently proceeded
*unlocked* into `prune_unneeded` (which unlinks files) and `store` (which
rewrites a shared registry). That is now an error, and the flush is skipped
rather than run without the lock. That part was right.

Lesson: the subagent reported the parent count and I copied it into a findings
file without running the function. Two commits' worth of my own review notes
downstream of it — including the decision to hold back a commit — rested on
arithmetic I never executed. The arithmetic was one line long.

## HIGH-3 — `UseDep` implements `Ord` that contradicts its own `Eq`, and the doc says it doesn't

`use_dep.rs:83` derives `PartialEq`/`Eq`/`Hash` over all three fields, but
`use_dep.rs:160-164` implements `Ord::cmp` as `self.flag.cmp(&other.flag)` — flag
only. So `cmp(a,b) == Equal` while `a != b`: a direct `Ord` law violation.

`9f836561` (this session) added a doc comment at `use_dep.rs:76-80` stating
"Deliberately `PartialEq`/`Eq` but **not** `Ord`", and its commit message repeats
it. The impl dates to `c32e372b` and was never removed. So this session wrote a
durable false claim into a doc comment — the exact pattern the Slop Warning
exists to stop.

`UseDepBuilder` is public (`lib.rs:76`) and the fields are `pub`, so this is one
`BTreeSet` away from silent data loss. Zero workspace callers of `UseDep`
ordering today, so it is latent rather than live. Fix: delete the `Ord`/
`PartialOrd` impls, which is what the doc already promises.

## MEDIUM-1 — `Dep::parse` without an operator folds an unparseable version into the package name

`dep.rs:257-263`: when `has_version_suffix` holds but `parse_cpv` backtracks,
`parse_cpn_or_cpv` falls through to `parse_cpn` instead of erroring, and
`cpn.rs:153-168` accepts any `[A-Za-z0-9_+-]` run. So the `u64::MAX` rejection
documented at `version.rs:261-263` is **not** applied at this entry point:

```
Dep::parse("cat/pkg-99999999999999999999999") -> Ok(cpn: "cat/pkg-99999999999999999999999", version: None)
Dep::parse("cat/pkg-1abc")                    -> Ok(cpn: "cat/pkg-1abc", version: None)
```

`Version::parse`, `Cpv::parse`, `Pf::parse`, and the *operator* form of
`Dep::parse` (wrapped in `cut_err` at `dep.rs:293`) all reject the same input. The
operator-less `Dep` path is the odd one out, so the guarantee holds everywhere
except where I documented it. The resulting atom is inert (its cpn matches no
real package), so this is silent-acceptance plus name corruption, not a wrong solve.

## MEDIUM-2 — `9f329680` canonicalises an unreachable state, and its doc says otherwise

`parse_slot_dep` (`slot.rs:262-274`) emits only `Operator(op)` for a bare operator
and always `slot: Some(..)` otherwise; the fields are private with no
builder/`From`/serde, so `Slot { slot: None, op: Some(_) }` is unconstructible
outside `slot.rs`'s test module. The `Canonical` machinery is harmless and robust
(`canonical()` is a method used by both `PartialEq` and `Hash`, so no path can skip
it) — but the doc at `slot.rs:123-128` claims PMS 8.3.3 makes the
`Slot { slot: None }` spelling "the *common* case (`:=` outnumbers every named form
in the Gentoo tree)". It is not: the parser routes every one of those to
`Operator`. That false claim legitimises three now-unreachable match arms
(`portage-atom-pubgrub/src/convert.rs:334`, `validate.rs:573`,
`portage-atom-resolvo/src/provider.rs:1419`) as load-bearing.

## LOW / process

- `3c873f62` `cache.rs:601,637`: `spawn_blocking(...).await.unwrap_or_default()`
  turns a panic or cancelled task into "this repo has zero metadata" — an empty
  depgraph, an unresolvable atom, or a gap index covering a whole repo. Silent
  wrong answer instead of a crash.
- `cb046ebc` `portage-binpkg/src/index.rs:111`: new `eprintln!` per container
  missing `CATEGORY`/`PF` on every index open. `AGENTS.md` requires new narration
  to be `tracing::warn!`; this bypasses `-q` and the subscriber.
- `203ff850`: the `u64` PMS 3.2 deviation is recorded **only** in `todo/`, which
  `AGENTS.md` designates prunable scratch that must not be the durable record, and
  `portage-atom/CHANGELOG.md` (published crate, 0.11.1) does not mention it either.
  When the scratch is pruned the justification dies.
- `de02498f` `cache.rs:172-175`: `reconcile_repo_cache` needs the staging target to
  be the *primary* cache dir, so a read-only primary (the ordinary unprivileged
  `em regen`) stages the secondary store and `reconcile_gap_index` is never called.
  Correct results, permanently stale sidecar. Also bundles an unmentioned
  behaviour change: `cache.rs:239-247` now publishes nothing when `error_count > 0`.
- `b122fc18`: the same false-edge defect survives in the resolvo bridge
  (`portage-atom-resolvo/src/provider.rs:1097-1102` matches every alternative of an
  `AnyOf` against every solution member). Low impact — nothing calls that graph
  today — but the fix landed in one of two copies.
- `de9bb272`: silently changed `em which` for an operator-less versioned atom
  (`_ => true` became `self.version.is_none()`). The new behaviour is the
  consistent one, but an unmentioned semantic change in a `refactor:` commit.

## Verified clean (worth recording, since these were the likely suspects)

- `c735f182` binhost cache key: `hex(Sha256(sync_uri))` as a single path component
  is injective up to a SHA-256 collision, and the pre-fix collision was real
  (`http` vs `https`, `?x=1` vs `?x=2`). Atomicity is genuine —
  `NamedTempFile` + `persist` is a same-directory `rename(2)`.
- `6a39c0ef` USE-justification determinism: every other order source feeding the
  BFS is already deterministic (explicit sort, `BTreeSet`, `FxHashMap`); the
  `RandomState` map it replaced was the only one in the path. Fix is complete,
  not local.
- `ba14cf1b` → `c722c1c8`: the revert is exact (`git diff ba14cf1b^ c722c1c8 --
  portage-atom/src/version.rs` empty) and the justification checks out — all three
  named call sites read `numbers` directly, so saturating really would have made
  distinct versions compare equal in the resolver.
- `Version`'s core comparison: brute-forced over 672 three-component versions
  plus 10M randomised triples — no antisymmetry, transitivity, or Eq/Hash
  violation. `BTreeMap<Version, _>` in the pubgrub provider is safe.
- `3329d5ea`, `774ebef5`, `b122fc18` (pubgrub side), `54a9e67d`, `cd0b37d2`,
  `d10ec9a4`, `f898dd99`, `b9eb8b11`, `6b76d606`, `450e3bf9`, `1973feb3`: no
  finding.

## HIGH-4 — `--autounmask` is a user-facing flag with zero consumers

`MergeFlags::autounmask` (`cli/merge_flags.rs:122`) is parsed, documented, and
propagated, but nothing reads it. The only reference outside its own declaration
is the pass-through copy at `maint/resume.rs:423` (`saved.autounmask ||
cli.autounmask`). `emerge.rs:541` forwards only `autounmask_write`, and
`emerge.rs:554` sets `autounmask_widen` from `--target`/cross atoms explicitly
*not* from this flag, exactly as `DepgraphOpts::autounmask_widen`'s own doc says.

So `em -p --emptytree --autounmask @system` is byte-identical to the same command
without `--autounmask` **by construction**. Two consequences: the oracle is three
scenarios, not four; and `capture-plan.sh:28-31` justifies that case as covering
"the autounmask suggestion path", which it does not. The `# required by` line it
was credited with lives in `package_use.rs:396` in the USE-change report, and the
autounmask report is gated on `dropped_autounmask` being non-empty — neither is
flag-dependent.

A documented flag that does nothing is its own finding, independent of the oracle.

## HIGH-5 — my own refactor series fused two doc comments, orphaning a contract

`b077a897` inserted `RootTargets`'s doc directly beneath `best_rebuild_version`'s
last line, fusing the two into one `///` block. Three later extractions re-anchored
on that same final line and the last insertion won. Result at
`depgraph/mod.rs:2023-2045`: lines 2023-2037 document `best_rebuild_version` (naming
`subslot::find_rebuilds`, `rb.version`, `dev-cpp/abseil-cpp`, `dev-libs/protobuf`)
and run straight into `actionable_autounmask_candidates`'s doc with no separator. So
the 30-line autounmask filter carries a 15-line contract about a function whose name
appears nowhere in it, and `fn best_rebuild_version` at 2350 has **no doc comment at
all** — a host-verified policy decision, documented, now attached to nothing.

`RUSTDOCFLAGS='-D warnings' cargo doc` cannot catch this: both items are private, so
rustdoc never renders or link-lints them. Four commits of this series reported
"full verification passed".

## The oracle is weaker than "four cases byte-identical" conveys

Beyond HIGH-4, these extractions are **vacuously** verified — the case exercises the
helper but not its body, so byte-identity would hold even if the body were
`Vec::new()`:

| helper | why it is vacuous |
|---|---|
| `widened_autounmask_candidates` | `autounmask_widen` is false in all cases (only `--target`/crossdev set it), so it returns at its second line |
| `provided_availability` | `package.provided` is empty on this host, so `flat_map` runs zero times |
| `apply_order_filters` | 3 of its 4 filters are dead (`--exclude`, `--resume`, cross-arch host stage); only the reinstall re-append can run |
| `actionable_autounmask_candidates` | `if !widened.is_empty()` only ever takes the false side; the `retain` never runs |
| `target_conflicts` | the `merge_root() == Target` filter is a tautology — no Host/Base entries exist |

Every case also pins `emptytree_native = true` and `cross.active = false`, leaving
the entire non-emptytree half and the entire multi-root half dark: the
`--complete-graph` repair loop, the subslot `:=` rebuild insertion, the `-N`/`-U`
reinstall detection, `best_rebuild_version`, and the Host/Base arms of
`already_installed`.

The code itself is sound — all six bodies are token-identical to their inline
originals, argument order and evaluation order check out (including the
`widened.get()` `Cell` hoist, which is correct because both `set` sites precede the
call and none follow), and the `exclude_omitted`/`resume_omitted` guards are
verbatim so neither `println!` can fire spuriously. It is the *evidence* that is
thin, not the refactor.

## Also in the depgraph series

- `_display_adapter` (`mod.rs:1593-1612`): an 18-field `repo::Adapter` literal
  field-for-field identical to `closure_adapter`, constructed and dropped for
  nothing. Invisible to clippy because of the `_` prefix. Pre-existing
  (`e7a5d0ce`), and the only `_`-prefixed construction in the function.
- `classify_root_targets` takes three consecutive bare `bool`s
  (`empty, selective, is_multi`). Correct today, but a future swap compiles
  silently — the one signature of the six a later edit could break undetected. The
  other five are type-distinct and swap-proof.
- `057bbcd9`'s message claims "the later one at the plan-membership filter is a
  separate binding and stays". There is no such binding: at `057bbcd9^` there is
  one definition, one use, and one doc mention. The commit message asserts a safety
  property that does not exist.

## ebuild module split: not pure code motion, but no production behaviour change

A token-level diff (injective token stream, `syn`-based, comments/uses/`#[doc]`
stripped) over the whole split: of 92 pre-split top-level items, **91 are
token-identical**; 119 functions before and after with identical name sets, none
lost or duplicated; 26 tests before and after, identical names; 428 → 450 doc
lines with **zero dropped or reworded**; 394 → 395 plain comments, none dropped.
The entire production delta is three `mod` declarations.

The deviations:
- 15 items widened `priv` → `pub(crate)`. 12 are required by the move. **3 are
  gratuitous**, with no cross-module caller at all: `binpkg::write_binpkg`,
  `protect::env_d_protect_mask`, `protect::merge_protect_layers`. Blast radius is
  bounded — the modules are private inside `pub(crate) mod ebuild`, so this is
  effectively `pub(in crate::ebuild)`.
- 3 test bodies rewritten for the new `super` (value-identical, and the
  `ConfigProtect::for_test` rewrite is provably equivalent).
- `flock.rs:54` — the intra-doc link `[`package_work_dir`]` no longer resolves,
  because that item lives in `mod.rs` and is not imported. Not linted:
  `lib.rs:17` declares `pub(crate) mod ebuild`, so rustdoc renders nothing inside
  it, and CI's `doc` job uses no `--document-private-items`. Latent, not a CI
  failure — it will bite the day `ebuild` becomes `pub`.

## `portage-atom` details worth carrying forward

- `parse_component` (`version.rs:616-620`) is the only numeric sub-parser without
  `cut_err`, so `Version::parse("1.99999999999999999999999")` reports a generic
  trailing-input error rather than the number-too-large one. No `parse_next` caller
  exists, so nothing is truncated today; the documented guarantee holds by accident
  of call-site choice, not by construction.
- `Version`'s `Hash` is inconsistent with its `Eq` for a NUL letter: `letter` is
  hashed as an `Option<char>` discriminant (`version.rs:374`) but compared as
  `unwrap_or('\0')` (`version.rs:509-511`), so `None` and `Some('\0')` compare equal
  and hash differently — the same defect shape `9f329680` just fixed for `SlotDep`.
  Only reachable via the `builder` feature, which no workspace crate enables.
- PMS 8.3.3 says a slot in `slot=` must not carry a sub-slot, but `slot.rs:246-250`
  parses one and two tests pin the non-compliant behaviour
  (`test_subslot_with_operator`, and `"0/1.75="` in `test_slot_round_trip`).
- The `builder` feature's tests never run in the gating job: no workspace crate
  enables `builder`, and CI's `test` job uses default features. So
  `test_version_builder_roundtrip` — the only test pinning builder-vs-parse `Eq` for
  `Version` — is compiled out of the gate (it does run in the `coverage` job, which
  uses `--all-features`).
- `Version::base()` and `Version::without_suffix()` now have zero callers after
  `de9bb272`, and `without_suffix`'s doc misattributes itself to PMS 8.3.1's `=V*`
  glob, which matches on numeric components only.

## Verified clean — `Version`'s comparison core

Brute-forced by transcription into Python: 0 antisymmetry/transitivity/Eq/Hash
violations across 672 three-component versions over a leading-zero-heavy alphabet,
plus 10M randomised triples with letters, suffixes and revisions, plus an exhaustive
`cmp_component` check over an 18-string digit alphabet. `hash_component` mirrors
`cmp_component`'s pair-dependent branch choice, and `Hash` uses the same
`max(numbers.len(), digits.len())` length rule as `cmp_without_revision`, so the two
cannot diverge. `BTreeMap<Version, _>` in the pubgrub provider is safe.
