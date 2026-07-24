# Full-codebase review — 2026-07-24

Sweep at 9b9e77a: production-unwrap census, panic/lexicographic-sort/`then_some`
scans, hardcoded-host-path scan, swallowed-error scan, `unsafe` audit, extra
clippy lints (`redundant_clone`, `needless_pass_by_value`, `unnecessary_wraps`,
`manual_let_else`), and an 8-significant-line duplicate-block detector across
all 15 crates. Prior audits (hand-rolled 2026-07-23, parser 2026-07-21) still
hold — nothing below re-opens those areas.

## Potential bugs

1. ✅ FIXED (5464011) **`portage-binpkg/src/maint.rs:285` — panic on SIZE-less index rows.**
   `size_mismatch: (!size_ok).then_some((actual_size, row.size.unwrap()))` —
   `then_some` evaluates its argument eagerly, so `row.size.unwrap()` runs even
   when `size_ok` is true. `size_ok` is defined as `row.size.is_none_or(...)`,
   so any row *without* a recorded SIZE that reaches the problems push (md5 or
   sha1 mismatch, or a signature failure) panics `em binpkg verify`. The md5 and
   sha1 lines right below already use the lazy `.then(|| …)` form. Fix: same.

2. ✅ FIXED **`portage-cli/src/pkg.rs:56` + `:231` — `em pkg use/mask/env` is
   root-blind.** Both `edit_valued` and `edit_mask` hardcode
   `/etc/portage/...`; `pkg::run(command)` (dispatch.rs:172) doesn't even
   receive `globals`, so `--root`/`--config-root`/`--prefix`/`--local` are
   silently ignored: queries read host state, and **edits write to the host's
   config** while the user believes they're operating on a prefix/sysroot.
   The correct helper already exists: `select::config_portage_dir_for(&Roots)`
   (currently `pub(super)` — promote it). Mitigation today: the explicit
   `--path` override. Same class as the `gentoo_mirrors_list` host-make.conf
   bug fixed in 94fef48.

3. ✅ FIXED (7cf1d91) **`portage-cli/src/ebuild.rs:1207-1211` — worker-env handoff swallows
   errors twice.** 🟡 `if let Ok(env_data) = capture_variables(...)` then
   `let _ = std::fs::write(...)`. If either fails, the Install worker later
   sources a missing/stale `worker-env` and fails (or half-behaves) far from
   the cause. Should at least `eprintln!` a warning; arguably propagate, since
   `should_dump_env()` means the worker *depends* on this file.

4. ✅ FIXED (7cf1d91) **`portage-vdb/src/field_cache.rs:40,46,57` — raw `lock().unwrap()`.** 🟡
   A panic while holding the lock poisons it and turns every later VDB field
   read into a panic cascade. The cache is a plain map with no invariants to
   protect — `.unwrap_or_else(std::sync::PoisonError::into_inner)` (the
   pattern `portage-cli/src/test_support.rs:36` already uses) is strictly
   better. Note: the poison-safe helper only exists in portage-cli test
   support; portage-vdb needs its own copy or a shared home.

## Sloppy / dubious (small, local)

Items 5-9 all fixed 2026-07-24: 5/6/8 in 584f2dc, 7 in 4ef8a21 (entry API,
no expect), 9 in afc1640 + 1028c1b + c7a1f2f.

5. `portage-binpkg/src/gpkg.rs:407-410` — `base.is_none_or(...)` guard
   followed by `base.unwrap()` three lines later; `let Some(name) = base else
   { continue }` states the invariant instead of re-proving it.
6. `portage-resolve/src/roots.rs:236-240` — `is_some_and(|b| ...)` then
   `self.base.as_deref().unwrap()`; if-let on `self.base.as_deref()` once.
7. `portage-cli/src/depclean.rs:193` — `in_degree.get_mut(t).unwrap()`;
   invariant holds (targets ⊆ cleanlist) but per project policy should be
   `.expect("targets come from cleanlist")`.
8. `portage-cli/src/cli.rs:611` — test `local_is_standalone_not_overlay`
   mutates `HOME` with save/restore but **no lock**, in a multi-threaded test
   harness; any parallel test reading `HOME` races. `pkgconf.rs`'s `PathGuard`
   + `test_support::path_lock()` is the right pattern.
9. Clippy (extra lints) worth applying — production code only:
   - redundant clones: `portage-atom-resolvo/src/provider.rs:626,655,907,930`
     (`use_constraints.clone()`), `portage-vdb/src/write.rs:346`,
     `portage-binpkg/src/gpkg.rs:84`,
     `portage-cli/src/crossdev/config_plan.rs:346`,
     `portage-cli/src/crossdev/mod.rs:2069`.
   - `needless_pass_by_value`: `depclean.rs:172 removal_order(Vec<...>)`
     (takes ownership *and* clones every entry into `by_cpv` anyway),
     `elfscan.rs:184 assemble_scan`, `config_plan.rs:251`,
     `repo/package.rs:23 name: String`, `repo/profile.rs:764 collect_stack`.
   - `unnecessary_wraps` (every caller pays a pointless `?`/`Ok(...)`):
     `select/mod.rs:147 get_chost`, `select/clang.rs:262 show`,
     `select/env_d.rs:412 run_show`, `vdb.rs:8,27`, `dispatch.rs:531 run_atom`,
     `query/list.rs:34`, `search.rs:53`, `maint/resume.rs:184 ensure_job_id`,
     `crossdev/mod.rs:883 show_target_cfg`, `ebuild.rs:2238`,
     `repo/sets.rs:203 finish_static_file`, `gentoo-stages/client.rs:98`,
     `pubgrub graph.rs:822`, `pubgrub solve.rs:288`.
   - `manual_let_else`: ~15 sites (vdb category/write, gpkg, maint, inherit,
     shell.rs:2274, ver_funcs.rs:522, repo/category, manifest, privilege,
     live_fs, resolve/repo.rs:1036).

## Duplication to consolidate

Ordered by value; the detector found 75 clusters, these are the real ones.

- **A. `next_build_id` verbatim ×2** — `ebuild.rs:1493` and `quickpkg.rs:368`,
  byte-identical 15-line fn. One home (portage-binpkg, next to the index it
  scans, or a cli-internal module).
- **B. solver-type triplication across crates** — `RequiredUse` in
  `portage-atom-pubgrub/src/required_use.rs` duplicates
  `portage-solver/src/required_use.rs` *although pubgrub already depends on
  portage-solver*; `PackageDeps`/`DepClass` in
  `portage-atom-resolvo/src/pool.rs:121-160` duplicate
  `portage-solver/src/facts.rs:28-70` (resolvo doesn't depend on solver yet —
  both are published crates, adding the dep is possible). The extraction into
  portage-solver looks started-but-unfinished.
- **C. `portage-repo/src/repo/category.rs` vs `portage-vdb/src/category.rs`**
  — near-whole-module duplication (5 clusters, ~90 lines): Category type,
  read_dir iterator scaffolding, sort. Needs a shared home (gentoo-core?) or
  an explicit "deliberately forked" note.
- **D. PMS var block ×2 in `portage-repo/src/build/shell.rs:873` / `:1419`** —
  the PV/PR/PVR/P/PF derivation + 7 `set_var` calls, identical in
  `source_ebuild`-init and phase-run paths. Extract
  `set_pms_name_vars(&mut self, ebuild)`.
- **E. `depclean.rs:310-345` vs `emerge.rs:895-935`** — the whole unmerge-batch
  preamble (shell + `set_build_roots` + `apply_profile_env` warning +
  preserve-libs registry/graph + failure loop) is copy-paste. Extract a shared
  batch-unmerge helper; divergence here means -c and --unmerge drift apart.
- **F. `merge/mod.rs`: `merge_sequential` vs `merge_parallel`** share the
  post-package bookkeeping (resume `mark_completed`, per-package `env_update`,
  `emit_pkg_end`) — clusters at 783/1020 and 873/1118; and within
  `merge_sequential` the Ok/Err arms both build `phases` + call `emit_pkg_end`
  (839/888) — the compute-then-match-once shape (see
  `unify-same-result-branches`).
- **G. `ebuild.rs` `WorkerArgs` construction ×3** — `build_and_merge` (402,
  424..512) and `merge_binpkg` (651) each hand-assemble the ~20-field
  `WorkerArgs` with 3-4 fields differing. A constructor taking the common
  context + per-call overrides kills the drift risk (new field ⇒ 3 edit sites
  today).
- **H. `portage-binpkg/src/index.rs:136` vs `:386`** — local `BinpkgIndex` and
  remote index duplicate `len`/`is_empty`/`find_reusable` (the reuse-gate
  rules are documented only on the local copy; the remote copy must track it
  by hand). Share the entry-filter core.
- **I. `portage-resolve`: `bdepend_trim.rs:146` vs `depend_trim.rs:97`
  (`should_keep`)**, and `download_size.rs:44` vs `required_use.rs:38`
  (plan-walk scaffold) — two pairs of same-shape logic.
- **J. incremental-var merge implemented twice** —
  `portage-repo/src/repo/profile.rs:744 merge()` vs
  `portage-solver/src/use_config.rs:281 merge_flag_lists_signed()` (the
  `-*`/`-flag` fold). Cross-crate; at minimum cross-reference the two so a
  PMS fix lands in both.
- **K. query boilerplate** — `query/keywords.rs`, `query/meta.rs`,
  `query/uses.rs` `run()` triplicate the resolve-atoms/iterate/print scaffold;
  `search.rs:18 run` vs `:184 run_emerge_style` overlap; `query/depgraph/mod.rs`
  repeats an internal block ×3-5 (412/693/976...) inside one giant fn.
- **L. `preserve_libs.rs:172` / `:243`** — `prune_dropped`/`prune_unneeded`
  share the remove_keys/updates apply-tail; small extract.
- **M. minor**: `gpkg.rs extract_image`/`read_metadata` container-open
  scaffold; `stages.rs toolchain_plan` internal repeat;
  `repo/build/env.rs EbuildEnv` vs `vdb/write.rs` spec struct field mirror
  (structural, probably fine); `commands/use_flag.rs`/`output.rs`/`has.rs`
  clap-arm boilerplate (structural, fine).

## Confirmed non-issues checked this pass

- All `unsafe` blocks audited: test-only `set_var` with SAFETY comments
  (cli.rs needs the lock per item 8), `elfscan` mmap, `jsonl_fd` from_raw_fd —
  all sound.
- `crossdev/mod.rs:1295` sort is on real `Version` (proper Ord).
- `vdb/category.rs:213` name sort is display-order strings, not versions.
- `slot.rs:152`/`cpn.rs:113` `chars().next().unwrap()` — guarded by the
  parser (`parse_ident_with_dot` never matches empty).
- `maint.rs:377 variants.last().unwrap()` — guarded by `len() < 2` continue.
- Doc-comment `unwrap()`s in examples are fine.
