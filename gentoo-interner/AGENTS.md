# Project Conventions

## Build Commands

```bash
cargo test                        # Run all tests (unit + doc)
cargo clippy -- -D warnings       # Lint — must be warning-free
cargo fmt --check                 # Format check — must pass
cargo doc --no-deps               # Build docs — must have no warnings
```

## Architecture

- Single module crate (`lib.rs`) — small enough to keep in one file
- Public API: `Interner` trait, `Interned<I>` wrapper, `GlobalInterner`, `NoInterner`, `DefaultInterner`
- `Interner` trait uses static methods; `Interned<I>` carries `PhantomData<I>`
- Feature-gated implementations:
  - `interner` feature (default): `GlobalInterner` using papaya + boxcar + sharded mutexes
  - `lasso`: `GlobalInterner` using `lasso::ThreadedRodeo` (benchmarking only)
  - `symbol-table`: `GlobalInterner` using `symbol_table::GlobalSymbol` (benchmarking only)
  - No features: `NoInterner` using `Box<str>` (no deduplication)

## Dependencies

Minimal. Any new dependency must be justified.

Current dependencies:
- `papaya` (optional) — lock-free HashMap for the default interner backend
- `boxcar` (optional) — concurrent Vec for O(1) resolve in the default backend
- `parking_lot` (optional) — sharded mutexes for the default backend slow path
- `lasso` (multi-threaded feature, optional) — alternative interner backend; benchmarking
- `symbol_table` (global feature, optional) — alternative interner backend; benchmarking
- `serde` (optional) — serialization support

## Coding Style

- `rustfmt` — all code must be formatted
- No dead code, no unused dependencies
- Doc comments on all public types and methods
- Tests in `#[cfg(test)] mod tests` block

## Commits

[Conventional Commits](https://www.conventionalcommits.org/):

- `feat:` — new functionality
- `fix:` — bug fix
- `refactor:` — code restructuring without behaviour change
- `docs:` — documentation only
- `test:` — adding or updating tests
- `ci:` — CI/CD changes
- `chore:` — maintenance (dependencies, tooling)

Use `{tag}!:` when the commit breaks the API.

## MSRV

MSRV follows the workspace floor (**1.95**, `rust-version.workspace = true` in
`Cargo.toml`). See root [`AGENTS.md`](../AGENTS.md).

## Slop Warning

This codebase was largely AI-generated. Be skeptical of existing code — it may
contain bugs or surprising edge-case behaviour. Do not assume existing
patterns are correct.

Enforced anti-slop rules (comment length, test quality, scope discipline):
see root [`AGENTS.md`](../AGENTS.md) § Unslop Rules.
