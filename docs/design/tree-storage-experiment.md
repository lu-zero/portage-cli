# The redb experiment: what storing ::gentoo in a database taught us

| Field | Value |
|---|---|
| Status | Experiment closed. The storage split is kept; the redb backend stays experimental behind the `redb` feature |
| Date | 2026-11-06, updated 2026-11-07 |
| Design | [`tree-storage.md`](./tree-storage.md) |
| Spike | branch `spike/redb-storage`, `spikes/tree-redb/` (`DESIGN.md`, `RESULTS.md`): the sync, size and md5-cache read figures |
| Host | thalia: Ampere-1a, 128 cores; runs pinned with `numactl -N 3 -m 3`, warm page cache unless stated, checkout and redb at one commit |

Can a Gentoo repository live in a database synced straight from git, with no checkout? It can.
The sync is much faster, the size halves or better on a plain filesystem, reads are a little
faster. What lasts is the split it forced on em: a repository's bytes come from a `TreeSource`
and its metadata from an `EntrySource`, whatever stores them.

## What was built

1. **The spike:** a git protocol v2 sync that completes the thin pack from objects already in
   redb, checks the tip's signature and applies the tree diff in one write transaction.
2. **The storage split** (steps 1–5): every reader goes through `Repository` and its tree.
3. **The redb backend:** `<location>.redb` holds the tree, `<location>.md5-cache.redb` the
   metadata; em owns the schema and syncs `sync-type = git-redb` itself.
4. **Builds without a checkout** (step 7): eclasses are read through the trees; only the package
   directory is made real, as a copy kept per tree.

## What a checkout holds

![What ::gentoo is made of, files per kind](./img/tree-storage/gentoo-tree-composition.svg)

132,261 files, 207 MiB at 1.6 KiB on average; on 4 KiB blocks they take 591 MiB. Every ebuild has
one md5-cache file, so the two are half the tree.

## Results

### Sync

![Sync time, git against the redb spike](./img/tree-storage/redb-sync-time.svg)

| Behind | git fetch + reset --hard | redb sync | Wire pack |
|---|---|---|---|
| 1 day | 0.83–0.97 s | 0.54 s | 106 KB |
| 7 days | 1.46–1.48 s | 1.25–1.26 s | 1.81 MB |
| 12 days | 1.96–1.98 s | 1.41 s | 3.83 MB |

Over GitHub, 12 days behind: 2.7–2.9 s against 3.5–5.3 s; `emerge-webrsync` takes 8.28 s and
50 MB for one day. `compact()` is 40% of the 1-day sync, yet does not stop growth: it frees pages
but not leaf fragmentation, and 14 daily syncs took the file from 148 to 178 MiB. A key-order
rewrite (1.2 s) brought it back to 142.5 MiB.

### Size on disk

![Size on disk, checkout against redb](./img/tree-storage/redb-disk-size.svg)

| | checkout files | `.git` (depth 1) | redb tree + md5-cache |
|---|---|---|---|
| Plain filesystem | 593 MiB | 96 MiB | 143 + 31 MiB |
| btrfs with zstd | 112 MiB | not measured | 173 MiB |

redb's objects are already zstd, so on a compressing filesystem only `.git` is saved.

### Reads

![Reading the md5-cache by thread count](./img/tree-storage/redb-md5-cache-read.svg)

| | checkout | redb |
|---|---|---|
| Valid entries for `::gentoo` (`repo_load` bench) | 70 ms | 55 ms |
| `em -p @world`, warm, 15 runs | 934 ± 41 ms | 920 ± 40 ms |
| `em -p @world`, cold page cache, 5 runs | 1.32–1.42 s, 124 MiB read | 1.19–1.25 s, 119 MiB read |
| `em search -S`, warm, 40 runs | 77 ± 12 ms | 80 ± 13 ms |

Most of the md5-cache gain is parallelism, which a directory read gets too. Plans and search
output are identical. `em search -S` used to scan the md5-cache, 84 against 107 ms on redb; it now
reads `metadata/pkg_desc_index`, as emerge does.

### Ebuilds no cache covers

![Sourcing the ebuilds no cache covers](./img/tree-storage/uncached-sourcing.svg)

| Case | before | after |
|---|---|---|
| Overlay without md5-cache, 3,909 ebuilds, cold | 12.75 ± 0.08 s | 1.50 ± 0.02 s |
| redb, cache database a sync behind, 2,751 ebuilds, cold | 10.34 ± 0.06 s | 1.60 ± 0.01 s |
| redb, no cache database, 33,977 ebuilds, cold | 73.1 s | 7.64 ± 0.06 s |
| Same, entries in em's cache | 2.58 ± 0.01 s | 0.73 ± 0.03 s |
| `em regen` of the checkout, for reference | 7.06 s | 7.06 s |

Not redb's fault: ebuilds without a valid entry were sourced one at a time on every backend. They
now go to the worker pool `em regen` uses.

### Cost on the default path

`em -p @world` without the `redb` feature: 851 ± 10 ms on master, 855 ± 9 ms on the branch.

## What we learned

1. **Sync is where a database pays.** One thin pack and one transaction, no worktree to repair;
   the tree is one verified commit, so its entries need no freshness check.
2. **Size depends on the filesystem.** A quarter of an uncompressed checkout; on btrfs with zstd
   only `.git` is saved.
3. **Parallel reads matter more than the container.** Cold, redb is 10% faster; warm, equal.
4. **The tree and the metadata cache live different lives.** Two files: the cache may lag, run
   ahead or be missing, and the shared gap filling covers it.
5. **A build needs one real directory, not a checkout.** A first attempt copied `eclass/` and
   opened the copy as the repository; a traced build showed only the package directory is read
   from disk, and the copy had hidden `profiles/thirdpartymirrors`.
6. **Measure on matching inputs.** A cold figure of 99 MiB came from a build still reading the
   checkout, and a tree from another commit made an overlay's cache look stale on every run.
7. **redb's readers and writer exclude each other.** Both lock the file at open; the one that
   comes second fails with an error naming the other.
8. **Reviews find what the author stops seeing:** pruning a copy another build used, the sandbox
   missing the databases, a corrupt database falling back to a stale checkout, an unchanged remote
   skipping the signature check, symlinks read as their target's name, a lagging cache no sync
   repaired, `em clean` treating a redb repository's distfiles as unreferenced.

## Open

The layout, compression, vacuum and concurrency work left is listed in
[`tree-storage.md`](./tree-storage.md#open).
