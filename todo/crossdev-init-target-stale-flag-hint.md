# `--target` sysroot-missing error hints a dead CLI flag

Status: 🔴 not started. Found 2026-08-29 during a from-scratch riscv64
crossdev bootstrap in a crossdev-stages sandbox.

`portage-cli/src/emerge.rs:379-381`'s early-fail hint for a `--target`
sysroot that hasn't been laid down yet:

```rust
bail!(
    "cross target '{tuple}' is not set up at {cfg}\n  \
     run: em crossdev -t {tuple} --init-target"
);
```

`crossdev`'s own `-t`/`--target` flag no longer exists — it was unified
into the top-level `--target`/`-T` flag (`CrossdevArgs` in `cli.rs` has no
`-t` field; the doc comment on the top-level flag says so explicitly:
"crossdev no longer has its own `-t`/`--target`. One flag for both
roles"). Following this hint verbatim fails to parse.

**Fix**: `"run: em --target {tuple} crossdev --init-target"`.
