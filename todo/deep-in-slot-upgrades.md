# `--update --deep` in-slot upgrades (`-uD`)

STATUS: **done 2026-07-18** (commits `47d03b6`, `91c8307`, `5d4b82b`).

Companion to [[deep-slot-bump]] (`prefer_newest_slot` for `:*` / `SlotChoice`).
This item is the **transitive in-slot version upgrade** path emerge uses under
`-uD`, not the cross-slot bump alone.

## Emerge semantics (summary)

| Invocation | Named target in-slot | Transitive deps in-slot | Recurse satisfied installed | Newest `:*` slot |
|------------|----------------------|-------------------------|-----------------------------|------------------|
| `pkg` / `-p` | Yes (may `[R]`) | No (Favor installed) | No | No |
| `-u` / `-up` | Yes | No | No | No |
| `-D` alone | As arg | No optional upgrades | Yes (graph walk) | Slot bump only in em |
| **`-uD` / `-uDp`** | Yes | **Yes** | Yes | **Yes** |

Installed-and-kept packages expand **runtime** edges (RDEPEND/PDEPEND/IDEPEND).
Packages **being upgraded/built** pull full build-time deps; under `-uD` those
build tools are themselves candidates for in-slot upgrade.

## What em does

```
prefer_newest_slot = deep || emptytree_native   # SlotChoice only
prefer_update      = update && deep && !emptytree_native
```

| Knob | Effect |
|------|--------|
| `prefer_update` | `choose_version` does not early-return Favor for non-root packages; falls through to newest in-range |
| `prefer_update` + `broot_filtered` | Keep host-satisfied DEPEND/BDEPEND/IDEPEND as solver constraints so tools like cmake enter the graph |
| `prefer_update` | Skip post-solve `bdepend_trim` so intentional BDEPEND upgrades are not dropped as “already on BROOT” |
| `prefer_newest_slot` | Unchanged — newest `:*` slot |

Do **not** use `InstalledPolicy::Rebuild` for `-uD` (that is emptytree full
build-closure expansion for kept same-version packages).

## Live parity (host 2026-07-18, `www-client/firefox`)

| Command | em | emerge |
|---------|----|--------|
| `-p` / `-up` / `-Dp` | 72 | 79 (pre-existing shallow delta) |
| **`-uDp`** | **150** | **150** |

Small residual CPN set diffs (provider/tooling choice): emerge-only
`rust-bin` NS / `cython` / `maturin`; em-only `rust-common` / `llvm`+`clang` /
`libgit2` / `llhttp`. Not treated as blockers for this item.

## Performance (same host, hyperfine ×7, warm)

| Command | Mean |
|---------|------|
| `em -p firefox` | ~1.4 s |
| `em -uDp firefox` | ~1.45 s (~4% over `-p`; larger plan) |
| `emerge -p firefox` | ~3.65 s |
| `emerge -uDp firefox` | ~6.3 s |

Default `-p` path is unchanged by `prefer_update` (gated on `update && deep`).

## Still open (related, separate)

- **`-N` / `--newuse`** — [[newuse]]; flag parsed, not consumed for USE-drift rebuilds
- **`-U` / `--changed-use`** — [[cli-flag-parity]]
- Numeric `--deep=N` — not implemented (boolean only)
- Resolvo bridge: `set_prefer_update` is a trait default no-op until wired

## Code anchors

- Trait: `portage-solver/src/solver.rs` — `set_prefer_update`
- Provider: `portage-atom-pubgrub/src/provider/{mod,solve}.rs` — field, Favor arm, `broot_filtered`
- CLI: `portage-cli/src/query/depgraph/mod.rs` — `DepgraphOpts.update`, knobs, skip trim
- Docs: `docs/architecture.md` (target / Favor / `-uD`), `docs/build-roadmap.md` M5
