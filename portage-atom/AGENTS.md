# Project Conventions

## Build Commands

```bash
cargo test                        # Run all tests (unit + doc)
cargo clippy -- -D warnings       # Lint — must be warning-free
cargo fmt --check                 # Format check — must pass
cargo doc --no-deps               # Build docs — must have no warnings
cargo run --example parse_atoms   # Smoke-test the example
```

## Architecture

- One primary type per module (`cpn.rs` -> `Cpn`, `cpv.rs` -> `Cpv`, etc.)
- Modules are private (`mod`, not `pub mod`); public API is flat re-exports in `lib.rs`
- Parsers use [winnow](https://crates.io/crates/winnow) 1.0 combinators
- Parser functions are `pub(crate)`, types and their methods are `pub`

## Dependencies

Minimal: currently only `winnow` and `thiserror`. Any new dependency must be
justified. Prefer standard library solutions where reasonable.

## PMS Compliance

This library implements the [Package Manager Specification (PMS)](https://projects.gentoo.org/pms/9/pms.html).
All public types must reference the relevant PMS section in their doc comments
(e.g. `See [PMS 3.2](...)`).

## Coding Style

- `rustfmt` — all code must be formatted
- No dead code, no unused dependencies
- Doc comments on all public types, fields, and enum variants
- Keep parser logic in the module alongside its type
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

## MSRV

MSRV follows the workspace floor (**1.95**, `rust-version.workspace = true` in
`Cargo.toml`). See root [`AGENTS.md`](../AGENTS.md).
CI tests against both stable and MSRV. Do not use features that require a newer
version without updating `rust-version` in `Cargo.toml` and the CI matrix.

## Slop Warning

This codebase was largely AI-generated. Be skeptical of existing code — it may
contain bugs, incomplete PMS coverage, or surprising edge-case behaviour.
Do not assume existing patterns are correct; verify against the PMS.

Enforced anti-slop rules (comment length, test quality, scope discipline):
see root [`AGENTS.md`](../AGENTS.md) § Unslop Rules.
