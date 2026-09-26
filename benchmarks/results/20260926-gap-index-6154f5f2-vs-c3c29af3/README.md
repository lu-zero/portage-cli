# gap-index fast path: the positional index is a small regression, 2026-09-26

- **baseline:** `6154f5f2` — `out.iter().position` linear scan, `out` mutated by
  `swap_remove` inside the loop
- **current:** `c3c29af3` — positional `HashMap<Cpv, usize>`, removals collected
  and applied in descending index order after the loop
- **delta:** `portage-repo/src/entries.rs` only
- **machine:** thalia (AmpereOne, 128 cores, 4 NUMA nodes) —
  [`machines/thalia.md`](../../machines/thalia.md)
- **why:** the real tree's gap list is empty, so `repo_load` cannot exercise this
  path at all; this A/B needed a synthetic fixture to have any signal.

## Result: current is slower, and the control case says why

Four interleaved rounds per case, criterion run separately in each build, deltas
computed by hand from the medians — never criterion's own `change:` line.

| case | baseline (linear) | current (indexed) | delta |
|---|---|---|---|
| `primary_8000_gap_0` | 39.88 ms | 44.39 ms | **+11.3%** |
| `primary_8000_gap_800` | 41.97 ms | 44.37 ms | +5.7% |
| `primary_8000_gap_8000` | 36.35 ms | 44.64 ms | **+22.8%** |

`gap_0` is the control, and it settles the interpretation: with an empty gap list
the `for cpv in &cpvs` body never runs, so the lookup cannot be responsible for
the +11.3%. What the change added is the **unconditional build of a
`HashMap<Cpv, usize>` over every primary entry**, inside the fast path and ahead
of the loop. A `HashMap` carrying a key and a `usize` is dearer to build than the
`HashSet<Cpv>` it replaced, and at 8,000 entries that is the whole difference.

The +22.8% at `gap_8000` is the same fixed cost plus a scan that turned out to be
cheap: `Cpv` comparison fails on an interned-pointer inequality almost
immediately, so 32M comparisons of a `position` scan cost less than building one
8,000-entry hash map. The O(gap x primary) term was real but never the dominant
one.

Ranges overlap heavily (baseline 32.03–47.45 ms, current 41.04–48.20 ms across
all cases, n=4), so no single row is significant on its own. What makes this
conclusive is the sign agreeing across all three cases *including* a control
where the changed code is inert.

## Outcome: index reverted, ordering fix kept

Reverting the index (keeping only the descending-index removal that replaced
`swap_remove`) and re-measuring against the indexed build, 5 interleaved rounds:

| case | indexed | reverted | delta |
|---|---|---|---|
| `primary_8000_gap_0` | 43.51 ms | **35.39 ms** | **-18.7%** |
| `primary_8000_gap_800` | 43.72 ms | **37.34 ms** | **-14.6%** |
| `primary_8000_gap_8000` | 44.80 ms | **37.05 ms** | **-17.3%** |

`gap_0` being the *fastest* reverted case is the control working in the other
direction too: with nothing in the gap list there is no lookup to do, so the
reverted build does the least work of all three. The indexed build's cost is
flat across all three (~44 ms) precisely because it is the fixed hash-map build,
independent of what the loop then does.

The surviving change is a one-line ordering fix, not a data-structure change:
removals are applied in descending index order rather than by `swap_remove`, so
the surviving entries keep the primary cache's order and `repo_entries` output no
longer depends on the gap list's order.

## Harness note

`cargo build -p portage-bench --benches` defaults to the **dev** profile. Copying
`target/release/deps/<bench>` after such a build silently re-copies the previous
`cargo bench` artifact, so an A/B compares one binary against itself — the first
attempt at this measurement did exactly that and produced a self-contradictory
−93.7% "improvement". The two binaries here are md5-distinct (see `meta.env`)
and were produced with `--release`.

## Files

- `meta.env` — machine, load average, commit shas, binary checksums
- `criterion-interleaved.txt` — raw criterion output, all 8 runs
- `criterion-interleaved.json` — parsed per-run point estimates
