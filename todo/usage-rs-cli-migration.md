# usage-rs CLI migration — postponed

**Status (2026-09-04): postponed, not abandoned.** Revisit in a few months
once usage-rs 6's rough edges (below) have had time to settle, or once
someone wants to invest in working around them in `em`'s own code.

## What this is

A full migration of `em`'s CLI parser from clap 4 to
[usage-rs](https://usage.jdx.dev/rust/) 6, per the design doc at
`docs/design/usage-rs.md` (still present on `master`, kept for reference —
it's a good writeup of the grammar tradeoffs regardless of whether the
migration proceeds). Grok executed the design doc's 9-PR plan end to end:
branch `usage-rs` in worktree `~/Sources/portage-cli-usage-rs`, based on
`master` @ `5be52b04`, 13 commits:

```
27d79307 refactor(cli): isolate __worker/__helper flags from future globals
43995ce4 test(cli): lock usage-rs 6 default-subcommand and mixin grammar
2bb10eee test(cli): lock flatten+global as a compile-fail
850fd492 refactor(cli): extract inline applet variants to Args structs
32ee13a4 refactor(cli): parse em with usage-rs 6
fd1881c7 fix: address review feedback for refactor(cli): parse em with usage-rs 6
ef8bed37 feat(cli): structure em --help with usage headings and effects
22c5a715 fix: address review feedback for feat(cli): structure em --help with usage headings and effects
1a8e3f69 feat(cli): ship usage-rs shell completions
1de2062a refactor(cli): dispatch applets with RunAsyncWith<&Cli>
4cc2d068 docs(cli): drop leftover clap wording
69d30922 docs(cli): generate reference pages from em's usage spec
49fd972e fix: address review feedback for docs(cli): generate reference pages from em's usage spec
```

155 files changed, ~15k insertions. The worktree is left in place (branch
`usage-rs`, not deleted) so this can be picked back up without redoing the
grammar-spike/dual-mount work — that part is genuinely done and matches
the design doc's decisions closely.

## Why postponed

Reviewed 2026-09-04 (`/code-review high` + manual build/test/live-argv
verification against the design doc's own must-preserve invocation
table). Full findings: [[usage-rs-migration-review]] (memory file, not
under `todo/` — session-local memory, not repo-tracked). Summary of why
this isn't ready to merge:

1. ~~**Not actually green.**~~ **Fixed 2026-09-04 (`cc06a82c`).**
   `cargo test -p portage-cli --release` was 615 passed / 1 failed
   (`help_tree_snapshot`) — the committed
   `portage-cli/tests/help_tree.snap` was never regenerated after PR6
   added the `em completion` applet; regenerating that also exposed a
   second, same-cause failure (`committed_cli_docs_match_spec` — PR7's
   `docs/user/cli/` reference pages were equally stale). Both
   regenerated (`UPDATE_HELP_TREE=1` / `UPDATE_CLI_DOCS=1`), no source
   changes; suite is now 616 + 4 passing.
2. **One real correctness bug**: `dispatch.rs`'s `--info` gate only
   checks that emerge's atom list is empty, not that no mode flag was
   requested, so `em --info -r` (or `-c`/`-C`/`-s`) silently prints
   system info instead of running the requested action. Untested
   combination — the design doc's must-preserve table never covers
   `--info` + a mode flag.
3. **A usage-rs 6 library limitation with no clean workaround found**:
   `usage::ValidationError::field(name).reason(...)` has no way to
   attach the actual offending value, so every cross-field `try_into`
   reject (the mechanism this whole migration hinges on for rejecting
   e.g. `em crossdev --root R` or `em -a search`) renders to the user as
   `invalid value '' for '--root': not valid with this applet` — reads
   like an empty string was passed, not "this flag isn't valid with
   this applet." Functionally correct (exit 2, correctly rejected), but
   materially worse UX than clap's messages for exactly the class of
   error this migration exists to make cleaner. This is the single
   biggest reason to wait: it's not an `em`-side bug to fix, it's
   upstream API surface that would need to mature (or `em` would need
   to post-process `render_failure`'s string output, which is fragile).
4. **One design question the doc explicitly flagged as needing a locked
   test never got one**: `em --info firefox` (Open Question #4 in the
   design doc). Live behavior: silently drops `--info`, runs a full
   defaulted `em firefox`.
5. Several duplicated-logic drift risks (all currently correct, no
   compiler-enforced sync) — not blockers by themselves, but signs the
   migration would benefit from another pass rather than merging as-is:
   `crossdev/mod.rs` hand-reimplements `overlay_root()`'s precedence;
   `validate()`'s `consumes_*` predicates duplicate the accessor
   `match` arms; `em completion <shell>` validity is declared twice
   (parser `choices()` + a hand-written `Shell::from_name` match);
   `--ask` gets a special-cased validation error field while every
   other `MergeFlags` field falls through to a generic name;
   `EM_EMERGELOG`'s env-fallback block is duplicated verbatim in both
   branches of `effective_activity()`.

What **did** check out, for the record — the parts of the design doc's
riskiest open questions that the spike (PR2) and swap (PR4) actually
proved out: same-type flatten of `MergeFlags`/`RootArg` onto `Cli` and a
child compiles and stays out of the child's own table; `em crossdev
--root R` is a clean parse error; `em -a search zlib` / `em -uD query
belongs ...` are correctly rejected; `em query depgraph --deep zlib`
still uses its own flatten, not the Cli overlay; there's exactly one
`--json`; hidden `__worker`/`__helper` stay out of `--help` but still
parse and self-document as hidden; the `--local`/`active set` trap
behaves as documented. So the hard architectural questions (Decisions
1-11 in the design doc) are answered and correct — what's missing is
polish, not a redesign.

## To resume

- Worktree/branch still exist: `~/Sources/portage-cli-usage-rs`, branch
  `usage-rs`.
- Rebase onto current `master` first (this branch is 45+ commits behind
  as of 2026-09-04, including `31c90e42` which drops the whole
  `fakeroost` privilege backend — [[fakeroost-fork]]. This branch's
  `Privilege`/`Backend` still has a `Fakeroost` variant that will need
  the same removal on rebase; `docs/design/usage-rs.md`'s `Privilege`
  ValueEnum snippet, which lists `fakeroost`/`pseudoroot`/`hakoniwa`
  cfg-gated variants, is now stale in the same way — drop `fakeroost`
  from it too when next touching that doc).
- Item 1 (stale test snapshots) is fixed. Fix item 2 (small). Decide on
  item 3 (either accept the UX regression, or write a thin wrapper
  around `Cli::render_failure`'s output for the small, enumerable set of
  `validate()` rejects — doable, just not attempted yet). Lock a test
  for item 4. Clean up item 5's duplication (or accept it and move on —
  none of the five are functionally wrong today).
