# Parse-failure tracking + Lazy<T> extraction: confirm no regression, 2026-08-21

- **baseline (anchor):** `c0d64e3` (previous anchor — lazy DEPEND-family parsing, 2026-08-21)
- **current:** `855b418` (HEAD after this session's remaining work)
- **machine:** thalia (AmpereOne, 128 cores, 4 NUMA nodes, 256 GiB) — see
  [`machines/thalia.md`](../../machines/thalia.md).
- **why:** five commits landed on top of the `c0d64e3` anchor this
  session (`fb7121c` lazy SRC_URI + eclass interning, `2f64914` the
  `Lazy<T>` primitive extraction, `290ce4c` the `is_empty_raw`→`is_empty`
  serialize fix, `db3ed9b` malformed-vs-empty parse-failure tracking,
  `855b418` closing the resolve-time re-source path's own gap in that
  tracking). None of it was perf-motivated — it's a correctness/design
  arc (two rounds of Opus design review plus an adversarial third-model
  review) — but `has_parse_failure()` forces all six lazy fields for any
  entry it's applied to, so it's worth confirming it didn't quietly
  reopen the cost `c0d64e3` closed. It shouldn't have: `has_parse_failure()`
  is only wired into `build_entry` (regen's write path, which forces
  every field anyway) and `resolve_ebuilds`'s suspect/gap path (already
  a small subset of a normal resolve, already paying real I/O).

Methodology: worktree-built anchor (`~/Sources/portage-cli-anchor-c0d64e3`,
sibling of `pkgcraft`/`brush`), every `-p` hyperfine comparison
interleaved-by-command within one invocation (`--warmup 2 -N -i`, 20
runs), `pgrep`/`ps` checked for a live build before every measurement,
load average 0.81 at the start of the run.

## Result: flat, as expected

### 1. `em -p` — the four standard targets

| target | anchor `c0d64e3` | current `855b418` | ratio |
|---|---|---|---|
| `firefox` | 1.214 s ± 0.126 | 1.172 s ± 0.127 | 1.04x (within noise) |
| `qtbase` | 1.246 s ± 0.170 | 1.186 s ± 0.155 | 1.05x (within noise) |
| `texlive` | 1.180 s ± 0.119 | 1.171 s ± 0.142 | 1.01x (flat) |
| `@world` | 1.376 s ± 0.138 | 1.436 s ± 0.113 | 0.96x (within noise) |

All four ranges overlap heavily — none of the four differences clears
the noise floor this project's benchmarks have repeatedly found (deltas
under ~5% are unresolved on this machine). No real movement either
direction, which is exactly what was expected: nothing in this tail
touches the bulk resolve-time read path.

Full raw output: [`hyperfine-interleaved.txt`](hyperfine-interleaved.txt).

### 2. `em regen` — flat, output byte-identical

5 runs each, real `/var/db/repos/gentoo` (32800 ebuilds), `-j20`, into
separate `-o` output directories:

| | anchor `c0d64e3` | current `855b418` | ratio |
|---|---|---|---|
| wall | 9.440 s ± 0.404 | 9.485 s ± 0.806 | 1.00 ± 0.10x |

Output verified byte-identical (`diff -rq`, all 32800 files, exit 0) —
`build_entry`'s new `has_parse_failure()` gate never actually rejects
anything in a real, well-formed tree, so it doesn't change what gets
written; it only changes behavior on the (unexercised, on this tree)
malformed-field path.

Full raw output: [`hyperfine-regen.txt`](hyperfine-regen.txt).

## Conclusion

No regression from `c0d64e3`. `855b418` remains the current state;
`c0d64e3` stays the anchor going forward since this run found nothing to
update it to (no commit here moved wall-clock outside noise in either
direction).
