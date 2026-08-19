# USE defaults/conf split: `em -p` wall-clock, 2026-08-19

- **baseline (anchor):** `deb4c81` — `em_before`
- **current:** uncommitted `use-defaults-package-use` worktree
  (`/home/lu_zero/Sources/portage-cli-use-fold`) — `em_after`
- **machine:** thalia (AmpereOne, 128 cores, 4 NUMA, 256 GiB) — see
  [`machines/thalia.md`](../../machines/thalia.md)
- **why:** the fold now walks profile `package.use` per CPV between
  make.defaults and make.conf. Confirm that did not move `em -p` wall time.

Binaries (gitignored; hashes in `sha256.txt`):

```
benchmarks/bench-results/20260819-use-fold-defaults-conf-deb4c81/em_{before,after}
```

Same copies under the main checkout's `benchmarks/bench-results/` so they
survive if the worktree is removed.

## How before was taken

**Not** the docs recipe (`git stash && cargo build --release`). Before is
the main checkout's existing `target/release/em`:

- mtime **2026-08-19 00:11**
- size 29084376
- sha256 `c43dbacd4695b7aefac91ecacaa91ab6bdbf456f9325e880cbfff285532a4427`
- tree SHA `deb4c81` (same commit as the worktree HEAD)

After was a `--release` build of the worktree with
`CARGO_TARGET_DIR=/home/lu_zero/Sources/portage-cli/target` (finished
2026-08-19 18:47, size 28844216, sha256
`c6c0a9b0e83a4eed995190c868b1c8fb458e4cb60d4551b45cefc8642cbd402d`).

Different LTO sessions, hours apart. Main tree also had in-progress
`portage-atom` comment unslop (text-only). Treat this as an environment
anchor plus a noise-level check, not a same-session delta.

## Result: no measurable `em -p` cost

Firefox plan: 74 `[ebuild]` lines, byte-identical between the two
binaries.

Interleaved `hyperfine --ignore-failure --warmup 2 -m 10` (raw:
`hyperfine-raw.txt`):

| target | before `deb4c81` | after (worktree) | ratio |
|---|---|---|---|
| `www-client/firefox` | 1.435 s ± 0.107 | 1.560 s ± 0.148 | before 1.09 ± 0.13× |
| `app-office/libreoffice` | 1.646 s ± 0.074 | 1.667 s ± 0.137 | before 1.01 ± 0.09× |
| `dev-qt/qtwebengine` | 1.528 s ± 0.152 | 1.486 s ± 0.091 | after 1.03 ± 0.12× |

Every error bar spans 1.0. Wall is still ~1.5 s with ~2.5 s user /
20–30 s system (parallel I/O), same shape as before.

**New `em -p` wall-clock anchor:** `deb4c81` / `em_before` as hashed
above. Future comparisons should rebuild that commit in-session if they
need a tighter delta; this blob is the recorded baseline from this run.
