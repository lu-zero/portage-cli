# Project Conventions

## Build Commands

Match CI (`.github/workflows/ci.yml`) before opening a PR. Locally, prefer
`cargo nextest` for the unit/integration suite (see [docs/testing.md](./docs/design/testing.md));
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

# CI `bench-smoke` (compile only). --features pkgcraft-compare pulls in
# pkgcraft (and gix) for the comparison benches; those are behind that
# feature specifically so a plain `cargo build`/`check` elsewhere in the
# workspace never pays for it. CI always includes it to catch breakage.
cargo check -p portage-bench --benches --features pkgcraft-compare

# MSRV verification (use the project's cargo-msrv tool)
cargo install cargo-msrv
cargo msrv verify --rust-version 1.95 --path portage-cli
```

When manually running/testing `em` (not benchmarking), build with
`cargo build --profile quick` instead of `--release` — thin LTO instead of
full LTO, ~4x faster to build (fresh or incremental) at real cross-crate
optimization, unlike debug or `profiling`'s `lto=false`. Use `--release`
for anything you'll benchmark or ship.

**Never exercise `em --keep-going`.** The flag exists for portage parity
and nothing else: a failed package must stop the run so its dependents
don't build against missing state and the failure stays first-class in
the log. Keep-going buries the root cause mid-tally (found live in the
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
| `bench-smoke` | `cargo check -p portage-bench --benches --features pkgcraft-compare` |
| `msrv` | `cargo msrv verify --rust-version 1.95 --path portage-cli` |
| `coverage` | optional locally (`cargo llvm-cov …`); uploads to Codecov in CI |

**Doctests:** plain `cargo test` runs them; `cargo nextest` does not. Broken
doc examples fail CI’s `test` job even when nextest is green. Rustdoc warnings
(broken links, invalid codeblocks, etc.) fail the separate `doc` job.

Full testing strategy and live-`emerge` parity: [docs/testing.md](./docs/design/testing.md).

## Architecture

- Binary crate producing the `em` command; CLI built with
  [usage-rs](https://usage.jdx.dev/rust/) derive macros, subcommands of the
  top-level `Cli` struct. Keep `main.rs` thin; extract modules as complexity grows.
- Business logic is delegated to the library crates (`portage-atom`,
  `portage-metadata`, `portage-solver`, `portage-resolve`, `portage-repo`,
  `portage-atom-pubgrub`, `portage-vdb`, `portage-binpkg`, `portage-distfiles`, …).
- **Read [`docs/architecture.md`](./docs/design/architecture.md) first** — it is the
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

CLI/runtime deps: `usage` (usage-rs 6), `tokio`, `anyhow`, `thiserror`.
Workspace `clap` stays for `portage-repo` (ebuild helpers + examples) and
`portage-bench`.

## Local dependency overrides

`brush`/`pkgcraft` are ordinary git dependencies in the workspace `Cargo.toml`
— a plain `cargo build`/`cargo test` fetches them itself, no sibling checkout
needed. A path patch in **gitignored** `.cargo/config.toml` is only for
iterating on local, uncommitted changes to one of those forks (sibling
`brush` / `pkgcraft` worktrees). Do not commit it. Example shape:

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

Full notes and failure modes: [`docs/testing.md`](./docs/design/testing.md) § "Bumping
brush".

## Coding Style

- `rustfmt` — all code must be formatted
- No dead code, no unused dependencies
- Doc comments on all public types and functions
- Tests live in a `#[cfg(test)] mod tests` block
- Keep doctests compiling and green (`cargo test --doc`); rustdoc under
  `RUSTDOCFLAGS=-D warnings` must stay clean

### Licensing — never embed a real shell script

This project is MIT-licensed; real `emerge`/`portage`/`crossdev`/eclass
shell code is GPL-2. Never copy or adapt a real GPL shell script (or a
close paraphrase of one) into a Rust string literal as a shortcut for
matching its behavior — that makes the literal a GPL derivative embedded
in an MIT tree.

- Implement behavior from the PMS specification text (which describes
  *what* must happen, not portage's own internal function names/control
  flow) or from independent understanding of the mechanism, in your own
  structure and wording. Matching an external tool's *interface* (a file
  naming convention, a directory layout another tool also reads) is fine;
  matching its *source code* is not.
- A real installed copy of `crossdev`/`portage` on the host is fine to
  read for understanding, and fine to shell out to or read data files
  from at runtime (the GPL work stays external, never embedded in `em`'s
  own distributed source) — the line is copying its *text* into this
  repository's `.rs` files.

### Unslop Rules

Enforced, not just a style nit — see [Slop Warning](#slop-warning) below
for why. This codebase is AI-generated by design, and every rule here is
something a past pass got wrong and had to clean up later.

**Comments**
- Default to no comment. Add one only when the *why* is non-obvious: a
  hidden constraint, a workaround for a specific bug, an invariant a
  future reader would violate without warning.
- Terse: one sentence beats a paragraph, a paragraph beats a wall of text.
  A comment that needs several paragraphs to justify a few lines of code
  is a sign the code itself needs simplifying, not that the comment needs
  more words. Never write a multi-paragraph doc comment or a multi-line
  comment block for something a reader can take in at a glance.
- Never restate what the code already says through its own names — the
  reader can read Rust.
- Never reference the current task, a commit, a PR/issue number, or a
  session ("fixed for the X flow", "added for issue #123", "per Luca's
  request"). That context belongs in the commit message; it rots the
  moment the file moves and nobody re-reads old commit logs to fix a
  comment.
- No commented-out code and no `// removed: ...` markers for deleted code
  — `git log`/`git blame` is the actual history.

**`todo/` is scratch, not documentation — never cited from a doc comment**
- `todo/*.md` is planning scratch: task briefs, investigation notes,
  status tracking, agent hand-off docs. It is expected to be pruned,
  archived to `todo/done/`, or rewritten at any time, by design.
- A doc comment (`///`, `//!`) — anything rustdoc renders as public,
  durable documentation — must never cite a `todo/` path as the source of
  a design decision or bug rationale. A future reader (human or agent) who
  doesn't have that file — because it moved, got archived, or never
  existed on their checkout — gets a dead pointer instead of the actual
  reason. Same for anything under `docs/`.
- A bare `//` implementation comment may reference `todo/` — it's an
  internal aside, not published documentation, and naturally gets pruned
  or rewritten alongside the code it sits next to.
- If the fact matters enough for rustdoc to show it, inline it as a real
  sentence in the doc comment instead of pointing elsewhere. If it's a
  durable architectural decision worth a longer writeup, promote it to
  `docs/design/*.md` and cite *that* instead.

**Dead weight**
- No speculative abstraction for a single call site: no config knobs,
  trait generalizations, or feature flags without a second concrete
  caller that needs them today.
- No error handling, fallback, or validation for a scenario the caller's
  own guarantees already rule out.
- `#[allow(dead_code)]` is not a way to keep something "just in case" —
  delete it; it's in git history if it turns out to be needed.

**Never downgrade a domain type to make code fast**
- `String`/`&str` is for text that is genuinely just text. If a value is a
  path, a CPV, an atom, or a slot, it keeps its type — `Utf8PathBuf`,
  `Cpv`, `Dep`, `Cpn` — even in the hottest loop in the workspace.
- When a typed value shows up hot in a profile, read what the type's impl
  actually does and use the cheap accessor at the *comparison site*
  (`as_str()`, `as_bytes()`). Example: `Utf8Path`'s `PartialEq` compares
  `Components` back-to-front, so `a.as_str() == b.as_str()` is a length
  check plus `memcmp` while `a == b` walks components. The field stays
  `Utf8PathBuf`; only the comparison changes.
- Swapping the field to `String` "for speed" trades away the meaning the
  type carried and spreads: the next reader cannot tell a path from any
  other string, and every conversion at the boundary comes back as an
  allocation.

**Comment length is not optional-nice, it's enforced**
- If justifying a change takes more than ~3 sentences, that justification
  belongs in the commit message, not the doc comment. A doc comment
  states the current invariant for a future reader; it is not the place
  to relitigate why a rejected alternative was rejected, walk through the
  investigation that found the bug, or narrate "found live: ...". One
  sentence of *why*, tops — cut everything else before committing.
- This applies doubly to comments written to justify a design decision
  the human pushed back on — the pushback itself is a signal the
  reasoning needs to be shorter and more confident, not longer and more
  defensive.
- Hard limits, not guidelines: no comment line over **150 characters**,
  no comment paragraph over **5 lines** (3 is better). Hitting either
  means cut, don't wrap — a longer paragraph is a sign the explanation
  needs to be smaller, not that it needs more room.

**Tests must exercise project logic, not the standard library**
- Don't add a test whose assertions would hold for *any* correct
  implementation of the underlying primitive — e.g. asserting a spawned
  `true` exits 0 and `false` exits 1 tests `std::process::Command`, not
  code this project wrote. If a helper is a thin wrapper with no branch
  of its own, it's fine to leave it uncovered by a dedicated test; the
  caller's own test coverage is enough.
- Prefer one test that exercises the actual decision point (a real
  branch, a real edge case, a real regression) over a test added purely
  to make "added a function → added a test" true.

**Scope discipline — stay inside the blast radius you were asked for**
- When investigating a live bug surfaces a *different*, deeper subsystem
  than the one you started in (e.g. debugging a config-layer bug leads
  into the dependency solver), stop and report the finding before editing
  that subsystem. A conversation agreeing a direction is promising is not
  the same as authorization to implement it — get an explicit go-ahead
  for the specific edit, not just agreement that the idea has merit.
- A signature change that cascades into many call sites and tests is a
  point to pause and confirm the shape before doing the mechanical
  propagation, not something to push through in one uninterrupted burst.
- If a change was explicitly reverted once, re-attempting it later — even
  with better justification — needs its own fresh explicit confirmation.
  Prior agreement on the general direction does not carry over to "go
  implement it now."

### Logging vs. program output — do not default to `println!`

This codebase has a real logging system (`src/diag.rs`): `tracing::info!`/
`warn!`/`debug!`/`trace!` events, filtered by a subscriber that honours
`-q`/`-v`/`-vv`/`-vvv` and `RUST_LOG`, rendered through a custom formatter
(portage's own `" * "` marker for `WARN`/`ERROR`, matching real emerge's
style). Read `src/diag.rs`'s module doc comment before adding any new
user-facing message.

- **New progress/diagnostic narration** ("resolved X", "wrote Y", "would do
  Z") is a `tracing::info!` event, not `println!`. This is what makes it
  respect `--quiet` and verbosity for free, and land on stderr instead of
  mixing into stdout.
- **`println!`/`print!` is reserved** for genuine emerge-parity protocol
  output that a script or human pipes/reads as the actual result — the
  `[ebuild N] pkg-1.0` plan display, `>>> Emerging (1 of N)` build-log
  passthrough, `config_plan`'s `-p`/`-a` preview-and-confirm prompt. If you
  are not reproducing something real `emerge`/`equery`/etc. would print,
  it is very likely a `tracing::info!`, not a `println!`.
- **A diagnostic that must survive `-q`/`--jobs N` is not a
  `tracing::info!`.** Both drop the level floor to `WARN`, so an `info!`
  event is invisible during exactly the parallel merge where a problem
  matters. `style.rs` has the writers for those: `warn_line!` (non-fatal,
  the caller carries on), `error_line!` (the item's operation definitively
  failed), `ewarn_sub_bullet!` (an indented item under one of those), and
  `einfo_line!` (a structured **stdout** report). An action banner that
  must keep its literal `">>> "` prefix goes through `tracing::info!` on
  `portage_repo::ACTION_TARGET`, which `diag.rs` renders bare.
- **Existing `println!` calls are not proof of the right pattern**, but
  most surviving ones are load-bearing: `em setup`'s "Prefix ready at …"
  report, `emerge.rs`'s `>>> Quitting.`/`>>> Unmerging …` parity lines and
  `dispatch.rs`'s `em log`/`em atom` results are all primary output, not
  narration. Check what a caller actually reads before converting one.

## Commits

[Conventional Commits](https://www.conventionalcommits.org/):

- `feat:` — new functionality
- `fix:` — bug fix
- `refactor:` — code restructuring without behaviour change
- `docs:` — documentation only
- `test:` — adding or updating tests
- `ci:` — CI/CD changes
- `chore:` — maintenance (dependencies, tooling)

Same hard limits as comments (see Unslop Rules): no body line over
**150 characters**, no paragraph over **5 lines** (3 is better). The
subject line still follows Conventional Commits' own ~50/72 convention.

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

See [`docs/testing.md`](./docs/design/testing.md) for the full picture: why
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

## Estimating how long something will take

Don't guess wall-clock time from a task's *shape* ("a full crossdev bootstrap
sounds like hours"). Check the machine you're actually on first — `nproc`,
`/proc/cpuinfo`, existing build/benchmark logs in this session or `todo/` —
and scale from a real prior timing on that same host if one exists, rather
than a generic instinct. A build that's genuinely hours on a laptop can be
~20 minutes on a fast multi-core dev box, and the reverse also happens on a
slow or contended one. State the estimate's basis ("X took Y on this host
last run") instead of asserting a duration as if it were host-independent.

## Slop Warning

This codebase was largely AI-generated. Be skeptical of existing code — it may
contain bugs or surprising behaviour. Do not assume existing patterns are
correct — including patterns in comments and docs: a `todo/` citation inside
a doc comment is exactly the kind of slop the
[Unslop Rules](#unslop-rules) above exist to stop; fix it (see that section)
rather than copying the pattern into new code.
