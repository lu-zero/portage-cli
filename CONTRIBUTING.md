# Contributing

Match [CI](./.github/workflows/ci.yml) before opening a PR. Locally, prefer
`cargo nextest` for the unit/integration suite (see
[docs/design/testing.md](./docs/design/testing.md)); still run plain
`cargo test` at least once if you need CI-identical behaviour (doctests +
default libtest scheduling).

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

# CI `bench-smoke` (compile only). --features pkgcraft-compare pulls in
# pkgcraft (and gix) for the comparison benches; those are behind that
# feature specifically so a plain `cargo build`/`check` elsewhere in the
# workspace never pays for it. CI always includes it to catch breakage.
cargo check -p portage-bench --benches --features pkgcraft-compare

# MSRV verification (use the project's cargo-msrv tool)
cargo install cargo-msrv
cargo msrv verify --rust-version 1.95 --path portage-cli
```

`just check` runs the fmt, clippy, doc and test jobs plus the documentation
checks (dead links, and whether `docs/user/cli` matches the command line).
`just docs` regenerates that CLI reference; never edit its pages by hand.

When manually running/testing `em` (not benchmarking), build with
`cargo build --profile quick` instead of `--release` — thin LTO instead of
full LTO, ~4x faster to build (fresh or incremental) at real cross-crate
optimization. Use `--release` for anything you'll benchmark or ship.

**Never exercise `em --keep-going`.** The flag exists for portage parity
and nothing else: a failed package must stop the run so its dependents
don't build against missing state and the failure stays first-class in the
log. Keep-going buries the root cause mid-tally (found live in the
i586 stage1 pilot — python's cross-configure failure got buried under
18 more packages before anyone looked). Don't pass it in scripts, docs,
examples, or manual runs; fail fast, fix, re-run.

### Pre-PR / “would CI pass?” checklist

| Job | Local equivalent |
|-----|------------------|
| `test` | `cargo test --workspace --exclude portage-bench` (unit + integration + **doctests**) |
| `clippy` | `cargo clippy --workspace --exclude portage-bench -- -D warnings` |
| `fmt` | `cargo fmt --all -- --check` |
| `doc` | `RUSTDOCFLAGS='-D warnings' cargo doc --workspace --exclude portage-bench --no-deps` |
| `cli-reference` | `just docs-check` (stricter than CI: compares content, not just the page set) |
| `bench-smoke` | `cargo check -p portage-bench --benches --features pkgcraft-compare` |
| `msrv` | `cargo msrv verify --rust-version 1.95 --path portage-cli` |
| `coverage` | optional locally (`cargo llvm-cov …`); uploads to Codecov in CI |

**Doctests:** plain `cargo test` runs them; `cargo nextest` does not. Broken
doc examples fail CI’s `test` job even when nextest is green. Rustdoc warnings
(broken links, invalid codeblocks, etc.) fail the separate `doc` job.

Full testing strategy and live-`emerge` parity:
[docs/design/testing.md](./docs/design/testing.md).

### Gentoo host tests

Five integration tests in `portage-cli/tests/comparison.rs` compare `em query`
output against `qfile`/`qlist` and are `#[ignore]` by default. On a Gentoo host:

```bash
cargo test -p portage-cli -- --ignored
```
