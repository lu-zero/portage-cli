# `env_d_dir`'s host fallback can double-register a host profile as a prefix profile

Status: ✅ fixed 2026-08-29 (`11f585e`). Found in an Opus review of
[[crossdev-root-prefix-target-toolchain-anchor]]. Lower severity —
narrow precondition.

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

## Fix (landed)

`env_d_dir` now always returns this root's own env.d path, whether or
not it currently exists — the host listing stays exclusively
`list_all_profiles`'s own explicit, correctly `is_host`-labeled pass.
Traced every call site (`linker`/`compiler`/`binutils`'s `run()`, which
only ever use the result to locate a profile *file* to activate, never
for listing): bare/`--local` topology's two paths already coincided
(no behavior change); for a `--prefix` whose own directory doesn't
exist yet, this correctly makes a host profile unselectable by
name/number too, consistent with `activate_latest`'s own `!is_host`
intent elsewhere. New test:
`env_d_dir_never_falls_back_to_the_literal_host_path`. Live smoke-check:
`em --prefix P select gcc list` still shows correct host/prefix
labeling and the active marker after the fix.
