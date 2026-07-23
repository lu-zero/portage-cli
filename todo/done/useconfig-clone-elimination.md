# Eliminate the per-package `UseConfig` clone in `desired_use`

STATUS: **superseded — implemented differently.** The Cow-through-solver
plan below (Option 2) was reviewed independently (Fable model) before
implementation and found unsound: its core premise doesn't hold on real
profiles. See "What actually shipped" below for the fix that replaced it.

## What actually shipped

Fable's review (and a follow-up spot-check against the actual code)
found two things:

1. **The "free borrow" fast path in Option 2 never fires in practice.**
   `desired_use` (`repo.rs`) already calls `.into_owned()` unconditionally
   before `apply_iuse_defaults`, and the very next step,
   `ForceMask::apply`, mutates `cfg` for essentially every real package —
   `ForceMask::is_empty()` requires the *global* `use.mask` to be empty
   too, which no real Gentoo profile has. So there is no case in practice
   where a version's effective USE could have been served by borrowing
   the global config directly; Option 1/2's premise doesn't hold.
2. **A real, cheaper bug**: `apply_package_use` (`portage-solver/src/use_config.rs`)
   only returned `Cow::Borrowed` when the whole `package_use` list was
   empty, not when no entry actually matched the package — so it cloned
   on every call once *any* `package.use` entry existed anywhere in the
   system (i.e. always, on a real profile).
3. **The actual dominant per-version cost** wasn't the ~35-entry global
   clone this doc focused on — it was `ForceMask::effective` unconditionally
   extending the masked set with the *entire* global `use.mask` (hundreds
   of entries) and disabling every one of them on `cfg`, for every version,
   regardless of whether the package's own `IUSE` even declares that flag.

Implemented instead (uncommitted as of this writing, in the working tree):

- `apply_package_use`: checks for an actual atom match before cloning,
  not just whether the list is non-empty.
- `ForceMask::effective`/`apply`/`pins`: take the package's own `IUSE` set
  and restrict the global `use.mask` scan to flags actually in it — a
  masked flag outside a package's `IUSE` can never be resurrected by that
  package's own `+flag` default, so skipping it is a no-op for correctness
  (verified: `UseConfig::get`/`get_with_iuse_default` already default an
  absent flag to `Disabled`, same as an explicit `disable()` would set).
  `pins()` still includes the full global set regardless (needed for the
  Level-C cede gate; cheap, not the hot path), so this only narrows what
  `apply()` computes.

### Verification

- `cargo nextest run --workspace --exclude portage-bench`: 1209 passed (3
  new: `apply_package_use_borrowed_when_list_nonempty_but_no_match`,
  `global_use_mask_only_applies_to_packages_own_iuse`, plus the existing
  force_mask tests updated for the new `iuse` parameter). Clippy/fmt clean.
- Parity: `USE="-* build"`-style live comparison of
  `em --root <dir> -vp sys-devel/gcc` against real
  `ROOT=<dir> emerge -vp sys-devel/gcc` — exact 16/16 package and USE-flag
  match (byte-identical modulo cosmetic formatting). Notably, the *old*
  (pre-fix) binary had 2 phantom packages (`virtual/libintl`,
  `virtual/libiconv`) that real emerge does not include — the `ForceMask`
  fix incidentally corrected this latent over-masking bug too.
- `benchmarks/bench-em-vs-emerge.sh SKIP_TIMING=1`: identical diff counts
  before and after (7/7/7 on firefox/thunderbird/libreoffice — a
  pre-existing, unrelated discrepancy; 0 elsewhere) — confirms no new
  parity regression.
- End-to-end timing (`hyperfine --warmup 2`, this host):
  - `dev-qt/qtwebengine` (82-package plan, heavy IUSE): 755.6 ms → 701.7 ms
    (≈8% faster, before 1.08±0.04× slower than after).
  - `app-office/libreoffice` (134-package plan): 874.9 ms → 810.9 ms
    (≈8% faster).
  - `sys-devel/gcc` (16-package plan, light IUSE): no measurable
    difference (520.0 ms vs 523.1 ms) — as expected, the win scales with
    the size/IUSE-richness of the dependency closure the solver walks,
    not with small plans.

## Original proposal (superseded, kept for context)

Measured as worth doing; the correctness groundwork (the `wildcard_reset`
bit) landed in `2f846a4`.

## Why

Every version the solver instantiates calls `Adapter::desired_use`
(`repo.rs:671`), which currently does:

```rust
let mut cfg: UseConfig =
    apply_package_use(self.use_config, cpv, slot, self.package_use).into_owned();
if let Some(m) = meta {
    apply_iuse_defaults(&mut cfg, m);
}
```

`.into_owned()` clones the **entire global `UseConfig`** (a
`HashMap<Interned, UseFlagState>`) for *every* version in the dependency
closure — thousands of times for a heavy resolve (gcc, firefox, qemu).
`apply_package_use` already returns `Cow::Borrowed(self.use_config)` when no
`package.use` entry matches (the common case), but `.into_owned()` throws that
optimisation away and clones unconditionally, because the caller needs an owned
`UseConfig` to fold IUSE defaults into.

`apply_iuse_defaults` then mutates only the package's *own* IUSE flags (the
ones absent from the global set) — typically 5–40 flags for a medium package,
up to ~90 for gcc/firefox. The other ~35 global flags were cloned only to be
copied verbatim and never touched.

## Measured cost

Isolated microbenchmark (`HashMap<Interned,State>`, real flag counts), 5000
packages (a realistic dep-closure version count), averaged over 100 reps:

| model | global flags | IUSE defaults | per-resolve |
|---|---|---|---|
| A — clone global + fold (current) | 35 | 40 | 6.06 ms |
| A2 — overlay only, no clone | 35 | 40 | 4.74 ms |
| B — token-stream rebuild | 35 | 40 | 5.42 ms |

For large packages (gcc/firefox, ~90 IUSE) the gap widens: A ≈ 15.9 ms,
B ≈ 9.3 ms (≈0.58×). The clone scales with the *global* flag count; the
overlay/rebuild scales with the *package's own* IUSE count. A2 (overlay) is the
fastest at medium sizes because it touches the smallest set.

These are per-resolve figures; against a 700 ms end-to-end `em -p sys-devel/gcc`
they are <1%, but they are pure CPU (no I/O), deterministic, and the clone also
drives allocation pressure (each clone reallocates the HashMap's node array).

### Why end-to-end timing won't show it cleanly

`em -p` wall time on this host is 700–900 ms with ±15–20% variance
(hyperfine, 8–10 runs), dominated by VDB/repo I/O and parallel sys time
(5–6 s user+sys on 0.7 s wall). The clone saving is ~5 ms — below the noise
floor of the end-to-end measurement. A controlled microbenchmark or an
allocation profile (dhat-heap) is the honest way to confirm the gain; the dhat
run was started but the profiling build's runtime made it impractical to
capture in-session.

## Correctness (already verified)

All three models — A (frozen set + `wildcard_reset` flag), A2 (overlay),
B (token-stream rebuild) — are **provably equivalent** to portage's true
accumulator (`[pkginternal defaults]…[make.defaults]…[make.conf]…[env]`,
`-*` = clear-at-point) across 200 000 fuzz cases each (2- to 7-flag universes,
random `+`/`-`/none defaults, random token streams with `-*` in every
position). Zero mismatches. See the analysis in the conversation that produced
`2f846a4`: the `wildcard_reset` bit is the *only* information the frozen set
cannot recover on its own (absent-after-`-*` vs never-mentioned), and A2/B
compute the same result by construction.

So this is a pure perf refactor; no behaviour change, no new signal needed.

## Proposal: A2 — shared global + small per-package overlay

Keep the frozen global `UseConfig` shared (behind the existing `&UseConfig`
borrow on `Adapter`), and have `desired_use` build only the **overlay**: the
package's own IUSE flags that are absent from the global set, taking their
`+`/`-` default (suppressed under `wildcard_reset`). The solver's
`VersionData.desired` becomes "global, plus this package's overlay" rather than
a flattened copy.

Two shapes for the overlay, in increasing intrusiveness:

1. **Flat owned `UseConfig`, built from the overlay only.** Smallest change:
   `desired_use` allocates a `UseConfig` containing *just* the flags
   `apply_iuse_defaults` would set (the absent ones), plus any `package.use`
   overrides, and the solver reads `desired.get(flag)` as
   "overlay if present, else global". This needs `desired` to carry a reference
   to the global config — currently it's a flat `UseConfig` stored by value in
   `VersionData`, so `VersionData` would hold `(global: &UseConfig, overlay:
   UseConfig)` or the read path would take the global as an argument.

2. **`Cow<'_, UseConfig>` through the solver.** `desired_use` returns
   `Cow::Borrowed(&global)` when the package has no IUSE defaults and no
   matching `package.use` (surprisingly common — pure deps with no local USE),
   and `Cow::Owned(overlay)` otherwise. The solver's `convert`/`validate` paths
   already take `&UseConfig`; they'd take `&UseConfig` from the `Cow` with no
   other change. This is the cleanest: zero allocation for the common
   no-local-USE package, and the overlay path only builds the small set.

Option 2 is preferred — it extends the `apply_package_use` `Cow` pattern that
already exists, and makes "package with no own USE" free. The cost is that
`VersionData.desired` becomes `Cow` (a lifetime into the solver's data
structure) or the solver re-derives the effective config lazily.

### Touch points

- `portage-solver/src/use_config.rs`: `UseConfig::merge_overlays(global,
  overlay)` helper, or a small `EffectiveUse` newtype over `(&UseConfig,
  &UseConfig)`.
- `portage-cli/src/query/depgraph/repo.rs`: `Adapter::desired_use` (line 671)
  builds the overlay instead of cloning; `effective_use_config` (line 337)
  likewise.
- `portage-atom-pubgrub/src/provider/mod.rs`: `VersionData.desired` (line 555)
  — either becomes `Cow` or the read sites gain a global-config argument.
- `portage-atom-pubgrub/src/repository.rs:278` (`InMemoryRepository`'s
  `desired_use`): same treatment for symmetry, though it's only used by
  benchmarks/tests.
- The three display/validation fallbacks (`output.rs`, `required_use.rs`,
  `download_size.rs`) and `effective_use.rs` call `apply_package_use`; they get
  the win automatically once `desired_use`/`effective_use` stop cloning.

### Risk

The `VersionData.desired` lifetime change is the invasive part — the solver
stores `desired` by value today and reads it in several post-solve loops
(`post_solve.rs`, `validate.rs`). Threading a `&UseConfig` global through those
read sites is mechanical but touches the solver/solver-boundary contract
([USE/solver boundary](../portage-atom-pubgrub/docs/use-and-solver-boundary.md)).
If that's unwanted, option 1 (overlay-built flat `UseConfig`, but only the
overlay entries, looked up as overlay-else-global via a stored global ref) keeps
`desired` owned but still avoids cloning the global entries.

## Verification plan

1. Correctness: re-run the 200k fuzz comparison against portage's accumulator
   after the change (the `correctness*.py` scripts from the `2f846a4` session).
2. Perf: controlled microbenchmark of `desired_use` over a real gcc
   dep-closure's version list, before/after — expect the medium-package case to
   drop from ≈6 ms to ≈4.7 ms and the large-package case from ≈16 ms to ≈9 ms
   per resolve.
3. Allocation: `cargo build --release --features dhat-heap` and compare
   `totBytesAllocated` / `totBytesInUseAtMax` for `em -p sys-devel/gcc` before
   vs after (the clone is allocation-heavy, so this is a stable proxy).
4. Parity: `benchmarks/bench-em-vs-emerge.sh` package-set parity must stay
   identical (no behaviour change).

## Relationship to the `wildcard_reset` work

This is the perf follow-up to `2f846a4` (the `USE=-*` correctness fix). That
commit established the frozen-set + flag representation and proved its
equivalence to portage; this proposal removes the clone that representation
currently pays per package, without touching the flag (which is free — one
`bool`).
