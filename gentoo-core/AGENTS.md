# Project Conventions

## Build Commands

```bash
cargo test                        # Run all tests (unit + doc)
cargo clippy -- -D warnings       # Lint — must be warning-free
cargo fmt --check                 # Format check — must pass
cargo doc --no-deps               # Build docs — must have no warnings
cargo run --example arch          # Smoke-test the example
```

## Architecture

- Core types in individual modules (`arch.rs`, `interner.rs`, `variant.rs`, `error.rs`)
- Modules `arch`, `interner`, and `variant` are public for generic type access
- Main types are re-exported as type aliases in `lib.rs`:
  - `Arch` = `arch::Arch<interner::DefaultInterner>`
  - `Variant` = `variant::Variant<interner::DefaultInterner>`
  - `KnownArch` = `arch::KnownArch`
  - `Error` = `error::Error`
- Secondary types (for custom interner configurations) accessible via pub modules:
  - `arch::Arch<I>` for custom interner generic
  - `interner::Interner`, `Interned<I>`, `GlobalInterner`, `NoInterner`
  - `variant::Variant<I>` for custom interner generic
- The [`Interner`] trait uses static methods; types using it carry `PhantomData<I>`
- Focus on minimal, reusable Gentoo-specific functionality
- Types and their methods are `pub` when part of the public API

## Dependencies

Minimal. Any new dependency must be justified. Prefer standard library solutions where reasonable.

Current dependencies:
- `thiserror` — ergonomic error type derivation
- `lasso` (multi-threaded feature, optional) — string interning for `GlobalInterner`; gated behind the `interner` feature (default on)

## Coding Style

- `rustfmt` — all code must be formatted
- No dead code, no unused dependencies
- Doc comments on all public types, fields, and enum variants
- Keep implementation logic alongside its type
- Tests live in a `#[cfg(test)] mod tests` block at the bottom of each module

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
CI tests against both stable and MSRV.
Do not use features that require a newer version without updating `rust-version` in `Cargo.toml` and the CI matrix.

## Gentoo-Specific Considerations

- Architecture handling must match Gentoo's keyword system
- Error messages should be Gentoo-user friendly
- Types should work well with Gentoo's package management concepts
- Consider integration with Portage and other Gentoo tools

## Slop Warning

This codebase was largely AI-generated. Be skeptical of existing code — it may
contain bugs or surprising edge-case behaviour. Do not assume existing
patterns are correct.

Enforced anti-slop rules (comment length, test quality, scope discipline):
see root [`AGENTS.md`](../AGENTS.md) § Unslop Rules.
