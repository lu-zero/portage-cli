# Profiling the resolve benchmark

This document captures the workflow we use to profile
`portage-bench`'s solver benchmarks. It exists so we do not
re-derive it every time we want to look at the hot path.

## Quick recipe

```sh
# 1. Build the resolve bench with the allocator you want to
#    profile, plus debug info via the `profiling` profile.
cargo bench --bench resolve --features mimalloc --profile profiling --no-run

# 2. Cargo prints the binary path; export it.
RESOLVE_BIN=target/profiling/deps/resolve-<hash>

# 3. Run perf record. --profile-time keeps the bench alive long
#    enough for perf to collect samples (criterion's bench mode
#    decides how long to run otherwise).
perf record -F 997 --call-graph dwarf -g -o /tmp/resolve-mimalloc.perf \
    -- "$RESOLVE_BIN" --bench --profile-time 20 \
       'resolve/targets/portage-atom-pubgrub/firefox'

# 4. Inspect.
perf report -i /tmp/resolve-mimalloc.perf --stdio -c $(basename "$RESOLVE_BIN") \
    --no-children --percent-limit 0.5
```

samply (visual flame graph in browser) is the alternative:

```sh
samply record -- "$RESOLVE_BIN" --bench --profile-time 20 \
    'resolve/targets/portage-atom-pubgrub/firefox'
```

## Why the `profiling` profile

`portage-bench/Cargo.toml` defines:

```toml
[profile.profiling]
inherits = "release"
debug = 1
lto = false
```

- `debug = 1` — line tables so perf can resolve symbols.
- `lto = false` — keeps function boundaries intact so the
  call graph isn't shredded by cross-crate inlining.
- inherits release — keeps opt-level=3 so we are profiling
  what the user will actually run.

Do not profile the default `bench` profile (full LTO):
addresses resolve to inlined fragments and the flat profile
becomes useless.

## Permissions

perf needs `kernel.perf_event_paranoid <= 1` for user-mode
profiling with call graphs. On this box that requires:

```sh
sudo sysctl -w kernel.perf_event_paranoid=1
# optionally also raise mlock for DWARF unwinder:
sudo sysctl -w kernel.perf_event_mlock_kb=8192
```

This is per-boot. Add to `/etc/sysctl.d/` to persist if you
profile often.

## Important caveat: setup is in the profile, not in the measurement

The resolve bench uses `criterion::Bencher::iter_with_setup`:

```rust
b.iter_with_setup(
    || build_provider(base_repo.clone(), sys.use_config(), &sys.package_use),
    |mut provider| criterion::black_box(provider.resolve_targets(targets.clone())),
)
```

Criterion runs `setup` before every iteration but excludes its
time from the reported mean. **perf does not know that.** Every
sample lands somewhere on the wall clock, so the resulting
profile is a mix of:

- `build_provider` → `PortageDependencyProvider::new` (setup,
  not timed)
- `base_repo.clone()` (setup, not timed)
- `provider.resolve_targets(targets.clone())` (the measured
  routine)

In practice the setup is several times more expensive than the
routine, so the profile is dominated by `Vec::clone`,
`provider::new`, `convert::ConvertCtx::*`, `Iterator::partition`
and `DepEntry::drop` — all of which are setup, not resolve.

When reading a perf profile of this bench, remember:
**setup work shows up too**, and disproportionately. To profile
just the measured routine, build the provider once and use
`iter()`. The synthetic-root work in `resolve_targets` is
idempotent (the root is removed at the end of every call), so
this is safe.

## Tracking allocations (dhat)

`portage-bench` has a `dhat-heap` feature that swaps the global
allocator for `dhat::Alloc`. It's mutually exclusive with
`mimalloc` (the resolve bench has `cfg(all(feature = "mimalloc",
not(feature = "dhat-heap")))` gating mimalloc out when dhat is
on).

```sh
cargo bench --bench resolve --features dhat-heap --no-run
$RESOLVE_BIN --bench 'resolve/targets/portage-atom-pubgrub/firefox'
# writes dhat-heap.json next to the binary
```

Note: dhat's `Profiler` writes its report when it is dropped.
A criterion `--bench` run only flushes after criterion exits,
so the report covers the whole run including setup.

Open the resulting `dhat-heap.json` in
[dh_view](https://nnethercote.github.io/dh_view/dh_view.html).

## How to read the matrix

The benchmark sweep we use (see `docs/profile-analysis.md` in
gentoo-interner) is a 3×2:

| dimension      | values                                  |
|----------------|-----------------------------------------|
| backend        | papaya (default), `lasso`, `symbol-table` |
| allocator      | glibc (default), `mimalloc`             |

`cargo bench --bench resolve --features <feats>` produces a
mean per resolve. Save baselines if you want criterion's diff
output:

```sh
cargo bench --bench resolve                          -- --save-baseline papaya-glibc
cargo bench --bench resolve --features mimalloc      -- --save-baseline papaya-mimalloc
cargo bench --bench resolve --features lasso         -- --save-baseline lasso-glibc
cargo bench --bench resolve --features lasso,mimalloc -- --save-baseline lasso-mimalloc
# ...
```

## Output we have on file

- `gentoo-interner/docs/profile-analysis.md` — the confirmed
  result of the 3×2 matrix: mimalloc collapses the inter-backend
  gap and is the dominant variable for this workload.

## Optimization candidates known to be hot

From the most recent perf trace under mimalloc, in rough order
of leverage (per the caveat above — these include setup):

1. **`PackageRepository::versions_for` clones every meta on
   every call**, and provider construction calls it twice per
   CPN. Returning borrowed data, or caching the first pass,
   would remove a large chunk of `Vec<DepEntry>::clone` traffic.

2. **`Iterator::partition` in the post-construction
   reachability check** allocates two `Vec`s per package
   version when most of the work is just retaining. Using
   `Vec::retain` with a side-channel for the dropped list
   would skip one allocation.

3. **`base_repo.clone()` in the bench `iter_with_setup`**
   clones the whole repo per iteration. Once we have a
   resolve-only example (see above), this drops out
   entirely.

4. **`InMemoryRepository::clone` itself** copies the full
   `HashMap<Cpn, Vec<(Cpv, PackageVersions)>>`. Until the
   bench harness changes, this is the per-iter cost we pay.

5. **`SmallVec` extend/drop in `convert::convert_deps`** —
   smaller (~4%) but worth a look if (1) and (2) are not enough.
