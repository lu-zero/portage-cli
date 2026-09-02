# `regression-matrix.sh --full` run, 2026-09-02

Status: 🟢 run complete, findings filed. First full matrix of the session,
sandbox `em-regression` (fresh), `--jobs 12`, `CROSS_TARGET=riscv64-unknown-linux-gnu`.

## Results

```
stages --stage1 --root (real)     PASS   154 packages, ">>> stage1 ready"
crossdev --setup bare             PASS   built through to cross binutils
crossdev --setup --root           PASS   correctly rejected (clap error, EXIT=2)
toolchain --setup --root          FAIL   dev-lang/perl src_configure — transient
toolchain --setup --prefix        FAIL   openrc/baselayout collision
toolchain --setup --local         FAIL   six simultaneous hard cycles
stages --stage1 --prefix (-p+real) FAIL  malformed invocation (script bug)
crossdev --setup --prefix         FAIL   phantom blocker
crossdev --setup --local          INFO   known-partial
```

A correct stage1 **was** produced — via `--root`. Every `--prefix` leg
failed, for three unrelated reasons.

## Filed separately

- [[crossdev-prefix-spurious-os-headers-blocker]] — uninstalled packages
  reported `[ebuild R]`, blocker fires for packages neither installed nor
  planned. Blocks the whole `--prefix` crossdev path.
- [[prefix-baselayout-claims-openrc-run]] — baselayout's VDB entry claims a
  68 KB `obj /sbin/openrc-run` that belongs to `sys-apps/openrc`.

**Worth checking whether those two are one bug.** Both are `--prefix`-only,
both are the installed view disagreeing with what is actually on disk — one
inventing an installed package, the other misattributing an installed file.
Establish that before fixing them as two.

## `--root`'s perl failure is transient, not a defect

`dev-lang/perl-5.44.0` died `src_configure: die: Unable to configure` at
39/76. It is not a real regression, and specifically **not** the phase-stdin
change in `0018023`: perl's VDB entry in that same root is timestamped
`13:57:45`, five minutes *after* the toolchain run ended at `13:52:27` —
`stages --stage1` merged it successfully with the same binary and the same
`/dev/null` stdin. A flake under `-j12`. Rerun to dismiss; do not investigate.

## `--local`'s failure is the known bootstrap cycle cluster

Preflight reports **six** hard cycles at once, not one problem:

```
pax-utils ↔ meson      xz-utils ↔ elt-patches
libxml2   ↔ meson      attr     ↔ gettext
zstd      ↔ meson      binutils ↔ elfutils[debuginfod]
```

**A wrong theory to not re-derive:** `binutils`' `+debuginfod` is *not* the
cause. `stages.rs`'s `binutils_use` already drops `-debuginfod` when
`is_self_contained_bootstrap`, with a comment naming this exact failure — so
it is tempting to conclude `--local` is merely mis-classified there. It is
one of six cycles; fixing that gate alone changes nothing.

This class is already settled: `todo/done/meson-zstd-python-hard-cycle.md`
concluded "not an `em` bug — a genuine, irreducible hard-dependency cycle in
`::gentoo`", and [[local-bootstrap-provided]] records the 11-node cycle as
*expected*. Catalyst does not resolve these either — it sidesteps them with
`USE="-* build"` over a curated `packages.build` list plus
`--implicit-system-deps=n`. So this is the known design gap under
[[local-bootstrap]], not something this run discovered.

## Script defects found by running it

- Both stage1 `--prefix` legs (`-p` and `--full`) pass only `--prefix DIR`,
  which `em stages --stage1` rejects outright — it requires an explicit
  `--root` distinct from the host install path. They cannot pass as written.
  The `--full` leg additionally pairs `--prefix` with the *native toolchain*
  dir rather than a crossdev-populated one; the sequence that would actually
  exercise the prefix path is `crossdev --setup --prefix P` first, then
  `stages --prefix P --root B --target T --stage1` against that same `P`.
- `test-scripts/README.md` still says the scripts drive `em` via
  `sudo chroot`. They do not — `regression-matrix.sh` uses `sandbox run`
  throughout, per the 2026-08-20 rewrite. The prose is stale, the code is
  right.

## None of this session's fixes are implicated

The failures are in resolution, in a merge's file ownership, and in an
upstream cycle. `0018023` (phase stdin), `3b67885` (VDB `-MERGING-`),
`8fddd53` (`has_version` cross-atom roots), `43b3c45` (lock notice) and
`0e943c2` (interrupt) touch none of those paths, and `crossdev --setup`
bare — same binary, same sandbox — resolved and built correctly as the
control.

`8fddd53` remains **unexercised**: the `--prefix` resolve dies upstream of
gcc-stage1, so the cross-atom `has_version` path is never reached.
