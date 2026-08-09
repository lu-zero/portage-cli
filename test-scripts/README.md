# test-scripts/

Live, privileged, "layer 5" regression tests — see
[`docs/testing.md`](../docs/design/testing.md#5-manual--privileged-live-testing-not-automatable-in-ci)
for where these fit relative to unit tests / CI. Nothing here runs in CI: it
needs `sudo` (chroot, mount), a real network fetch for distfiles, and a
[`crossdev-stages`](https://github.com/lu-zero/crossdev-stages) checkout
(`~/Sources/crossdev-stages` by default — sibling to this repo). Run these
by hand before/after a change that touches build execution, privilege
handling, root/prefix mapping, or binpkg reuse — not as a gate that runs
every time.

Every script here builds `em` itself first (release), then drives it via
`sudo chroot` inside a **fresh** `crossdev-stages` sandbox — never a reused
one. A stale sandbox left over from a previous run has repeatedly caused
confusing, unrelated failures (see "known crossdev-stages gotchas" below),
so every script destroys-then-recreates its sandbox at the start and tears
it down (mounts + directory) on exit.

## Scripts

### `regression-matrix.sh`

Root-topology regression matrix for the toolchain/crossdev/stages bootstrap
paths (`docs/root-topology.md`'s three modes: bare, `--root`, `--prefix`,
`--local`). Exists because "`-p`/pretend passes" was not enough to catch
real bugs that only manifest during an actual build (installed-view VDB
sharing, a CPPFLAGS-injecting bashrc) — quick mode still runs the fast `-p`
checks (catches solver/ordering regressions); `--full` also runs real
builds for the toolchain-bootstrap matrix.

```sh
./test-scripts/regression-matrix.sh              # quick: -p checks only
./test-scripts/regression-matrix.sh --full        # + real stage1/crossdev builds
./test-scripts/regression-matrix.sh --full --jobs 16
```

Env vars: `CROSSDEV_STAGES_DIR` (default `~/Sources/crossdev-stages`),
`SANDBOX` (default `em-regression`), `CROSS_TARGET` (default
`riscv64-unknown-linux-gnu`).

### `test-binpkg-identity-sandbox.sh`

Real-chroot regression test for the binpkg identity model
(`todo/binpkg-subtargets.md`): CHOST + `build_env_key`, multi-instance
PKGDIR, the asymmetric empty-key reuse gate, `em maint binpkg
list`/`fingerprint`/`prune`, and the S6 package.env fold. Builds
`sys-libs/zlib` twice under two different `-march` values into the same
PKGDIR (producing two distinct `build_env_key` instances for the same CPV —
the exact "same CHOST, different micro-arch" scenario the feature exists to
disambiguate), then checks `list`/`fingerprint`/`prune` against them for
real, and proves a real `-e -k` merge reuses the matching variant and
rebuilds under a non-matching one (not via `-p`'s preview tag — see the
script's own comment on why: `-p`'s binary/ebuild tag is a documented
simplification that never reflects the real per-key gate).

```sh
./test-scripts/test-binpkg-identity-sandbox.sh
./test-scripts/test-binpkg-identity-sandbox.sh --keep   # leave the sandbox for manual poking
./test-scripts/test-binpkg-identity-sandbox.sh --sandbox my-name
```

Exits non-zero if any check fails (each check prints `PASS:`/`FAIL:`, plus
`ALL CHECKS PASSED` / `SOME CHECKS FAILED` at the end).

### `test-crossdev-flavours.sh`

Two checks, not a single matrix: (1) `regression-matrix.sh`'s existing
crossdev/toolchain matrix (bare/`--root`/`--prefix`/`--local` × `crossdev
--setup`, real cross-toolchain builds, not just `-p`) run once more for
`riscv64-unknown-linux-gnu` (a thin wrapper — regression-matrix.sh already
does the real work); (2) a fast, standalone check that
`aarch64-unknown-linux-gnu` (identical to this machine's own host CHOST) is
correctly *rejected* by `em crossdev --setup` instead of attempted. That
second case isn't a real cross target: `cross-*/linux-headers` is the real
upstream ebuild, symlinked in, and it decides its own install path by
checking `CTARGET != CHOST` — a same-arch tuple makes it install straight
into `/usr/include`, colliding with the native package already there (real
crossdev has the identical limitation). Found live 2026-07-20 running this
script (980 file collisions); `em crossdev --setup` now rejects it up
front (`reject_same_arch_target` in `crossdev/mod.rs`), so the useful check
here is "fails fast with the right message", not "the multi-minute
bootstrap succeeds".

```sh
./test-scripts/test-crossdev-flavours.sh          # riscv64 quick (-p only) + aarch64 fast-reject check
./test-scripts/test-crossdev-flavours.sh --full    # riscv64 + real stage1 builds, same aarch64 check
```

### `test-crossdev-binpkg-sandbox.sh`

Real cross-compiled binpkg regression test for the host/target CHOST split
(`todo/binpkg-subtargets.md`'s S1/S4: per-entry PKGDIR dual index for cross
host BDEPEND reuse) — the one live-verification gap
`test-binpkg-identity-sandbox.sh` doesn't cover (that script only varies
`-march` under one CHOST; this one actually cross-*builds* for two
different target tuples). For each of `riscv64-unknown-linux-gnu` and
`x86_64-pc-linux-gnu` (both genuinely cross from this aarch64 host — no
qemu needed, cross-compiling never executes target-arch code): bootstraps
the cross toolchain (`crossdev --setup`), cross-builds `sys-libs/zlib` for
real, and checks the resulting binpkg's recorded CHOST is the *target*'s
(not the host's), that `fingerprint --host` correctly reports the real
host CHOST instead, and that each target's PKGDIR is genuinely isolated
(no cross-contamination between targets or with the host's own PKGDIR).
`aarch64-unknown-linux-gnu` is deliberately not one of the two targets —
see `test-crossdev-flavours.sh` for why (same-arch as host, correctly
rejected before any build is attempted). Slower still than
`test-crossdev-flavours.sh`: two full cross-toolchain bootstraps *and* two
real cross package builds.

```sh
./test-scripts/test-crossdev-binpkg-sandbox.sh
./test-scripts/test-crossdev-binpkg-sandbox.sh --keep
```

## Known `crossdev-stages` gotchas

- **Never hand-patch, bind-mount, or `sudo chroot` into an *existing*
  sandbox from a previous run.** Always `sandbox setup` (or let a script do
  it) a fresh one — a 2026-07-12 incident traced confusing, irreproducible
  failures back to state left over in a reused sandbox.
- **`sandbox destroy` runs unprivileged.** A `sudo chroot` build leaves
  root-owned files under e.g. `var/tmp/portage/*/work/` that an unprivileged
  `rm -rf` can't remove — `destroy` silently leaves a half-removed
  directory behind, and the tool's own sandbox registry then gets out of
  sync with it (`sandbox setup --name X` sees a stale "already exists"
  registry entry and skips unpacking, then fails with "sandbox not found"
  when it tries to actually use the now-missing directory). Found
  2026-07-20 while writing `test-binpkg-identity-sandbox.sh`. Fix: `sudo rm
  -rf` the sandbox directory yourself before calling `sandbox destroy` —
  both scripts here already do this; do the same in any new script.
- **`--dry-run` on `sandbox setup` is not actually a dry run** (observed
  2026-07-12) — it unpacks the real stage3 anyway. Don't rely on it to
  preview without side effects.
- `crossdev-stages sandbox destroy NAME` takes `NAME` as a positional
  argument, not `--name NAME` (only `sandbox setup` takes `--name`).

## Adding a new script

- Compute `REPO_ROOT` as one level up from the script's own directory
  (`SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"; REPO_ROOT="$(cd
  "$SCRIPT_DIR/.." && pwd)"`) — everything here lives in `test-scripts/`,
  not the repo root, so don't assume `SCRIPT_DIR` is the workspace root.
- Build `em` (or whatever binary) from `$REPO_ROOT`, not the script's own
  directory.
- Destroy-then-recreate your sandbox at the start (never reuse), and tear
  down mounts + `sudo rm -rf` the sandbox directory in a `trap ... EXIT` so
  a failed run doesn't leave anything behind.
- Wire the standard trio a bare stage3 is missing: the real repo tree
  (`mount --bind "$REPO_ROOT/portage-repo/gentoo" ...`), `/etc/resolv.conf`
  (DNS for distfile fetches), and a distfiles cache bind (avoids needing
  network for cache hits) — see either existing script for the exact mount
  set (`proc`/`dev`/`sys` too).
