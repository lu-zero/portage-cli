# Project Conventions

## Build Commands

```bash
cargo test                                # Run all tests (unit + doc)
cargo clippy --all-targets -- -D warnings # Lint — must be warning-free
cargo fmt --check                         # Format check — must pass
cargo doc --no-deps                       # Build docs — must have no warnings
cargo run --example list                  # Smoke-test the async list example
cargo run --example download              # Smoke-test the async download example
```

**Note**: Examples now require Tokio runtime and use async/await syntax.

## Architecture

- One primary type per module (`stage3.rs` -> `Stage3`, etc.)
- Modules are private (`mod`, not `pub mod`); public API is flat re-exports in `lib.rs`
- Uses standard library and minimal dependencies
- Parser functions are `pub(crate)`, types and their methods are `pub`

## Dependencies

Minimal: currently uses tokio for async runtime and reqwest for HTTP. Any new dependency must be
justified. Prefer standard library solutions where reasonable.

### Current Dependencies (Async Version)

**Library Dependencies (minimal):**
- `reqwest`: Async HTTP client with `rustls` + `stream` features
- `tokio`: Async runtime with `fs` + `io-util` features only (minimal footprint)
- `futures`: For stream handling utilities
- Standard library and minimal crates for core functionality

**Dev Dependencies (for examples/tests):**
- `tokio`: Full feature set for examples that use `#[tokio::main]`
- `env_logger`: For example logging
- `serde_json`: For testing

### Tokio Feature Strategy

**Library:** Minimal features (`fs`, `io-util`) - only what's actually used
- No runtime features needed in library (no task spawning)
- No macros needed in library (only examples use `#[tokio::main]`)

**Examples/Tests:** Full features for convenience and development
- Includes `rt`, `macros`, etc. for easy example development
- Not forced on library users

## Coding Style

- `rustfmt` — all code must be formatted
- No dead code, no unused dependencies
- Doc comments on all public types, fields, and enum variants
- Keep logic in the module alongside its type
- Tests live in a `#[cfg(test)] mod tests` block at the bottom of each module
- **Async Code**: All I/O operations should use async equivalents (`tokio::fs`, async reqwest)
- **Streaming**: Use streaming for large downloads to conserve memory
- **Error Handling**: Proper async error propagation with `?` operator

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
contain bugs, incomplete coverage, or surprising edge-case behaviour.
Do not assume existing patterns are correct; verify against the actual requirements.

Enforced anti-slop rules (comment length, test quality, scope discipline):
see root [`AGENTS.md`](../AGENTS.md) § Unslop Rules.
