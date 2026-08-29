# `require_root_distinct_from_host` never fires under `--target`

Status: 🔴 found 2026-08-29 (Opus review of [[crossdev-root-prefix-target-toolchain-anchor]]),
not fixed. Higher severity than that item — silent corruption of a
live prefix, no special precondition needed.

## The bug

`Cli::require_root_distinct_from_host` (cli.rs ~line 598) tests
`resolved.is_overlay()` to decide whether to reject a degenerate
`--root` equal to the host/prefix path. But `Cli::roots()`
unconditionally sets `.with_base(Some(sysroot))` under `--target`
(cli.rs:310), so `resolved.is_overlay()` (`eprefix.is_some() &&
base.is_none()`) is **always false** whenever `--target` is set — the
guard is dead code for every cross invocation.

**Verified live:**

```
em --prefix /tmp/pfxprobe --root /tmp/pfxprobe stages --stage1 -p
  !!! em stages --stage1 needs an explicit --root that doesn't equal the host install path

em --prefix /tmp/pfxprobe --root /tmp/pfxprobe --target riscv64-... stages --stage1 -p
  !!! cannot resolve make.profile at ...    ← sailed straight past the guard
```

Adding `--target` to the *exact same* degenerate invocation turns a
clean rejection into whatever downstream failure happens to occur
first — meaning `--prefix P --root P --target T stages --stage1` would,
absent that downstream config-resolution failure, actually bootstrap a
full stage1 straight into the live prefix `P`.

## Fix

Test `self.base_roots().is_overlay()` instead of `resolved.is_overlay()`.
Checked against every case in
`require_root_distinct_from_host_rejects_the_degenerate_cases`
(cli.rs:1030), including the `prefix_target` case (cli.rs:1061,
`--prefix /tmp/a --target T`, expects `is_ok`) and the `--local`
exemption — all stay green with this change.
