# `crossdev --setup -a` declined still runs the full build

Status: ✅ fixed 2026-08-29 (`0b5a901`). Found investigating
[[crossdev-setup-pretend-cold-target-gap]].

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

**Fix**: `init_target` now returns `config_plan::Outcome` instead of
`Result<()>`; `setup()` bails immediately on `Outcome::Declined` instead
of falling through to `toolchain_plan`. The plain `em crossdev
--init-target` action (which never needed the distinction) just discards
it via `.map(|_| ())`.

**Verified live**: `em --target x86_64-unknown-linux-gnu crossdev
--setup -a` with a piped `n` answer (via `script` to fake a tty, since
`--ask` requires a real terminal) stops right after `>>> Quitting.` — no
toolchain plan or build step runs afterward.
