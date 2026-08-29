# `env_d_dir`'s host fallback can double-register a host profile as a prefix profile

Status: 🔴 found 2026-08-29 (Opus review of [[crossdev-root-prefix-target-toolchain-anchor]]),
not fixed. Lower severity — narrow precondition.

## The bug

`select/env_d.rs`'s `env_d_dir` (~line 83-93) falls back to
`/etc/env.d/<subdir>` when the prefix's own copy
(`P/etc/env.d/<subdir>`) isn't a directory yet — but still labels the
result `is_host = false` (i.e. "this prefix's own env.d"). Separately,
`list_all_profiles` (~line 140-151) also collects the same host
directory directly, labeled `is_host = true`. The same directory ends
up enrolled twice, under conflicting labels.

`activate_latest` (~line 474-486) filters on `!p.is_host` to pick which
profile to activate. With the double-registration, it can end up
picking the **host's** compiler profile and activating it into the
**prefix's** `config-<target>` file.

Precondition: `P/etc/env.d/<subdir>` absent while `/etc/env.d/<subdir>`
exists — narrow in the normal crossdev flow (binutils/gcc create the
prefix's own copy as they merge), but reachable on a first activation
into a fresh prefix.

## Fix

Have `env_d_dir` return the config-root path unconditionally for
callers that mean "this root's own env.d" — let `list_all_profiles` own
the host-fallback decision explicitly instead of `env_d_dir` making
that call implicitly.
