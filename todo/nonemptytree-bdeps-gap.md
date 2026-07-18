# Non-emptytree `-p` gap is BDEPEND, not `--deep` traversal

STATUS: built-package BDEPEND fixed 2026-06-18 (commit a56690d). `em -p firefox`
82 -> 128, now includes everything emerge's 125 does (nothing missing). Built
packages always pull BDEPEND (`broot_filtered`); the within-run trim always runs.
`em -pe` still 383; tests pass.

**2026-07-18 note:** the old “em `--deep` traversal gap” chase that motivated
this file is closed for **in-slot `-uD`** — see [[deep-in-slot-upgrades]]. This
doc remains useful as the BDEPEND / with-bdeps history for non-emptytree `-p`.

REMAINING (+1 over-pull, em 126 vs emerge 125):
- ~~libxcrypt/libcrypt abi_x86_32~~ FIXED 2026-06-18 (commit 74d1643): global
  `use.mask` was not forcing flags off over a `+flag` IUSE default, so
  compiler-rt-sanitizers's `+abi_x86_32` stayed on (arm64) and its multilib
  `virtual/libcrypt[abi_x86_32(-)?]` dep pulled 32-bit libcrypt. em -p 128 -> 126.
- ~~`dev-build/cmake` (U, revbump)~~ FIXED 2026-06-18 (`installed-revbump-update-on-prune.md`):
  `em` updated an installed build tool where emerge keeps the satisfied installed
  one. Cause was the `installed_missing_from_repo` set making `Favor` fall through
  to the newest repo version (update-on-prune). The field is removed; under
  `Favor` any satisfying installed cpv is kept even when pruned from the tree.
  `em -p firefox` is now 0 diffs vs emerge.
- Offset `@system` `--root <empty>` — **FIXED 2026-06-19** in a clean
  crossdev-stages stage3 ([[crossdev-stages-sandbox]]). With a truly empty
  target root (`em -p --root /eroot --config-root /` vs
  `PORTAGE_CONFIGROOT=/ emerge --root=/eroot -p`, all `[ebuild N]`, no
  contamination): **em 177 → 180 == emerge 180**. Isolated `curl` repro:
  **em 15 == emerge 15** (was em 12, missing the 3 host build-copies).

  The gap was DEPEND, not BDEPEND: curl's nghttp2 BDEPEND is under `test?`
  (off); `net-libs/{nghttp2,nghttp3,ngtcp2}` are curl's `DEPEND="${RDEPEND}"`
  (`http2`/`http3`/`quic` on). A target package's build edges (`DEPEND`/
  `BDEPEND`/`IDEPEND`) the host (`BROOT == ESYSROOT == /`) lacks must be merged
  to `/` so the target can compile/link against them — emerge lists these
  `to /` alongside the ROOT runtime copy.

  **Fix: post-solve host build-closure walk** (`depgraph/host_copies.rs`).
  After the Target solve (kept single-rooted, pristine), a BFS over the
  finalized Target plan collects each entry's host-unsatisfied build-dep CPNs
  (`bdepend_avail::unsatisfied_cpns` against the host VDB + earlier host
  copies), resolves a version (Target plan's version when shared, else newest
  repo), and emits `MergeRoot::Host` entries — recursing into each copy's own
  build edges, bounded by the host VDB.

  **Why post-solve, not in the solver:** the first attempt routed unsatisfied
  BDEPEND+DEPEND to `MergeRoot::Host` inside `get_dependencies`, but
  `ensure_host_instances` + `package_data_key` alias every Host package to its
  Target `PackageData`, so introducing `pkg@Host` ballooned the Target solve
  (curl 12 → ~120). Giving Host packages independent `PackageData` (true
  dual-root scheduling per `root-model.md`) is the heavier fix the post-solve
  walk defers — it keeps the Target solve unchanged and derives host copies
  against the host VDB afterwards, exactly like `preflight` does.

  **Cost: negligible.** Benchmarked in the stage3 sandbox (release, `time`
  builtin, 5 runs): `@system` offset BASE 1.14s → NEW 1.09s; `@system` native
  (walk is an early-return no-op) 0.79s → 0.81s; `curl` offset 0.65s → 0.66s.
  All within run-to-run noise (±10%); the walk (in-memory BFS over ≤180 pkgs)
  is dwarfed by the ~0.6–1.1s solve.

---


Discovered 2026-06-18 while chasing the "em --deep traversal gap" (sandbox
aarch64, 305 pkgs installed, `www-client/firefox`).

## Counts

| invocation                       | total | note                                  |
|----------------------------------|-------|---------------------------------------|
| `emerge -p` (default)            | 125   | == `emerge --with-bdeps=n`            |
| `emerge -p --with-bdeps=n`       | 125   | required build-deps still included    |
| `emerge -uDp`                    | 131   | +6 = `--deep` slot-bumps              |
| `em -p` (default `with_bdeps=n`) | 82    | **drops 43 required build tools**     |
| `em -p --with-bdeps`             | 128   | includes them, but +3 vs emerge's 125 |
| `em -pe` / `emerge -pe`          | 383   | parity (emptytree forces bdeps on)    |

The 43 em is missing under `-p` are all build tooling: cbindgen, cargo-c, cython,
clang/clang-common, vala, docbook-xml-dtd, xmlto, pybind11, scikit-build-core,
pillow, pygments, glib-utils, gdbus-codegen, … — i.e. firefox's (and its deps')
**BDEPEND**.

## The real divergence

emerge's `--with-bdeps=n` only drops the *optional* BDEPEND of packages that are
**installed and being kept** — it still pulls the BDEPEND of any package being
**built** (cbindgen is strictly required to build firefox). em's
`with_bdeps=false` is coarser: it drops BDEPEND wholesale, including the required
build-deps of newly-built packages, so `em -p firefox` (82) is missing the build
toolchain and would not actually build.

So (as of 2026-06-18 diagnosis):
- This was **not** the full `--deep` in-slot upgrade gap; at the time `--deep`
  mainly meant slot bumps (125 → 131). Slot bump: [[deep-slot-bump]]. Full
  `-uD` in-slot upgrades: [[deep-in-slot-upgrades]] (2026-07-18).
- `em -p --with-bdeps` (128) over-pulls by 3 vs emerge's 125 — a second, smaller
  BDEPEND-set discrepancy (historical; re-verify before treating as open).

## Likely fix direction

Mirror emerge: always include BDEPEND of packages being **built** (`N`/rebuild),
and let `--with-bdeps` govern only the BDEPEND of packages **not** being rebuilt
(installed + kept). This lives in the broot/BDEPEND filtering
(`provider/solve.rs` `broot_filtered` + `bdepend_trim.rs`) and the
`solve_with_bdeps` wiring in `query/depgraph/mod.rs`. Care: the offset/`@system`
BDEPEND parity (182 == `emerge --with-bdeps=n`, see em-root-characterization.md)
was tuned against the current behaviour — re-verify it after.
