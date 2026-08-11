# depgraph RepoSet threading — confirm the anchor, 2026-08-11

- **change:** `19d8fe8` (refactor(depgraph): thread the RepoSet through
  DepgraphOpts, not repo_path) + `41636c4` (alias find_cpns).
- **baseline:** `7dbe9fc` (the reachable equivalent of the prior anchor's
  `2b79d47`/`b1a8bcc` — see
  `results/20260811-repoloading-redesign-anchor-2b79d47/README.md`'s
  SHA-labelling note).
- **current:** `d47b763` (HEAD after both fixes + the anchor README sync).
- **question:** does threading the caller's already-built `RepoSet` through
  `DepgraphOpts` (instead of rebuilding it from `repo_path` inside
  `depgraph()`) measurably speed up `em -p`? It should halve the per-merge
  `Repository::open` count.

## 1. strace — the clean signal

`strace -f -e trace=openat em -p sys-devel/gcc`, counting each repo's
`repo_name` / `layout.conf` opens (one per `Repository::open` of that repo).
Full output: `strace-repo-opens.txt`.

| file | baseline (`7dbe9fc`) | current (`d47b763`) |
|---|---|---|
| gentoo `profiles/repo_name` | 8 | **4** |
| gentoo `metadata/layout.conf` | 8 | **4** |
| guru `metadata/layout.conf` | 2 | **1** |
| crossdev `metadata/layout.conf` | 2 | **1** |

Exactly halved across every repo. The second `repo_set_from_conf` build that
used to run inside `depgraph()` is gone — every repo is opened once per
merge instead of twice. (gentoo shows 4, not 1, because it is opened as
main, as each overlay's master, and transitively — all halved, none added.)

This is the definitive confirmation: the structural change landed as
intended, and the per-merge repo-opening work (the `layout.conf` parse,
`repo_name` read, categories read, arch.list, …) is cut in half.

## 2. hyperfine wall-clock — noise-bound on this run

Interleaved `hyperfine --warmup 2 --runs 10 -i`, `--profile quick` binaries,
real host repo. Full raw: `hyperfine-raw.txt`.

| target | baseline | current | ratio |
|---|---|---|---|
| `www-client/firefox` | 1.426 s ± 0.104 s | 1.487 s ± 0.130 s | baseline 1.04x faster |
| `sys-devel/gcc` | 1.245 s ± 0.101 s | 1.331 s ± 0.130 s | baseline 1.07x faster |
| `net-misc/openssh` | 1.272 s ± 0.109 s | 1.213 s ± 0.071 s | current 1.05x faster |

A focused gcc re-run (`--warmup 5 --runs 15`) landed at `1.01 ± 0.12`
(baseline marginally faster).

**Reading this: do not trust the wall-clock here.** The host was contended
(load avg 4-7, other agent processes active) and variance was ±8-10% — far
above the ~1% effect one fewer repo-open produces on an I/O- and solve-
dominated ~1.3 s workload. The three targets don't even agree on direction
(firefox/gcc say baseline faster, openssh says current faster), the classic
signature of noise rather than signal. The prior anchor README
(`20260811-repoloading-redesign-anchor-2b79d47`) got ±2-5% on a quieter
machine; this run cannot resolve an effect that small.

The strace result in section 1 is the real evidence. A clean wall-clock
confirmation of the *magnitude* would need an idle host (load < 1) and
probably `--runs 30+`; deferred until the machine is free.

## Conclusion

The fix works: `Repository::open` count per `em -p` merge is halved (strace,
deterministic). Wall-clock impact is positive-but-small and below this
run's noise floor — consistent with repo-opening being a small fraction of
a resolve's total time (the bulk is `repo_entries`' cache read + the
pubgrub solve, both unchanged by this refactor).
