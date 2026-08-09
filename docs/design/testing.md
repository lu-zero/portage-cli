# Testing strategy

How correctness is actually established in this workspace: what runs in CI,
what has to be run by hand, and — the recurring lesson of this project —
why unit tests alone have repeatedly missed real bugs that a direct
comparison against real `emerge` caught immediately.

## The layers

### 1. Unit tests (`#[cfg(test)] mod tests`, in-module)

The bulk of the suite (~1200 tests workspace-wide). Per `AGENTS.md`, tests
live next to the code they cover, not in a separate file. Rough weight by
crate (test-function count, not a quality signal, just where the mass is):
`portage-repo` (197), `portage-cli` (193), `portage-metadata` (161),
`portage-atom` (146), `portage-atom-pubgrub` (141), `portage-atom-resolvo`
(82). This is where the USE-flag/solver-boundary logic
(`docs/architecture.md`'s "USE/solver boundary" section), atom parsing, and
CLI plumbing get their fast, deterministic coverage.

Run: `cargo test --workspace --exclude portage-bench` (matches CI exactly —
see below). That command runs **unit tests, integration tests, and
doctests**. `cargo nextest` runs unit/integration only — pair it with
`cargo test --workspace --exclude portage-bench --doc` when iterating with
nextest so doctest-only breakage does not surprise CI.

### 2. Crate-level integration tests (`tests/*.rs`)

- `portage-repo/tests/ver_functions.rs` + `ver_functions.sh` — ported
  directly from Gentoo's own `eclass/tests/version-funcs.sh`, exercised
  against the real `EbuildShell` (brush-backed). A golden-file test: the
  fixture *is* upstream's own test data, not something invented here.
- `portage-repo/tests/brush_compat.rs` — bash constructs commonly found in
  real Gentoo eclasses (not synthetic snippets), because `brush`
  compatibility gaps only show up on real eclass code, not on
  what you'd guess to write by hand.
- `portage-binpkg/tests/write.rs` — GPKG binary package round-trip.
- `portage-cli/tests/comparison.rs` — `em query` vs `qfile`/`qlist`/`qsize`/
  `equery` on a **live Gentoo host**. All five tests are `#[ignore]` by
  default (no such host in CI); run explicitly with
  `cargo test -p portage-cli -- --ignored` when you have one.

### 3. `cargo nextest` — use it locally, even though CI doesn't

CI (`.github/workflows/ci.yml`) runs plain `cargo test --workspace --exclude
portage-bench`. Locally, **prefer `cargo nextest run --workspace --exclude
portage-bench`** — `portage-repo`'s shell/profile tests call
`std::env::set_current_dir` (process-global) and race under `cargo test`'s
default parallel-thread execution, producing sporadic unrelated failures
(one triage found 28 failures in a single run: 24 an unrelated `--jobs` race,
1 a real bug, 3 pre-existing flakes from this same root cause). nextest's process-per-test
isolation sidesteps it. **If a plain `cargo test` run shows scattered
failures in `build::shell`/`build::profile` tests that don't relate to your
change, re-run under nextest before concluding you have a regression** —
this has cost real debugging time more than once this project.

This flakiness has never been root-fixed (the `set_current_dir` call would
need to move off a process-global path, or tests needing it would need
`#[serial]`-style exclusion — neither is done). It's a known gap, not a
mystery to re-investigate each time.

### 4. Live parity against real `emerge` — the strongest oracle this project has

`benchmarks/bench-em-vs-emerge.sh` (see `docs/benchmarks.md`) resolves a
fixed basket of real-world targets (qtbase, texlive-core, firefox,
qtwebengine, thunderbird, libreoffice, qemu, a crossdev target) with both
`em -p` and real `emerge -p` and diffs the package sets. `SKIP_TIMING=1`
runs the parity check alone in seconds.

This is not a "nice to have" — it is how most of the real, non-obvious bugs
in this project have actually been found, because they were invisible to
unit tests that only exercise what the author already thought to test:

- The `--root` config-root parity bug, the `is_cross_arch` false-positive,
  Choice/SlotChoice host-satisfaction gap, and the `initial_depend`
  target-VDB-weave regression (all found and fixed 2026-07-12) were caught
  by noticing `em --root <dir> gcc -vp` didn't match `ROOT=<dir> emerge -vp
  gcc` package-for-package — not by any failing unit test.
- The `USE=-*` wildcard-reset bug (same day) was found because a *user*
  ran `USE="-* build" ROOT=X emerge -vp curl` and it showed `-quic` where
  `em` showed `quic` enabled — disproving an earlier, plausible-sounding
  but wrong internal explanation.
- The `ForceMask`/`UseConfig` perf fix incidentally uncovered a phantom-package
  bug (`virtual/libintl`/`virtual/libiconv` appearing in `em`'s
  `sys-devel/gcc` plan when real emerge's didn't) purely because the parity
  diff was re-run as part of verifying the perf change, not because
  anything was looking for it.

**Practical rule** (see the `live-verify-full-pretend-output` lesson):
when live-verifying a fix, read the **entire** `-p`/`-v` output, not just
the target atom or the lines you expect to have changed — display
formatting and merge-destination routing are separate code paths from the
resolve logic, and a fix in one has repeatedly left the other silently
wrong (`output.rs`'s `format_flags`, `required_use.rs`, `download_size.rs`
each needed their own copy of a fallback fix the solver-level code already
had, discovered only by reading full `-vp` output line by line).

### 5. Manual / privileged live testing (not automatable in CI)

For the full **toolchain → stage1 → stage3** flow (native and cross) and how
to drive it through a crossdev-stages sandbox, see the draft
[`stages-and-testing.md`](../user/stages-and-testing.md). Below is the shorter
sandbox recipe used by `test-scripts/`.

Some of this project's surface can only be verified by actually building
real packages under real privilege boundaries — no unit test substitutes
for it:

- **Chroot testing**: copy the release `em` binary into a real stage3
  chroot and `sudo chroot` in, rather than driving a host `em` against
  `--root <chroot>` — the latter doesn't exercise the same privilege/
  environment boundaries a real build needs.
- **`crossdev-stages` sandbox** for a clean, disposable, from-scratch stage3
  — see the recipe below.
- **`--local`/`--prefix` wall-testing**: building a real, complete
  dependency closure (e.g. the full native+cross toolchain bootstrap, or
  the firefox closure) end-to-end in a throwaway root is how privilege
  scoping, VDB write correctness, and environment-handoff bugs
  (`__worker` scoping, `mark_phase_sourced`, EPREFIX propagation) actually
  got found — these are integration failures that only manifest under a
  full real build, not a resolve-only `-p`.

These aren't scripted or repeatable in the way `bench-em-vs-emerge.sh` is;
treat them as exploratory/regression sweeps to run before/after a change
that touches build execution, privilege handling, or root/prefix mapping,
not as a gate that runs every time.

#### `crossdev-stages` sandbox recipe

See [`test-scripts/`](../../test-scripts/README.md) for ready-to-run,
self-contained scripts that automate this recipe end-to-end (build `em`,
fresh sandbox, wire mounts, drive scenarios, tear down) — read on for the
manual step-by-step this recipe follows, or if you're writing a new script.

`../crossdev-stages` (`~/Sources/crossdev-stages`, a sibling Rust project)
can spin up a clean, disposable stage3 rootfs in seconds — much faster than
hand-rolling one, and it doesn't carry state from a previous test run the
way a reused `/tmp`/`/var/tmp` scratch root can. **It drives real
`emerge`/`{tuple}-emerge` internally for its own `sandbox crossdev`/
`target stage1` commands — those are not used here.** Only `sandbox setup`
(and optionally `sandbox prepare`) are used, to get a rootfs; `em` is then
copied in and driven directly via `sudo chroot`, exactly like the plain
chroot-testing recipe above, just with a faster/cleaner way to obtain the
rootfs.

```sh
cd ~/Sources/crossdev-stages

# One sandbox per scenario under test — cheap, don't share unless the
# scenarios are guaranteed not to interact (e.g. two different --prefix
# subdirs in the same chroot are fine; a from-scratch --root bootstrap and
# a --local bootstrap probably shouldn't share one, to keep each run's
# findings unambiguous).
./target/release/crossdev-stages sandbox setup --arch aarch64 --name em-test-1
# NOTE: --dry-run on `sandbox setup` is not actually a dry run (observed
# 2026-07-12) — it unpacks the real stage3 anyway. Don't rely on it to
# preview without side effects.

cd /home/lu_zero/Sources/portage-cli
cargo build --release -p portage-cli
SB=~/.cache/crossdev-stages/sandboxes/em-test-1
sudo mkdir -p "$SB/usr/local/bin"
sudo cp target/release/em "$SB/usr/local/bin/em"

# The bare stage3 has no repo tree, no /etc/resolv.conf (breaks distfile
# fetch DNS), and no distfiles cache — wire all three in:
sudo mkdir -p "$SB/var/db/repos/gentoo" "$SB/proc" "$SB/dev" "$SB/sys" "$SB/var/cache/distfiles"
sudo mount --bind "$(pwd)/portage-repo/gentoo" "$SB/var/db/repos/gentoo"   # real tree + md5-cache, already checked out
sudo mount --bind /proc "$SB/proc"
sudo mount --rbind /dev "$SB/dev"
sudo mount --bind /sys "$SB/sys"
sudo cp /etc/resolv.conf "$SB/etc/resolv.conf"
sudo mount --bind /var/cache/distfiles "$SB/var/cache/distfiles"   # cache hits avoid needing network for most fetches

sudo chroot "$SB" /usr/local/bin/em --help   # sanity check before anything real
```

From there, drive scenarios exactly as documented in
`docs/root-model.md`: e.g.
`sudo chroot "$SB" /usr/local/bin/em toolchain --setup --root /root/x -p`
first (fast, catches resolution regressions), then the real (non-`-p`) run.

This exact recipe (2026-07-12, four sandboxes: native `--root`, `--prefix`,
`--local`, cross riscv64) is what surfaced two real, previously-unknown
bugs in a single session — `cede_required_use`'s early return silently
skipping Level-C autosolve for already-installed packages under `--prefix`,
and `--local`'s preflight BDEPEND check not recognizing `PATH`-found host
tools — both invisible to `-p`-only testing against a repo tree with
nothing installed, and both requiring a *real* stage3 base (already-populated
VDB) to reproduce.

Clean up when done: `sudo umount "$SB"/{var/cache/distfiles,sys,dev,proc,var/db/repos/gentoo}` (or `umount -R`
for the rbind under `dev`), then `./target/release/crossdev-stages sandbox destroy --name em-test-1`.

## What's *not* here (known gaps)

- **No property-based/fuzz testing** (no `proptest`/`quickcheck` dependency
  anywhere in the workspace, no fuzz targets). The one exception was an
  ad hoc 200k-case Python fuzz comparison written to prove the
  `wildcard_reset` USE-flag representation equivalent to portage's real
  accumulator semantics (2026-07-11 session) — it was never committed, so
  it can't be re-run today. If USE-flag/incremental-variable semantics
  change again, consider writing a similar check as a committed
  `#[test]` (even a small one, comparing against a hand-derived oracle)
  rather than an ephemeral script, so the proof survives past one session.
- **No mutation testing** or coverage-gated merges — `cargo llvm-cov` runs
  in CI (uploads to Codecov) but isn't a required check.

## CI gates (`.github/workflows/ci.yml`)

All run on `stable` and the declared MSRV (currently 1.95):

| Job | Command |
|---|---|
| `test` | `cargo test --workspace --exclude portage-bench` (unit + integration + **doctests**) |
| `msrv` | `cargo msrv verify --rust-version 1.95 --path portage-cli` |
| `clippy` | `cargo clippy --workspace --exclude portage-bench -- -D warnings` |
| `fmt` | `cargo fmt --all -- --check` |
| `bench-smoke` | `cargo check -p portage-bench --benches` (compiles benches, doesn't run them) |
| `coverage` | `cargo llvm-cov --workspace --exclude portage-bench --all-features --lcov` → Codecov |
| `doc` | `cargo doc --workspace --exclude portage-bench --no-deps` (`RUSTDOCFLAGS=-D warnings`) |

Do not treat “nextest green” as CI-green: nextest skips doctests, and the
`doc` job is a separate gate for rustdoc warnings (broken intra-doc links,
etc.).

`portage-bench` is excluded from `test`/`clippy`/`coverage`/`doc` everywhere
(it's a dev-only harness with a pinned `pkgcraft` git dependency that CI
can't always resolve the same way local sibling-worktree overrides do) but
still gets a compile-only smoke check so its benches don't silently rot.

## Bumping brush (routine maintenance)

As common as a formal bench run: the ebuild shell is **brush** from
`github.com/lu-zero/brush`, branch **`for-portage-repo`**, pinned by **git
`rev`** on the three workspace deps `brush-core` / `brush-builtins` /
`brush-parser` in the root `Cargo.toml`. Local day-to-day work uses a
**gitignored** path patch to a sibling checkout.

Expected layout:

```text
Sources/
  portage-cli/     # this workspace
  brush/           # for-portage-repo branch (mine → lu-zero/brush)
```

### Local path patch (dev only — never commit)

`.cargo/config.toml` (gitignored; see `AGENTS.md`):

```toml
[patch."https://github.com/lu-zero/brush.git"]
brush-core = { path = "../brush/brush-core" }
brush-builtins = { path = "../brush/brush-builtins" }
brush-parser = { path = "../brush/brush-parser" }
```

(Keep any pkgcraft path patch alongside if you use one.)

### Recipe after changing `../brush`

1. **Stay on `for-portage-repo`** in `../brush`; finish rebase/fixups there.
2. **With the path patch active**, validate portage-repo (nextest, not plain
   `cargo test` — shell tests race under shared process cwd):
   ```bash
   cargo nextest run -p portage-repo
   cargo nextest run -p portage-repo --test brush_compat   # 39 shell/eclass cases
   ```
   Full crate green (hundreds of tests) is the bar before pinning.
3. **Push the brush branch to `mine`** (rebased history ⇒ force-with-lease is
   expected):
   ```bash
   cd ../brush
   git push --force-with-lease mine for-portage-repo
   git rev-parse --short=8 HEAD    # pin this
   ```
4. **Bump the pin in portage-cli** root `Cargo.toml` (all three `brush-*`
   lines, same `rev`). Temporarily remove the brush path patch (leave
   pkgcraft if needed) and:
   ```bash
   cargo check -p portage-repo     # must resolve git+rev from lu-zero/brush
   cargo nextest run -p portage-repo --test brush_compat
   ```
   `Cargo.lock` is gitignored — only commit `Cargo.toml`.
5. **Commit + push portage-cli**:
   ```text
   chore(deps): bump brush fork to <rev> (for-portage-repo)
   ```
6. **Restore** the local path patch for continued brush work.

### What not to confuse

| Thing | Role |
|-------|------|
| Path patch in `.cargo/config.toml` | Machine-local only; overrides whatever `rev` is in `Cargo.toml` |
| `Cargo.toml` `rev = "…"` | What CI and other clones use; must exist on `mine/for-portage-repo` |
| `92ebb646` vs rebased tip | After a rebase the old rev may **not** be an ancestor of HEAD — always re-pin to the new tip |
| `origin` vs `mine` on brush | Upstream is reubeno/brush; our fork remote is typically `mine` → `lu-zero/brush` |

## Before opening a PR / finishing a session

The sequence this project actually follows (see `AGENTS.md` for the
individual commands):

1. `cargo build` (or `--release` if timing/behaviour matters — debug
   builds of anything touching `brush` are too slow to trust for feel).
2. `cargo nextest run --workspace --exclude portage-bench` (not plain
   `cargo test` for the bulk suite — see above) **plus**
   `cargo test --workspace --exclude portage-bench --doc` (nextest does
   not run doctests; CI’s `test` job does).
3. `cargo clippy --workspace --exclude portage-bench -- -D warnings`.
4. `cargo fmt --all -- --check` (CI enforces this; `clippy` passing is not
   sufficient on its own).
5. `RUSTDOCFLAGS='-D warnings' cargo doc --workspace --exclude portage-bench
   --no-deps` (CI `doc` job).
6. For anything touching resolution/USE/root handling: `SKIP_TIMING=1
   ./benchmarks/bench-em-vs-emerge.sh` at minimum; a targeted live
   `-vp`/`-p` comparison against real `emerge` for the specific case the
   change targets, reading the *entire* output.
7. For anything touching performance-sensitive code: the two-binary
   `hyperfine` recipe in `docs/benchmarks.md`, on a target big enough for
   the effect to clear the noise floor.
7. For anything touching the ebuild shell / `../brush`: the **Bumping
   brush** recipe above (nextest `portage-repo` + pin + push), not a one-off
   `cargo check` against an unpushed worktree.
