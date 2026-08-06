# Test plan — cross-emerge `llvm-core/clang` for riscv64, under `--prefix` and `--local`

STATUS: 🔴 not started — plan only, drafted 2026-08-06. Nothing below has been
run yet; this is the checklist to execute, not a report of results.

## Goal

Confirm `em` can, for our usual cross target (`riscv64-unknown-linux-gnu`,
GCC model), under **both** `--prefix` and `--local`:

1. stand up the topology itself,
2. bootstrap a cross toolchain inside it (`crossdev --setup`),
3. cross-build `llvm-core/clang` through that toolchain (the actual
   "cross-emerge of clang" goal), and
4. do all of that with `-b`/`--buildpkg` and have the resulting binpkg be
   correct and reusable — not just "the merge succeeded".

`--prefix` was already run through steps 2-3 once, informally, on
2026-08-04 (`riscv64-clang-crossbuild-cbuild-esysroot-fixed` memory, fixes in
commit `ad59ed0`) and is now also written up as a worked example in
`docs/crossdev.md`. This plan turns that into a repeatable checklist instead
of a one-off session result, and — the actual new ground — runs the same
shape through `--local`, which has never been tested for this scenario.
`-b` correctness (step 4) has also never been specifically checked for
clang; existing binpkg-identity coverage (`test-scripts/
test-crossdev-binpkg-sandbox.sh`) only exercises `sys-libs/zlib`.

## Target

`riscv64-unknown-linux-gnu`, **GCC** model (the project's standard test
tuple — matches `CROSS_TARGET` in `test-scripts/regression-matrix.sh`). Not
`-L`/LLVM crossdev mode: that mode rejects glibc targets outright, and this
tuple is glibc. So clang is built as an ordinary **target-arch** package
through the GCC cross toolchain (`docs/crossdev.md`'s "two package classes"
— `llvm-core/clang` installs into the sysroot, it is not the host-side
cross-compiler itself), exactly like the 2026-08-04 run.

## Scenario matrix

| # | Scenario | Topology flag | Known state going in |
|---|---|---|---|
| A | `--prefix` overlay | `--prefix DIR` | Proven once, informally, 2026-08-04 (commit `ad59ed0`) |
| B | `--local` standalone prefix | `--local [DIR]` | **Never run for this scenario.** `--local`'s own native toolchain bootstrap has a known genuine hard-cycle partial failure (gdbm/elt-patches/meson/gettext/glibc[cet]/python); whether that blocks *crossdev's* toolchain too is an open question (see Scenario B step 0) |

## 0. Sandbox setup (crossdev-stages)

One fresh sandbox per scenario — never reuse or hand-patch an existing one
(hard rule, `crossdev-stages-sandbox` memory: a reused/broken sandbox has
cost a full bad session before; symptoms look like `em` bugs but aren't).

```sh
cd ~/Sources/crossdev-stages
cargo build --release
./target/release/crossdev-stages sandbox setup --arch aarch64 --name em-clang-prefix
./target/release/crossdev-stages sandbox setup --arch aarch64 --name em-clang-local
```

`sandbox setup` alone is enough — skip `sandbox prepare` (that installs a
whole host-dependency list for the board/image pipeline; irrelevant to
driving our own `em` binary against the shipped repo tree).

Deploy the release `em` binary into each:

```sh
cd ~/Sources/portage-cli
cargo build --release -p portage-cli
for name in em-clang-prefix em-clang-local; do
  cp target/release/em ~/.cache/crossdev-stages/sandboxes/$name/usr/local/bin/em
done
```

Drive everything from here with
`crossdev-stages sandbox run --name <name> "<cmd>"` — never `sudo chroot`
directly into the sandbox (same hard rule; `sandbox run` needs no manual
`proc`/`dev`/`sys` mount management and doesn't leave orphaned mounts behind
on a killed build).

## Scenario A: `--prefix`

**Step 1 — stand up the topology alone (cheap sanity check):**

```sh
em --prefix /root/xp setup -p
em --prefix /root/xp setup
```

**Step 2 — bootstrap the cross toolchain.** No separate `em --target T
setup` pre-step: `crossdev --setup`'s own `init_target` already bootstraps
the outer root itself. (A prior version of this recipe ran `em --target T
--prefix DIR setup` first — with `--target` set that follows the sysroot
substitution and writes the *sysroot's* make.conf instead of the outer
prefix's, and because config writes are `FillGapsOnly`, `crossdev --setup`'s
own later correct write then gets silently skipped since a file already
exists at that path. Confirmed as a real bug this exact recipe hit
2026-07-17 — don't reintroduce it.)

```sh
em -p --prefix /root/xp --target riscv64-unknown-linux-gnu crossdev --setup
em    --prefix /root/xp --target riscv64-unknown-linux-gnu crossdev --setup --jobs N
```

Verify the cross compiler is real and runs:

```sh
/root/xp/usr/bin/riscv64-unknown-linux-gnu-gcc --version
```

**Step 3 — cross-build clang, with `-b`:**

```sh
em -p --prefix /root/xp --target riscv64-unknown-linux-gnu -b llvm-core/clang --jobs N
em    --prefix /root/xp --target riscv64-unknown-linux-gnu -b llvm-core/clang --jobs N
```

**Step 4 — buildpkg verification:** see the shared checklist below.

## Scenario B: `--local`

**Step 0 — recheck whether this is still blocked before doing anything
else.** A 2026-07-12 run found `em --target riscv64-unknown-linux-gnu
--local crossdev --setup` failing at **preflight**, before merging
anything — flagged then as "real gap, not yet root-caused" and never
followed up. A lot has landed since (`cede_required_use` fix, USE-fold
redesign, the `--target`/outer-root `init_target` fix above). Don't assume
either "still broken" or "silently fixed" — run it first and read the
actual preflight output:

```sh
em -p --local --target riscv64-unknown-linux-gnu crossdev --setup
```

- If this now resolves cleanly: proceed directly with steps 1-3 below,
  no workaround needed — and note this as a real, worth-recording fix
  (cross-reference which recent change plausibly explains it).
- If it still fails at preflight: that failure signature *is* the first
  real finding to record (with the actual `needs:` lines, not a guess),
  before falling back to the workaround in step 1.

**Step 1 — topology setup, with the known-cycle workaround if step 0 shows
it's still needed.** `--local`'s own **native** toolchain bootstrap hits a
genuine 11-node hard cycle (gdbm↔elt-patches↔meson↔gettext↔glibc[cet]↔python
— `install-order-scc-tiebreak-fix`/`regression-matrix-script` memories) and
cannot complete from nothing. If crossdev's toolchain bootstrap under
`--local` turns out to depend on that native base existing first (unclear —
that's exactly what step 0 checks), the documented workaround
(`native-prefix-toolchain-bootstrap-fix` memory) is: build a native
toolchain via `--prefix` into some `DIR` first (borrows the host's tools to
break the cycle), then point `--local` at that same, now-populated `DIR` as
its `EPREFIX` so it has something of its own to build from — rather than
starting `--local` from a genuinely empty prefix.

```sh
# only if step 0 shows crossdev --setup --local is still blocked:
em --prefix /root/xl toolchain --setup --autounmask-write --jobs N
em --local /root/xl setup -p
em --local /root/xl setup
```

**Step 2 — bootstrap the cross toolchain** (same shape as Scenario A step 2,
`--local` in place of `--prefix`):

```sh
em -p --local /root/xl --target riscv64-unknown-linux-gnu crossdev --setup
em    --local /root/xl --target riscv64-unknown-linux-gnu crossdev --setup --jobs N
/root/xl/usr/bin/riscv64-unknown-linux-gnu-gcc --version
```

**Step 3 — cross-build clang, with `-b`:**

```sh
em -p --local /root/xl --target riscv64-unknown-linux-gnu -b llvm-core/clang --jobs N
em    --local /root/xl --target riscv64-unknown-linux-gnu -b llvm-core/clang --jobs N
```

**Step 4 — buildpkg verification:** see the shared checklist below.

## Buildpkg verification checklist (run for both scenarios)

Don't stop at "the merge exited 0" — `-p`'s binary/ebuild tag is a
documented simplification that doesn't reflect the real per-key reuse gate
(`test-binpkg-identity-sandbox.sh`'s own noted gotcha), so check the real
mechanisms directly:

- [ ] A binpkg for `llvm-core/clang` exists in the target sysroot's PKGDIR.
- [ ] `em maint binpkg list` / `em maint binpkg fingerprint` (both scenarios,
      after the clang build) show the recorded **CHOST as the target's**
      (`riscv64-unknown-linux-gnu`), not the build host's — same check
      `test-crossdev-binpkg-sandbox.sh` already does for `sys-libs/zlib`,
      now against clang specifically (bigger/slower, worth confirming the
      identity model holds for a toolchain-heavy package too).
- [ ] `em maint binpkg fingerprint --host` still separately reports the
      real build-host CHOST correctly (not swapped/confused with the
      target's).
- [ ] The installed clang binary is genuinely target-arch:
      `file <sysroot>/usr/bin/clang` (or wherever `llvm-core/clang` lands
      inside `<sysroot>`) reports an ELF for RISC-V, not the build host's
      arch.
- [ ] Real reuse, not just the `-p` tag: re-run with
      `-e -k llvm-core/clang` (`-e`/`--emptytree` forces a fresh plan
      decision, `-k`/`--usepkg` allows binpkg reuse) and confirm via the
      build log / timing that it actually reuses the just-built binpkg
      instead of recompiling clang.
- [ ] A rebuild under a genuinely different `build_env_key` (e.g. a
      different `-march` via `CFLAGS`) does **not** wrongly reuse the first
      binpkg — the same asymmetric-key check `test-binpkg-identity-sandbox.sh`
      does for zlib, worth doing once for clang since it's the first
      cross-toolchain-class package this check has been run against.

## Known prior blockers to watch for (don't re-diagnose from scratch)

- `--local`'s native-toolchain hard-cycle partial failure (see Scenario B
  step 1) — expected, already understood, not a new bug if it recurs in
  that specific shape.
- `cede_required_use`'s installed-cpv early-return gap under `--prefix`
  (fixed 2026-08-01, commit `58d335b` — should **not** recur; if it does,
  that's a real regression).
- The `--target`-with-`--prefix`/`--local` `FillGapsOnly` config-clobber bug
  described in Scenario A step 2 — fixed, watch for it resurfacing if the
  recipe accidentally reintroduces the extra `em --target T setup` pre-step.
- `preflight.rs`'s BDEPEND check not recognizing PATH-found host tools under
  `--local` (2026-07-12 finding, `stages --stage1` context — may or may not
  apply to `crossdev --setup`'s own preflight; step 0 above is what
  actually resolves whether it's relevant here).

## Order of execution

1. Scenario A (`--prefix`) first — re-derives already-proven ground, so a
   regression here is the highest-priority finding (something broke what
   used to work).
2. Scenario B step 0 alone (the preflight recheck) — cheap, and its result
   decides whether the rest of B needs the workaround.
3. Scenario B steps 1-4 — new coverage, slowest (a from-scratch cross
   toolchain bootstrap plus a full clang build, possibly preceded by a
   native `--prefix` toolchain build if the workaround is needed).

## Out of scope for this pass

- Actually executing the produced riscv64 clang binary (needs qemu-user;
  not requested here — "use the new toolchain to build something" is
  satisfied by cross-building clang itself, the goal package).
- Fixing anything found — this is a validation pass. Findings get recorded
  here (following `em-stages-scenario-matrix.md`'s numbered-finding
  convention) and triaged afterward.
