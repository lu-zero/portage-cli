# Real hard dependency cycle: zstd ↔ meson ↔ python — resolved

Found live-testing `em --local toolchain --setup` on real Debian 12. Not an
`em` bug — a genuine, irreducible hard-dependency cycle in `::gentoo`:

```
app-arch/zstd  --BDEPEND-->  dev-build/meson   (meson.eclass: >=dev-build/meson-1.2.3)
dev-build/meson --DEPEND-->  dev-lang/python   (meson is a Python program)
dev-lang/python --DEPEND-->  app-arch/zstd     (python-3.14.7.ebuild: `!build? ( app-arch/zstd:= )`)
```

Confirmed via direct instrumentation of `portage-atom-pubgrub`'s
`install_order`: `meson`/`zstd` share the same *hard* SCC, so the
cross-group-hard-predecessor guarantee doesn't apply and the tie-break
picks `zstd` before `meson` — same mechanism already documented for
`xz-utils ↔ elt-patches`.

## Why it only surfaced under `--local`

A real host, `--root`, or `--prefix` already has *some* version of the
cycle members installed, satisfying the edge regardless of build order —
how real bootstrap cycles get broken in practice. `--local` starts from a
truly empty VDB, so `preflight::check` correctly reports it as genuinely
unsatisfiable.

## Actual root cause: a missing host tool, not `toolchain_plan`

This cycle surfaced *after* `toolchain_plan` already gained a real
`dev-lang/python` merge step (a separate, earlier fix in the same
investigation: `python.eclass`'s own VDB check needs a real install, not
just a `package.provided` claim — see that commit). Once python was real,
the remaining `zstd ↔ meson` edge was still the open question here.

`dev-build/meson` was already in `package.provided`'s TIER1 list
(`setup/provided.rs`) with a `meson --version` host probe — the designed
mechanism for exactly this. The Debian 12 test container simply had no
host `meson` installed, so the probe correctly (silently) found nothing
and omitted the entry, per TIER1's "leave it out, don't invent" rule.

Installing `meson` on the container host and re-running `em setup --local`
made the whole `toolchain --setup` plan resolve cleanly end to end — no
*further* code change needed. Confirmed live.

Two USE-override fixes to `toolchain_plan` were attempted and reverted
before finding this (scoped `USE=build` on the python step, then a
blanket `-*`/`BOOTSTRAP_USE` widening) — both treated the symptom, and the
blanket version was also independently unsafe: it would have stripped
`cxx` from the final native `gcc` step, which has no stage2 rebuild.

## What actually landed

A hard-cycle *detection* feature (`DepgraphOutcome::hard_cycle_edges`,
`preflight::check`'s new parameter): when a preflight failure is caused by
a genuine irreducible hard cycle, the message now says so distinctly
instead of the generic "needs: X" — useful for *any* future case like this
one, not just this specific triangle.
