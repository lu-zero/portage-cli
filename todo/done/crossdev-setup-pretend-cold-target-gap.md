# `crossdev --setup -p` can't preview a never-initialized target

Status: ✅ fixed 2026-08-29 (`51784c8`). Found same session as
[[crossdev-init-target-stale-flag-hint]].

`em --target riscv64-unknown-linux-gnu crossdev --setup -p` on a target
that has never been `--init-target`'d (even once, for real) failed at
step 1 (`baselayout`):

```
!!! cross target 'riscv64-unknown-linux-gnu' is not set up at /usr/riscv64-unknown-linux-gnu
```

`--setup`'s own doc comment says it "implies `--init-target`", and under
`-p` `init_target` does preview its config writes (the "config changes:"
block prints correctly) — but pretend mode never actually writes
`<sysroot>/etc/portage/make.conf`. The `baselayout` step doesn't run with
`use_outer_eroot` (it installs into the sysroot itself), so
`emerge.rs`'s early sysroot-exists check ran for real and found nothing
there yet, even though a live (non-`-p`) `--setup` run would have created
it moments earlier in the same invocation.

**Fix**: `portage_resolve::use_env::SysrootOverride` — the one real
chokepoint reading sysroot config from disk is `use_env::compute_use_env`
(`canonicalize`s `make.profile`, sources `make.conf` through the ebuild
shell). The profile side needed no virtualization: `make.profile` always
symlinks to a real, already-existing `::gentoo` profile directory, so the
override just points `ProfileStack::build` at that real directory
straight, skipping the not-yet-written symlink. `make.conf`'s content was
already computed in-memory (`make_conf_body`); `portage_repo::ConfSource`
(already used internally for a transient `USE=` override) was made public
so that content sources directly via `ConfSource::Str`, no temp file.

Threaded as an optional field through `DepgraphOpts`/`EmergeOpts`/
`RunStagedOpts`, set only by `crossdev::setup()` under `globals.pretend`,
and only for the `baselayout` step specifically (the one step that
actually targets the sysroot — host-side `cross-*` steps keep resolving
against the outer EROOT's real config unchanged).

**Verified live**: `em --target x86_64-unknown-linux-gnu crossdev --setup
-p` on a target with nothing on disk previews the full 6-step plan
(including baselayout's real package resolution), and `ls
/usr/x86_64-unknown-linux-gnu` afterward confirms nothing was written —
`-p` semantics intact. Unit-tested:
`use_env::tests::sysroot_override_lets_a_cold_target_resolve` (asserts
the cold-target failure *without* the override too, so the test is a
real repro, not vacuous).

## Related, found but not fixed: `-a` declined still builds anyway

While tracing this, noticed `init_target` returns `Ok(())` whenever
`config_plan::apply` doesn't report `Applied` — true for both `-p`
*and* a declined `-a` prompt (`Outcome::Declined`). `setup()` doesn't
distinguish the two: it just continues on to compute `toolchain_plan`
and run every build step regardless, meaning `--setup -a` with the user
answering "n" to the config-write prompt still attempts the full
toolchain build against a config that was declined. Not fixed here —
out of scope for today's `-p`-only bug — but worth its own look.
