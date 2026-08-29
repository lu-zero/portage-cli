# `crossdev --setup -a` declined still runs the full build

Status: 🔴 not started. Found 2026-08-29 investigating
[[crossdev-setup-pretend-cold-target-gap]], not fixed there (out of scope
for that fix).

`init_target`'s only reaction to `config_plan::apply`'s outcome is:

```rust
if !config_plan::apply(&entries, globals.pretend, ask, policy)?.applied() {
    return Ok(());
}
```

`Outcome::applied()` is false for **both** `Previewed` (`-p`) and
`Declined` (`-a`, user answered no) — `init_target` returns `Ok(())`
either way, and `setup()` doesn't inspect that return value at all
beyond `?`. So a declined `-a` config-write prompt and a `-p` preview
are currently indistinguishable to the caller: `setup()` proceeds to
compute `toolchain_plan` and run every build step regardless of which
one happened.

Under `-p` that's correct (the whole run is a preview, so continuing to
preview later steps is fine — see the sibling fix that made this
actually work on a cold target). Under a **declined** `-a`, this means
telling `em` "no, don't write that config" doesn't stop it from
attempting the full toolchain bootstrap against a target whose config
was never actually written.

**Fix direction**: not yet designed. `init_target` needs to distinguish
"previewed" from "declined" so `setup()` can abort on the latter — either
have `init_target` return the `Outcome` instead of `Result<()>`, or bail
explicitly on `Outcome::Declined` before returning.
