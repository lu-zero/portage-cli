# `crossdev --setup -p` can't preview a never-initialized target

Status: 🔴 not started. Found 2026-08-29, same session as
[[crossdev-init-target-stale-flag-hint]].

`em --target riscv64-unknown-linux-gnu crossdev --setup -p` on a target
that has never been `--init-target`'d (even once, for real) fails at step
1 (`baselayout`):

```
!!! cross target 'riscv64-unknown-linux-gnu' is not set up at /usr/riscv64-unknown-linux-gnu
```

`--setup`'s own doc comment says it "implies `--init-target`", and under
`-p` `init_target` does preview its config writes (the "config changes:"
block prints correctly) — but pretend mode never actually writes
`<sysroot>/etc/portage/make.conf`. The `baselayout` step doesn't run with
`use_outer_eroot` (it installs into the sysroot itself), so
`emerge.rs`'s early sysroot-exists check runs for real and finds nothing
there yet, even though a live (non-`-p`) `--setup` run would have created
it moments earlier in the same invocation.

The real (non-pretend) `--setup` run works fine end-to-end from a cold
target — this is a preview-only gap: you cannot `-p` a combined
init+bootstrap in one shot on a target that has *never* been initialized
for real, only on one that already has been at least once.

**Fix direction**: not yet designed. One option is having the sysroot-exists
check in `emerge.rs` accept an in-memory "this step's own prior steps in
this same run already covered it" signal, similar to how `setup()` already
passes an in-memory `pretend_alias` for the repo config under `-p`
(`crossdev/mod.rs:282-290`) instead of relying on a written repos.conf.
