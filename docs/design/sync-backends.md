# `em sync` git backends

`em sync` has two git backends, and a build compiles exactly one of them:

- `maint/sync/git_cmd.rs`, the default, runs the `git` binary.
- `maint/sync/git_gix.rs`, behind `--features sync-gix`, uses gitoxide (`gix`) in process.

Both follow Portage's `GitSync.update`: a shallow fetch, then `clean -d -x` + `reset --hard` on a
non-volatile repository, or `reset --merge` on a volatile one. This page tracks what keeps the gix
backend from being the default. Re-check it on every gix bump and move the "as of" line.

## Status as of gix 0.90 (2026-11-04)

The 0.90 bump only changed the API: `max-performance-safe` became `max-performance`,
`Repository::is_shallow` now returns a `Result`, `gix::path::try_from_bstr` became `from_bstr`,
and `merge_base` now reports "no common ancestor" as `Ok(None)`.

| Need | In gix | What `em` does |
|------|--------|----------------|
| Hard reset / checkout to a commit | No porcelain; upstream's `crate-status.md` still lists reset orchestration as unfinished | `gix_ext::hard_reset_to`, built from `index_from_tree`, `gix_worktree_state::checkout` and a ref edit; written to be proposed upstream as `Repository::reset` |
| `git clean -d -x` | No porcelain | `gix_ext::clean_worktree`, built on `dirwalk` |
| Rewrite the remote URL | No porcelain | `gix_ext::set_remote_url` |
| `git gc --auto` after a shallow fetch | No gc, prune, repack or maintenance API | Skipped. Unreachable objects pile up across shallow fetches, which costs disk space but is still correct; `git_cmd` runs `git gc --auto` |
| Commit signature check | `Commit::verify` runs `gpg` or `ssh-keygen` as git's config tells it to; it can't take a keyring file | Same check in both backends: read the raw commit and verify it in process against `sync-openpgp-key-path` (`maint/sync/verify.rs`, the `pgp` crate) |
| Committer identity for reflog writes | Clone and fetch now set a fallback themselves | `hard_reset_to` sets the fallback before its own ref edit. No gap left |

### Upstream plans (checked 2026-11-05)

gitoxide tracks planned work in GitHub Discussions; the issues for these items were closed when the
work moved there.

- **Reset:** [discussion 2779](https://github.com/GitoxideLabs/gitoxide/discussions/2779) lists `gix`
  reset (soft/mixed/hard/merge/keep) as an open task with no date. Our `Repository::reset_hard`
  ([PR 2849](https://github.com/GitoxideLabs/gitoxide/pull/2849)) was closed: the maintainer
  plans to write it. Downstream tracking: [portage-cli#9](https://github.com/lu-zero/portage-cli/issues/9).
- **Signatures:** [discussion 2796](https://github.com/GitoxideLabs/gitoxide/discussions/2796) covers
  signing commits without the gpg binary, not verifying them. The only proposal is to expose the
  primitives and leave the crypto to the caller, which is what `verify.rs` already does.
- **gc / maintenance:** [discussion 2801](https://github.com/GitoxideLabs/gitoxide/discussions/2801)
  ("`gix` towards 1.0") has a maintenance item with no design yet, and
  [discussion 2782](https://github.com/GitoxideLabs/gitoxide/discussions/2782) proposes merging packs
  incrementally. Neither has any code.

## Where the backends differ

- **Volatile repository with uncommitted changes.** `git reset --merge` keeps local changes that don't
  touch the incoming files and fails only on a conflict. gix's `ResetGuard::RefuseIfDirty` refuses
  any uncommitted change. So gix refuses some syncs that `git` would complete; it never overwrites
  anything `git` would keep.
- **Repository maintenance.** Only `git_cmd` runs `git gc --auto` (see the table).

## Performance

The comparison was measured on 2026-07-29 and hasn't been redone since gix 0.90.
See `benchmarks/sync-gix-vs-git-2026-07-29.md` and `benchmarks/gix-vs-git-scale-crossover-2026-07-29.md`.

- Build: sync-gix takes 5.4x as long to build (47 s → 4 min 14 s) and adds 6 MB to the binary.
- Cold shallow clone of `::gentoo` from a local copy: gix is about 15% faster. Repositories smaller
  than roughly 15k-65k objects are faster with `git`.
- Over the real network at `::gentoo` scale, `git` is about 9% faster.
- Not measured yet:
  - the cost of gix's own reset (`hard_reset_to`) on a warm `::gentoo` re-sync;
  - a warm re-sync over the real network;
  - a repository with full history rather than a shallow mirror.

## Test and CI coverage

- Each build tests only its own backend. `cargo nextest run -p portage-cli --features sync-gix` swaps
  `git_cmd`'s tests for `git_gix`'s and adds the `gix_ext` tests.
- CI's `test`, `clippy` and `doc` jobs build with default features. Only the `coverage` job
  (`--all-features`) compiles sync-gix, so a clippy or rustdoc failure that only appears under
  sync-gix doesn't fail CI.
- Gaps between the two test suites:
  - `git_cmd` has no test that a matching `sync-uri` leaves the remote config alone;
  - only `git_gix` has a test for a dirty volatile repository, and the backends behave differently
    there (see above).

## On a gix bump

1. Check the gix source for a reset/checkout porcelain, a maintenance API and a `Commit::verify`
   that can take a keyring. Each one found replaces a row in the table.
2. Run `cargo nextest run -p portage-cli --features sync-gix` and
   `cargo clippy -p portage-cli --features sync-gix -- -D warnings`.
3. Update the "as of" line, and the performance section if you measured.
