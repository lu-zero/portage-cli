# `--jobs`: a backwards RDEPEND edge is silently dropped from `build_blockers`

Status: 🔴 not started, root-caused with strong evidence. Found 2026-08-23
during the first genuine, full, real (non-pretend) `em --local` toolchain
bootstrap run ([[local-bootstrap-provided]], `--jobs 48`).

**Superseded an earlier, wrong diagnosis same day**: originally filed as
"python-exec wrapper gap," assuming the wrapper files themselves were
missing or misplaced. Live inspection disproved that — the prefix's
`sys-apps/gentoo-functions-1.7.7` build failed with
`meson-format-array: no python-exec wrapped executable found in
<EROOT>/usr/lib/python-exec`, but every relevant file was present and
correct: `$P/usr/bin/meson-format-array` (symlink), `$P/usr/lib/
python-exec/python-exec2` (dispatcher), `$P/usr/lib/python-exec/
python3.14/meson-format-array` (the wrapped script itself, shebang
correctly EPREFIX-qualified) all existed exactly as expected. The real
gap: `dev-lang/python-3.14.6_p1` itself — the actual interpreter
`meson-format-array` needs at runtime to dispatch to that wrapped
script — **had not even started building** when `gentoo-functions`
(pulling in `meson-format-array` as a BDEPEND build tool) started and
failed.

## Root cause

`build_blockers` (`portage-cli/src/query/depgraph/mod.rs:1701-1741`)
derives `--jobs` scheduling order purely from DEPEND/BDEPEND/RDEPEND
edges, but only records a blocker when the dependency's linearized plan
index is *numerically earlier* than the dependent's (`to < from`,
line 1732). A `hard` (DEPEND/BDEPEND) edge that ends up backwards
(`to > from`) at least gets recorded into `hard_cycle_edges` for
`preflight` to report (line 1735-1740) — but an **RDEPEND** edge that
ends up backwards is silently dropped with no recording and no warning
at all, since the `hard &&` guard on line 1735 excludes it.

This is exactly what happened: `dev-build/meson-format-array` has a
genuine `RDEPEND="${PYTHON_DEPS}"` (resolves to `dev-lang/python:3.14`
under this run's `PYTHON_TARGETS`). Confirmed via the log's own printed
order for this step's 112-entry plan: `meson-format-array` and
`gentoo-functions` both land around position ~37-39, while
`dev-lang/python` itself lands around position ~79 — *later* — because
`install_order`'s topological placement of `dev-lang/python` is driven
entirely by *its own* build closure (openssl, sqlite, ncurses, gdbm,
readline, libffi, …), which is unrelated to and doesn't require
`meson-format-array`/`gentoo-functions` at all. Nothing in `python`'s own
closure needs those two, so nothing pulls `python`'s position earlier —
meanwhile `meson-format-array`'s *own* build doesn't need a working
python at merge time either (it's just a `python_foreach_impl` file
install), so nothing blocks *it* from finishing early. The RDEPEND edge
from `meson-format-array` to `python` is real and load-bearing (a
python-exec-wrapped script is only actually *runnable* once its RDEPEND
interpreter exists), but `install_order` has no reason to treat it as an
ordering constraint the way a DEPEND/BDEPEND edge is — so it ends up
scheduled backwards, and the backwards-edge check that would have caught
this for a hard edge doesn't apply to RDEPEND at all.

## Why this is a real, general class of bug, not a one-off

Any BDEPEND on a build tool that is itself a `python_r1`/`python-exec`
(or similarly wrapper-based) package is exposed to this: the tool's own
*merge* doesn't need its RDEPEND interpreter, but *using* the tool
(which is exactly what a BDEPEND consumer does) does. `install_order`
currently has no signal that "BDEPEND on T" implicitly also needs "T's
own RDEPEND, ready" as a build-order constraint — it only tracks T's own
direct DEPEND/BDEPEND for T's *build*, not T's RDEPEND for T's *use*.

## Reproduction

```sh
em --local DIR setup
em --local DIR toolchain --setup --jobs 48
# python step: meson-format-array + gentoo-functions complete/attempt
# before dev-lang/python itself has even started building
```

## How to attack

Two shapes, not yet decided between:

1. **Narrow**: when a BDEPEND target `T` is chosen, also derive a
   blocker from `T`'s own RDEPEND closure (or at least the direct RDEPEND
   edges), not just `T` itself — i.e. treat "needs T usable" as
   transitively requiring T's own runtime deps, mirroring how BDEPEND
   already means "needs to actually run this."
2. **General**: extend the existing backwards-edge detection
   (`hard_cycle_edges`) to also cover RDEPEND, at minimum as a warning —
   real portage's own scheduler presumably handles this class correctly;
   worth checking `_emerge/Scheduler.py`'s own dependency-graph ordering
   for how it avoids this before picking a fix shape.

Either way: confirm this doesn't regress the documented reason RDEPEND
is currently treated loosely for ordering purposes elsewhere (soft-cycle
tie-breaking, `install_order`'s Tarjan SCC linearization) — see
`install-order-scc-tiebreak-fix` in memory and the soft-order history in
`todo/done/for-sonnet.md`.
