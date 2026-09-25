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

## 1b. Two perf commits shipped with no measurement at all

Checking this prompted by the commit count, and it's the sharpest thing I found.
Of 19 `perf(...)` commits, 17 carry a number in `benchmarks/BENCHMARKS.md` or a
paired `bench(...)` commit. Two carry nothing:

- `1a52b6b1 perf(activity): bound background sink queues`
- `0c30acb1 perf(activity): skip unused orphan history`

Neither appears in `BENCHMARKS.md`, and neither has a follow-up bench commit —
unlike `cd0b37d2`/`02018404`, whose measurements landed in `29458ac1` and
`0cb67883` respectively. These two went in on reasoning alone ("skip unused
work", "bound the queue") and broke the discipline the other seventeen kept.

They're also the pair I trust *least*. Both touch `portage-activity`, both are
about not doing work, and "we no longer do this work" is exactly the shape of
change that produces a wrong answer rather than a slow one. A bounded queue can
also silently drop or reorder entries, which is a correctness question, not just
a throughput one. I should not have shipped them unmeasured and I can't
retroactively vouch for them.

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
