# `--target` sysroot-missing error hints a dead CLI flag

Status: ✅ fixed 2026-08-29 (`7789b34`). Found during a from-scratch
riscv64 crossdev bootstrap in a crossdev-stages sandbox.

`portage-cli/src/emerge.rs:379-381`'s early-fail hint for a `--target`
sysroot that hasn't been laid down yet:

```rust
bail!(
    "cross target '{tuple}' is not set up at {cfg}\n  \
     run: em crossdev --target {tuple} --init-target"
);
```

`crossdev`'s own `-t`/`--target` flag no longer exists — it was unified
into `--target`/`-T` on the `Topology` mixin (`CrossdevArgs` has no `-t`).
An earlier hint of `em --target {tuple} crossdev --init-target` is also
stale as of 2026-08-30: topology flags belong after the applet.

**Fix** (current): `"run: em crossdev --target {tuple} --init-target"`.
