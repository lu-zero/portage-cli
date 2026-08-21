# USE_EXPAND flag composition: a group/value/flag triple, or memchr?

Opened 2026-08-21, spun out of the `94ea5a1` `IUSE_EFFECTIVE` perf-regression
fix (`1e9a21b`). Not started — a proposal only.

## The gap this would close

Once composed, a USE_EXPAND flag like `python_targets_python3_13` can't be
split back into its group (`PYTHON_TARGETS`) and value (`python3_13`) by
string-scanning — the flag's own name has no marker for where the group
prefix ends and the value begins (`elibc_glibc` vs `elibc_glibc_musl`-shaped
ambiguity is real once you don't have the original `USE_EXPAND` key list in
hand). Anything that wants to *group*, *hide* (`USE_EXPAND_HIDDEN`), or
round-trip USE_EXPAND flags currently either re-derives the split from
context it happens to have nearby, or can't do it at all: `em use`/`em pkg
use`'s expand grouping, `--info -v` display, `package.use`'s `KEY: v1 v2`
colon form.

The same "compose via `format!("{prefix}_{value}")` then intern" pattern
shows up at four sites (found via grep while fixing `94ea5a1`):

- `portage-metadata/src/iuse_effective.rs:64,100` (now the only hot one —
  see below, and it's already been fixed by caching)
- `portage-resolve/src/use_env.rs:558,568` (`expand_use_expand_colon`,
  config-load-time `package.use` parsing — cold)
- `portage-repo/src/build/profile.rs:495` (per-profile-layer, per-solve —
  cold)

## Why this is a modelling cleanup, not a perf fix

`1e9a21b` already fixed the actual `94ea5a1` regression by caching the
profile-invariant `IUSE_EFFECTIVE` half once per solve (`IuseInjection::new`)
— the composed flags at
`iuse_effective.rs:64,100` are now computed once total, not once per
candidate. Two independent Opus review passes (well, one pass, asked to
evaluate this specific idea) concluded a triple **would not have fixed** the
regression and is mildly *negative* on a genuinely hot path: it composes
worse (3 interns instead of 1 concatenated string) and costs more per-entry
memory (12 bytes vs 4) for a set whose only real hot operation is
`iuse.contains(&flag)` against plain-string `use.mask`/`use.force` entries,
which must be keyed on the combined form regardless of what else the type
carries.

So: worth doing for the display/round-trip case, decoupled from any solver
hot path.

## Two shapes to weigh

**A. A stored triple/table** (Luca's original framing — `{ group: Interned,
bare: Interned, expanded: Interned }`), owned by a `UseExpandTable` that
memoizes `(group, value) -> flag` so repeated compositions for the same pair
don't re-allocate:

```rust
// portage-atom, not gentoo-interner (which is deliberately domain-free
// generic string interning — this is Portage-domain).
pub struct UseExpandGroup {
    name:   Interned<DefaultInterner>,  // "PYTHON_TARGETS"
    prefix: Interned<DefaultInterner>,  // "python_targets" — lowercased once
}
pub struct ExpandedFlag {
    group: Interned<DefaultInterner>,
    value: Interned<DefaultInterner>,
    flag:  Interned<DefaultInterner>,   // "python_targets_python3_13"
}
// Hash/Eq/Borrow<Flag> delegate to `flag` only, so an ExpandedFlag is
// interchangeable with a bare Flag in any set keyed by flag name.
pub struct UseExpandTable {
    groups: HashMap<Interned<DefaultInterner>, UseExpandGroup>,
    memo:   HashMap<(Interned, Interned), Interned>,
}
```

**B. Luca's alternative: skip the stored triple, split the *existing*
composed flag string with `memchr`** against the known group prefix length
(the prefix is already known wherever a split is needed — `USE_EXPAND`'s
key list is always in scope) instead of carrying group/value alongside every
flag. Cheaper to add (no new type, no memo to keep coherent), and worth
sizing up first: `memchr` a single byte at a known offset is about as fast
as an operation gets, and the display/grouping call sites aren't hot at all
(once per `em use`/`--info -v` invocation, not per solve) — so shape A's
whole reason to exist (avoiding repeated composition cost) may not be worth
the added type surface if B is "fast enough compared to the current
status" for what's actually a cold path.

## Before starting

- Actually benchmark B against A for the real display call sites — this
  whole item may resolve to "just memchr-split at display time, don't add a
  type" once measured, per Luca's own instinct.
- If A wins, decide whether `ExpandedFlag`'s `Hash`/`Eq`/`Borrow<Flag>`
  delegation-to-`flag`-only design actually composes cleanly with the sets
  `iuse_effective_set()`/`ForceMask::effective()` already build (`HashSet<Flag>`,
  `BTreeSet<Flag>`) — those would need to stay `Flag`-keyed regardless, so A's
  value is purely at the display/config-parsing boundary, not inside the
  solve.
