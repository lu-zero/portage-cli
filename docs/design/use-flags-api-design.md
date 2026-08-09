# USE flags API design

## Background

`em query depgraph` needs the effective USE flag state (profile stack + make.conf + user
environment) to feed the PubGrub solver.  Today that logic lives in
`portage-cli/src/query/depgraph/use_env.rs` and talks to the shell directly:

```rust
stack.configure_shell(&mut shell, &confs).await?;
let use_str = shell.get_var("USE").unwrap_or_default();
let use_expand = shell.get_var("USE_EXPAND")...;
```

The analysis below captures where that logic should live, what types it should use, and
how the two consumers (solver and ebuild execution) relate to each other.

---

## Shell lifecycle

`EbuildShell` starts clean (`do_not_inherit_env = true`).  It goes through these states:

```
new()                → bare shell, portage builtins registered
use_flags() / configure_shell()
                     → profile/conf/env evaluated; bash USE vars set
source_ebuild()      → full ebuild env loaded
run_phase()          → active execution
```

Two distinct operations are currently fused in `configure_shell`:

1. **Profile evaluation** — bash computation that resolves the effective USE tokens.
   Pure output: a list of enabled flag names.  The shell is a means to an end.

2. **Shell configuration for execution** — sets the Rust-side `use_flags: HashSet<String>`
   so the `use()` / `usev()` / `usex()` builtins work correctly during phase execution.

The solver only needs operation 1.  Ebuild execution needs both.
**Operation 1 is the foundation; operation 2 builds on top of it.**

---

## The `UseFlags` type

Lives in `portage-repo/src/repo/profile.rs` (pure data, no shell dependency).
Exported from the crate root.

```rust
pub struct UseFlags(Vec<Interned<DefaultInterner>>);
```

- Elements are **enabled flags only** — all USE_EXPAND expansions applied,
  `use.force` added, `use.mask` removed, env layer applied.
- No disabled tokens; absent from the set means not enabled.
- Using `Interned<DefaultInterner>` matches what `UseConfig` (portage-atom-pubgrub)
  consumes directly, with zero conversion at the call site.

Ergonomics:

```rust
impl UseFlags {
    pub fn iter(&self) -> impl Iterator<Item = &Interned<DefaultInterner>> { ... }
}

impl IntoIterator for UseFlags { ... }
```

### What `UseFlags` does NOT contain

`USE_EXPAND` group names (e.g. `"PYTHON_TARGETS"`) are **not** part of `UseFlags`.
They are display metadata — used only in `format_flags` to group
`python_targets_python3_14` back under `PYTHON_TARGETS="..."` in pretty output.
The solver never sees or needs them.

The CLI reads `USE_EXPAND` from the shell directly after calling `use_flags()`,
since the shell is still live at that point:

```rust
let flags = stack.use_flags(&mut shell, &confs).await?;
let expand_keys: Vec<String> = shell.get_var("USE_EXPAND")
    .unwrap_or_default()
    .split_whitespace().map(str::to_owned).collect();
```

An alternative richer type was considered:

```rust
// considered, deferred as premature
struct UseFlag {
    expanded: Interned<DefaultInterner>,  // "python_targets_python3_14"
    compact: Option<(
        Interned<DefaultInterner>,  // group key: "PYTHON_TARGETS"
        Interned<DefaultInterner>,  // short:     "python3_14"
    )>,
}
```

This would let display code skip prefix-matching entirely.  Deferred until a second
consumer of the compact form exists.

---

## `ProfileStack::use_flags()`

Lives in `portage-repo/src/build/profile.rs` (second impl block — needs shell access).

```rust
impl ProfileStack {
    pub async fn use_flags(
        &self,
        shell: &mut EbuildShell,
        extra_confs: &[&Path],
    ) -> Result<UseFlags>
}
```

### Evaluation order

1. Source each profile's `make.defaults` through brush with per-layer USE isolation
   (`profile_env` — already implemented).
2. Source each `extra_confs` entry (typically `/etc/portage/make.conf`) via
   `source_incremental` — already implemented.
3. **Environment layer** — after the profile sets `USE_EXPAND`, discover which variable
   names it contains, then for each key (plus `USE` itself) check `std::env` and apply
   any values as a final incremental layer via `source_incremental` on a synthetic
   script.  This is how `CC=my-cc em ebuild …` and `PYTHON_TARGETS=python3_15 em …`
   work — the same explicit env-injection pattern already used in `init_build_env` for
   `CC`, `CFLAGS`, etc.
4. `collect_use_flags(shell)` → apply `use.force` / `use.mask` → intern tokens → return.

The shell's bash state is modified by steps 1–3 (necessary for bash evaluation).
The Rust-side `use_flags: HashSet<String>` is **not** set — that is `configure_shell`'s
job.

### `configure_shell` becomes Layer 2

```rust
pub async fn configure_shell(
    &self,
    shell: &mut EbuildShell,
    extra_confs: &[&Path],
) -> Result<()> {
    let flags = self.use_flags(shell, extra_confs).await?;
    let refs: Vec<&str> = flags.iter().map(|f| f.as_str()).collect();
    shell.set_use_flags(&refs)
}
```

No logic duplication.  `configure_shell` remains in the public API unchanged from the
caller's perspective.

---

## Impact on the CLI

`use_env.rs::compute_use_env` and `build_use_config` collapse significantly.

Before (current):

```rust
stack.configure_shell(&mut shell, &confs).await.ok()?;
let use_str = shell.get_var("USE").unwrap_or_default();
let use_expand: Vec<String> = ...;
// build UseConfig by splitting use_str and handling +/- prefixes
for token in use_str.split_whitespace() {
    if let Some(name) = token.strip_prefix('-') {
        config.set(Interned::intern(name), UseFlagState::Disabled);
    } else {
        config.set(Interned::intern(token), UseFlagState::Enabled);
    }
}
```

After:

```rust
let flags = stack.use_flags(&mut shell, &confs).await.ok()?;
// display metadata — shell still live
let expand_keys: Vec<String> = shell.get_var("USE_EXPAND")...;
// build UseConfig — all tokens are enabled-only, no disabled branch needed
let mut config = UseConfig::new();
for flag in flags {
    config.set(flag, UseFlagState::Enabled);
}
```

The disabled branch in `build_use_config` disappears because `use_flags()` returns
enabled-only tokens (force/mask already resolved).

---

## Other pending improvements noted during review

### `Stability::is_acceptable()` — portage-metadata

The free function `keyword_accepts` in `query/depgraph/repo.rs` is a domain method
missing from the library:

```rust
// portage-metadata/src/keyword.rs
impl Stability {
    pub fn is_acceptable(self) -> bool {
        matches!(self, Stability::Stable | Stability::Testing)
    }
}
```

Then `keyword_accepts` becomes a one-liner using it, or can be replaced inline.

### `query/depgraph/` structure — correctly placed

The `Adapter` (implements `PackageRepository` for `RepoData`) is intentional
integration-layer code.  `PackageRepository` is designed to be implemented at the
consumer boundary.  No move needed.

The `installed.rs` bridge (VDB → solver types) similarly belongs at the CLI level since
neither `portage-vdb` nor `portage-atom-pubgrub` should depend on the other.
