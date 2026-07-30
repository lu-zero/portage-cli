# Project Conventions

## Build Commands

Match CI (`.github/workflows/ci.yml`) before opening a PR. Locally, prefer
`cargo nextest` for the unit/integration suite (see [docs/testing.md](./docs/testing.md));
still run plain `cargo test` at least once if you need CI-identical behaviour
(doctests + default libtest scheduling).

```bash
# Core suite — CI `test` job (includes unit, integration, *and* doctests)
cargo test --workspace --exclude portage-bench
# Prefer for day-to-day (process-per-test; avoids portage-repo cwd races):
cargo nextest run --workspace --exclude portage-bench
# nextest does *not* run doctests — also run:
cargo test --workspace --exclude portage-bench --doc

cargo clippy --workspace --exclude portage-bench -- -D warnings
cargo fmt --all -- --check

# CI `doc` job (rustdoc warnings are hard errors)
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --exclude portage-bench --no-deps

# CI `bench-smoke` (compile only)
cargo check -p portage-bench --benches

# MSRV verification (use the project's cargo-msrv tool)
cargo install cargo-msrv
cargo msrv verify --rust-version 1.95 --path portage-cli
```

### Pre-PR / “would CI pass?” checklist

| Job | Local equivalent |
|-----|------------------|
| `test` | `cargo test --workspace --exclude portage-bench` (unit + integration + **doctests**) |
| `clippy` | `cargo clippy --workspace --exclude portage-bench -- -D warnings` |
| `fmt` | `cargo fmt --all -- --check` |
| `doc` | `RUSTDOCFLAGS='-D warnings' cargo doc --workspace --exclude portage-bench --no-deps` |
| `bench-smoke` | `cargo check -p portage-bench --benches` |
| `msrv` | `cargo msrv verify --rust-version 1.95 --path portage-cli` |
| `coverage` | optional locally (`cargo llvm-cov …`); uploads to Codecov in CI |

**Doctests:** plain `cargo test` runs them; `cargo nextest` does not. Broken
doc examples fail CI’s `test` job even when nextest is green. Rustdoc warnings
(broken links, invalid codeblocks, etc.) fail the separate `doc` job.

Full testing strategy and live-`emerge` parity: [docs/testing.md](./docs/testing.md).

## Architecture

- Binary crate producing the `em` command; CLI built with
  [clap](https://crates.io/crates/clap) derive macros, subcommands of the
  top-level `Cli` struct. Keep `main.rs` thin; extract modules as complexity grows.
- Business logic is delegated to the library crates (`portage-atom`,
  `portage-metadata`, `portage-solver`, `portage-resolve`, `portage-repo`,
  `portage-atom-pubgrub`, `portage-vdb`, `portage-binpkg`, `portage-distfiles`, …).
- **Read [`docs/architecture.md`](./docs/architecture.md) first** — it is the
  main architecture reference (crate catalog, the `em -p` resolution pipeline,
  USE stacking precedence, the USE/solver boundary, post-solve validation, and
  known divergences from emerge). Keep it updated as the design changes.

## Dependencies

Workspace members (14 library/binary crates + `portage-bench`):

- `gentoo-interner` — string interning
- `gentoo-core` — architecture and variant types
- `portage-atom` — PMS atom parsing (Cpn, Cpv, Dep, etc.)
- `portage-metadata` — md5-cache metadata, `RequiredUseExpr`, keywords, IUSE
- `portage-solver` — solver-agnostic trait and shared vocabulary
- `portage-atom-pubgrub` — PubGrub solver bridge (`em` resolves through this by default)
- `portage-atom-resolvo` — Resolvo SAT solver bridge (cross-check)
- `portage-resolve` — resolution policy / plan layer (USE, roots, post-solve; depends on `portage-repo`, unpublished)
- `portage-repo` — repository layout, profile stack, embedded ebuild shell
- `portage-vdb` — installed package database (`/var/db/pkg`)
- `portage-binpkg` — GPKG binary package read/write
- `portage-distfiles` — distfile fetch and mirror resolution
- `gentoo-stages` — stage3 tarball fetch/cache
- `portage-cli` — the `em` binary (unpublished)
- `portage-bench` — benchmark harness (excluded from most CI jobs; compile smoke only)

CLI/runtime deps: `clap`, `tokio`, `anyhow`, `thiserror`.

## Local dependency overrides

Machine-specific path patches in **gitignored** `.cargo/config.toml` (sibling
`brush` / `pkgcraft` worktrees) are expected during development. Do not commit
them. Example shape:

```toml
[patch."https://github.com/lu-zero/brush.git"]
brush-core = { path = "../brush/brush-core" }
brush-builtins = { path = "../brush/brush-builtins" }
brush-parser = { path = "../brush/brush-parser" }

[patch."https://github.com/pkgcraft/pkgcraft.git"]
pkgcraft = { path = "../pkgcraft/crates/pkgcraft" }
```

## Bumping the brush fork (routine, like a bench run)

`portage-repo` depends on the **lu-zero/brush** fork branch **`for-portage-repo`**
(workspace `Cargo.toml` pins `brush-*` by `rev`). After work on the local
sibling checkout `../brush` (same branch), land it like this — same cadence as
running official benchmarks:

1. **Local override** — path-patch as above so builds use `../brush` without a
   pin bump yet.
2. **Validate** (prefer nextest; path patch active):
   ```bash
   cargo nextest run -p portage-repo
   # at minimum the shell surface:
   cargo nextest run -p portage-repo --test brush_compat
   ```
3. **Publish brush tip to mine** (rebase-friendly; force-with-lease is normal):
   ```bash
   cd ../brush
   git checkout for-portage-repo
   git push --force-with-lease mine for-portage-repo
   git rev-parse --short=8 HEAD   # e.g. 940bec39
   ```
4. **Pin in portage-cli** — set all three `brush-*` workspace deps in root
   `Cargo.toml` to that `rev`, drop the path patch temporarily (or use a
   pkgcraft-only config) and `cargo check -p portage-repo` so the git source
   resolves. `Cargo.lock` is gitignored; only `Cargo.toml` is committed.
5. **Commit + push portage-cli**:
   ```text
   chore(deps): bump brush fork to <rev> (for-portage-repo)
   ```
6. Restore the local path patch for day-to-day work if you still have a live
   brush worktree.

Full notes and failure modes: [`docs/testing.md`](./docs/testing.md) § "Bumping
brush".

## Coding Style

- `rustfmt` — all code must be formatted
- No dead code, no unused dependencies
- Doc comments on all public types and functions
- Tests live in a `#[cfg(test)] mod tests` block
- Keep doctests compiling and green (`cargo test --doc`); rustdoc under
  `RUSTDOCFLAGS=-D warnings` must stay clean

## Commits

[Conventional Commits](https://www.conventionalcommits.org/):

- `feat:` — new functionality
- `fix:` — bug fix
- `refactor:` — code restructuring without behaviour change
- `docs:` — documentation only
- `test:` — adding or updating tests
- `ci:` — CI/CD changes
- `chore:` — maintenance (dependencies, tooling)

When a commit was significantly assisted by an AI tool, note it with an
`Assisted-by:` trailer rather than a `Co-Authored-By:` trailer. Use the kernel's
format (`AGENT_NAME:MODEL_VERSION`, colon-separated, e.g.
`Assisted-by: Maki:glm-5.2`). Only list *specialized* analysis tools after the
model version if any were used; basic dev tools (git, cargo, editors) are not
listed. The agent never adds a `Signed-off-by` (DCO) — that is the human's.

## MSRV

Until the first complete release, the workspace tracks **latest stable**
dependencies and bumps `rust-version` as needed (currently **1.95**, driven by
`cfg_select!` stabilization). Do not pin crates to older releases to satisfy a
lower MSRV.

CI runs `stable` and the declared workspace minimum (`1.95`). After a release,
foundational crates may again advertise a lower standalone MSRV; the workspace
floor follows whatever latest deps require.

When a dependency bump needs a newer compiler, raise `rust-version` in
`[workspace.package]` and the CI matrix entry, then `cargo msrv verify`.

## Testing strategy

See [`docs/testing.md`](./docs/testing.md) for the full picture: why
`cargo nextest` is preferred locally over plain `cargo test` (known
`portage-repo` flakiness), that nextest skips doctests, the
live-parity-against-real-`emerge` workflow that has caught most of this
project's real bugs, and the pre-PR checklist.

## Gentoo host tests

Five integration tests in `portage-cli/tests/comparison.rs` compare `em query`
output against `qfile`/`qlist` and are `#[ignore]` by default. On a Gentoo host:

```bash
cargo test -p portage-cli -- --ignored
```

## Slop Warning

This codebase was largely AI-generated. Be skeptical of existing code — it may
contain bugs or surprising behaviour. Do not assume existing patterns are
correct.
