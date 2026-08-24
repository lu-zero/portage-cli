# Newest-wins resolution + single-layer autounmask cascades on a fresh unkeyworded slot

Status: 🔴 not started, open design question. Found 2026-08-24 during the
crossdev-stages replacement retest, testing `--ex-pkg
sys-devel/clang-crossdev-wrappers` (i586 pilot sandbox).

## What was observed

`cross-i586-pc-linux-gnu/clang-crossdev-wrappers` resolves to slot `23`
(newest) by default. `--autounmask-write` surfaces and fixes exactly one
unsatisfiable edge per run — accepting `clang`/`lld`'s `**` reveals
`clang-common` needs it too; accepting that reveals `llvm-core/llvm`
needs it too. Each pass is a real, correct fix for what it surfaces — the
cascade itself is the finding, not a wrong fix.

**Had the resolver actually try all three nearby slots explicitly**
(`em -p --autounmask --autounmask-write cross-i586-pc-linux-gnu/
clang-crossdev-wrappers:SLOT`, iterated to convergence, from a clean
`package.accept_keywords` baseline each time — the real experiment, not
just reading `KEYWORDS=` lines by hand):

| slot | autounmask entries needed | result |
|---|---|---|
| **21** | **0** | resolves clean immediately, ends in a full real plan |
| **22** | n/a — not a keyword issue | hard resolver conflict, unrelated to masking (see below) |
| **23** (picked by newest-wins) | 2 auto-discovered (`clang`, `lld`) + at least 1 more autounmask can't find on its own (`clang-common`) | still not resolved after 2 passes |

**Slot 22's failure isn't upstream keyword lag at all** — it's a real
conflict with crossdev-stages' *own* sandbox config:
`llvm-core/llvm:22` DEPENDs on `llvm-core/llvmgold:0 >=22`, and this
sandbox's `package.mask/llvm-unused-slot` (written by
`crossdev-stages/src/portage.rs::MakeConf::write`) masks exactly
`>=llvm-core/llvmgold-22` — deliberately, to keep `dev-lang/rust`'s own
`llvm_slot_21` pin from dragging in the whole llvm:22 chain for nothing.
That mask, written for a completely unrelated reason, makes
`clang-crossdev-wrappers:22` unresolvable in this same sandbox. Real,
distinct finding: two of crossdev-stages' own mechanisms (the Rust
llvm-slot pin and any future `--ex-pkg clang-crossdev-wrappers` use)
collide with each other, independent of anything `em`-side.

## The open question (not decided, needs its own investigation)

Luca: "we have to sort out if we can resolve the
last-usable-version-given-the-dependencies or we should autounmask the
whole set." Two candidate directions:

1. **Prefer a version that needs strictly less masking** when a newer
   candidate would cascade further than a nearby older one — not
   necessarily "fully satisfiable with zero masking" as the bar (slot 21
   here is that; slot 22 isn't, and is still clearly the better pick over
   23). Changes solver semantics generally, not just for this case —
   needs to confirm this wouldn't fight `-u`/`--deep`'s existing meaning,
   and check what real portage actually does here first (candidate: real
   portage's own autounmask may have the *same* single-layer-at-a-time
   limitation — this might not be a divergence to fix, but confirm
   before assuming).
2. **Autounmask the whole transitive set in one pass** instead of one
   edge at a time — collect every masked package the *would-be* plan
   touches (not just the first unsatisfiable one) and write acceptance
   for all of them together. Smaller, more mechanical change; doesn't
   touch version-selection preference at all, just autounmask's own
   single-pass scope.

## Same shape as the GCC version-pin gap

This is the same underlying need as
[[crossdev-gcc-version-flag]]/`crossdev-stages/todo/em-replaces-crossdev-gcc-pin.md`:
a caller-facing way to pin/steer which slot or version gets selected,
so resolution doesn't have to reach for "newest" and then fight a
cascading mask (or, here, so a caller who *wants* slot 21 — the one
that actually resolves clean in this sandbox — can just say so directly
instead of hoping autounmask eventually gets there, or worse, landing on
22 and hitting an unrelated masked-package wall). Worth designing both
together rather than separately — a
general version-pin mechanism might make direction 1 above unnecessary
in practice (the caller pins what they want, the cascade question
becomes moot for that case), while direction 2 is still worth doing
independently since not every caller will want to pin.

## How to attack

1. Check what real portage's `--autounmask`/`--autounmask-write` actually
   does for a comparable fresh-slot-bump scenario — settles whether
   direction 1 is a real divergence worth fixing or an intentional
   match to existing (if inconvenient) real-world behavior.
2. Prototype direction 2 first (smaller, more mechanical, no semantics
   change) — see if it alone resolves cases like this one acceptably.
3. Only pursue direction 1 if 1's investigation shows real portage
   itself avoids the cascade some other way.
