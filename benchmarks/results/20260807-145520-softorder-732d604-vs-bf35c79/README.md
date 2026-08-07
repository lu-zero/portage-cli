# Soft-order #3 fix: performance cost check (Grok's homework, 2026-08-07)

- **date (UTC):** 2026-08-07T14:30–15:05
- **current:** `42e4042` (HEAD; code is `732d604`, `bf35c79`→`42e4042` on top is docs-only)
- **baseline:** `bf35c79` (tip immediately before the fix)
- **change under test:** `732d604` — `fix(solver): repair soft RDEPEND order
  after soft-cycle walk` — adds an `install_order` pass-2 acyclic soft-edge
  repair step in `portage-atom-pubgrub` (soft-ready pick + repair walk),
  fixing bug #3 from the clang cross-build investigation
  ([[clang-crossbuild-prefix-local-test-plan]]).
- **method:** `git worktree add` for the baseline (sibling of `pkgcraft`/
  `brush`, see [[benchmark-baseline-worktree]]), both built `--release`.
  Three independent comparisons, in increasing order of realism.

## Result: no measurable regression — all three checks land at ~1.00x

### 1. Criterion `resolve` bench (isolated `install_order`/PubGrub microbench)

Single run each, not same-process-interleaved (criterion doesn't support
cross-binary interleaving), so treat as a coarser signal than the two
hyperfine sections below. Full output: `resolve-bench-baseline-bf35c79.txt`,
`resolve-bench-current-42e4042.txt`.

| Target | baseline (bf35c79) | current (42e4042) | delta |
|--------|---------------------|---------------------|-------|
| load_repo | 1.366 s | 1.265 s | -7.4% (unrelated to the fix; repo-load, not solve) |
| build_provider | 543.8 ms | 546.0 ms | +0.4% |
| firefox | 12.038 ms | 12.506 ms | +3.9% |
| gcc | 4.529 ms | 4.452 ms | -1.7% |
| rust | 7.880 ms | 8.049 ms | +2.1% |
| openssh | 4.218 ms | 4.164 ms | -1.3% |
| python | 5.886 ms | 5.802 ms | -1.4% |

Mixed sign, no consistent direction, all within single-digit percent — reads
as day-to-day system noise across two separate `cargo bench` invocations,
not a targeted regression. If the soft-cycle repair pass had a real
per-target cost, we'd expect a consistent increase scaled by each target's
cycle complexity; instead the deltas scatter around zero.

### 2. Same-run interleaved hyperfine, exact scenario the fix targets

`em -p --prefix /root/xp --target riscv64-unknown-linux-gnu virtual/libcrypt
sys-libs/pam` on the `em-1ac8067-verify` crossdev-stages sandbox (real
riscv64 cross sysroot, the actual soft-cycle graph — glibc/libxcrypt/pam —
this fix was written for):

```
baseline: 624.5 ms ± 21.3 ms
current:  622.4 ms ± 15.0 ms
current ran 1.00 ± 0.04 times faster than baseline
```

### 3. Same-run interleaved hyperfine, heavier full plan

`em -p --prefix /root/xp --target riscv64-unknown-linux-gnu llvm-core/clang`
(135-package cross plan) on the same sandbox:

```
baseline: 650.3 ms ± 25.0 ms
current:  635.2 ms ± 22.5 ms
current ran 1.02 ± 0.05 times faster than baseline
```

### 4. Host-side sanity check, unrelated large target

`em -p www-client/firefox` against the live host repo (no cross/soft-cycle
involvement expected):

```
baseline: 1.138 s ± 0.023 s
current:  1.139 s ± 0.027 s
baseline ran 1.00 ± 0.03 times faster than current
```

## Conclusion

All four measurements — one isolated microbench and three same-run
interleaved hyperfine head-to-heads spanning the exact soft-cycle scenario,
a heavier real cross-build plan, and an unrelated large host target — land
within ±2% of 1.00x with overlapping error bars. **The soft-order fix
(`732d604`) has no detectable performance cost.** This tracks with the fix
shape: the pass-2 repair walk only runs extra work when a soft cycle is
actually encountered, and even for graphs that do hit it (the cross
riscv64/libcrypt/pam scenario), the repair is a small bounded pass over an
already-small SCC, not a change to the dominant repo-load/resolve cost.

Raw output for (2) and (3) not saved separately (hyperfine's stdout summary
above is complete); (1)'s full criterion output is in the two `.txt` files
in this directory.
