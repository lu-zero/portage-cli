# Parser audit pass

STATUS: 🟢 **FULL PASS COMPLETE 2026-07-21.** All 8 items audited against the
real Portage reference source (`/usr/lib/python3.13/site-packages/portage/`),
not memory. Three real bugs found and fixed with fail-first regression tests;
the rest confirmed correct or documented as known low-severity divergences.

Fix summary (each its own commit):
- **Item 7** (top finding, 2026-07-20, `78550ca`): `FEATURES`/`ACCEPT_*`/
  `CONFIG_PROTECT*`/`IUSE_IMPLICIT` added to the `incr` list in both
  `ProfileStack::profile_env` and `source_incremental`
  (`portage-repo/src/build/profile.rs`). Test
  `make_conf_merges_features_without_interpolation`.
- **Item 5** (`ee6fc15`): **inverted `!flag?` USE-dep semantics** — a genuine
  correctness bug Fable's earlier read had marked "correct". PMS 8.2.6.4:
  `foo[!bar?]` = `bar? ( foo ) !bar? ( foo[-bar] )`, so parent-flag-OFF requires
  the *target* flag **disabled**, not enabled. Both evaluators had it inverted
  (`check_use_deps` in `validate.rs` and `eval_violated_use_dep` in
  `post_solve.rs`). Verified against portage's own
  `_use_dep.evaluate_conditionals` `!?` branch. Tests
  `check_use_deps_conditional_inverse_parent_off_requires_target_off` and
  `eval_violated_use_dep_matches_pms_table` (full six-form table).
- **Item 2** (`0bcdafa`): the four `/etc/portage/package.*` dir readers in
  `portage-resolve/src/use_env.rs` skipped only `is_file()`, leaking dotfiles
  and `~` editor backups that real portage's `_recursive_basename_filter`
  ignores. Exposed the profile stack's directory reader as
  `portage_repo::read_config_lines` (extended to also skip `~`), reused it in
  all four loaders (removing the third duplicate walk). Tests
  `package_use_dir_skips_dotfiles_and_backups`,
  `package_keywords_dir_skips_dotfiles_and_backups`.

Clean / documented (no fix needed): items 1, 3, 4, 6, 8 — see per-item notes
below. Full test/clippy/fmt suite green after the pass.

Was row 4/8 on the 2026-07-18 next-pending queue ([[PENDING]]). Full report
below, verbatim from the audit agent (one path citation corrected:
`repo/profile.rs` → the actual `build/profile.rs`).

A burst of parser-touching work landed across the metadata/profile/atom path
without a unified correctness re-check. Make a deliberate pass to confirm each
parser is PMS/`make.conf(5)`-faithful and that the layers agree, before the next
round of features piles on top.

## Scope (the parsers to review)

Recent commits (`41c35ad` `b6accf2` `1f5c6a4` `26fa1d7` `bb90bd4` `a934c89`
`2796f95` `6b2296c` `c826528` `99c9ae3` `67068eb` + the `-*` clear-all cluster)
touched:

- **Incremental `-*` clear-all** across the layers — `USE`, `ACCEPT_LICENSE`,
  `ACCEPT_KEYWORDS`, USE_EXPAND colon form (`L10N: -* en`). Confirm the
  `-*`-inside-a-group "clear then rebuild" rule and the profile→globals→conf→env
  precedence agree with `make.conf(5)` / PMS 5.2.4 everywhere, not just the depgraph
  display path.
- **`package.use` / `package.license` / `package.accept_keywords`** — the profile
  stack + `/etc/portage` readers. Are directory-form (PMS 5.2.4, files
  concatenated in filename order) and the per-package atom match identical across
  the three? `read_lines` is shared — verify it handles both file and dir.
- **`ACCEPT_LICENSE` `@GROUP` expansion** (`license_groups`) — confirm `@`-group
  resolution and the `-`-prefixed negation in license tokens parse like portage's
  `_license_map`.
- **`@set` expansion** (`@system`/`@world`/`@profile`/`@selected` + user sets) —
  the set stack and `sets.conf` reader. Verify nested `@set` refs and the
  profile `packages` accumulator.
- **USE-dep evaluation** (`UseFlagLookup` trait, interned flag keys) — the
  `[flag?]`/`[flag=]`/`[flag]` conditional eval in the atom solver bridge.
  Cross-check the `flag?` (conditional) vs `flag=` (required) semantics against
  PMS 8.2.
- **IUSE defaults** (`+flag`/`-flag`) — the `1f5c6a4` override rule and the
  `expand_use_expand_colon` group handling. Confirm the merge path and the depgraph
  path fold defaults identically (a known historical divergence risk).
- **`make.conf` / `make.globals` / `make.defaults`** sourcing (brush) — incremental
  merge of `USE_EXPAND`, `FEATURES`, `ACCEPT_*`. The brush `+=` array-append fix
  (`9086ca4`) and the `[[ -v assoc[key] ]]` fix (`aa172f9`) are in; confirm no
  regression in the incremental variable model.
- **`md5-cache` / `metadata` parse** (`portage-metadata`) — `auxdbkey_order`,
  `REQUIRED_USE` expr, `SRC_URI` tree. The computed-SRC_URI fix (`2965fa2`) is in;
  spot-check the cache-entry field set against `auxdbkey_order`.

## brush shell parser/printer (surfaced 2026-07-01 by the `__worker` env handoff)

- ✅ **`$'…'` ANSI-C quoting**: a literal `"` in the body made the winnow parser
  swallow the closing `'` (it went through `parse_balanced_delimiters`, whose
  construct scanner opened a double-quoted string). Broke sourcing any
  `declare -p` dump containing `COMP_WORDBREAKS`. Fixed in the fork
  (`6038e073`, dedicated parser + compat YAML tests); workspace rev bumped.
- 🟡 **`$"…"` gettext quoting** still goes through the same generic
  `parse_balanced_delimiters` scanner. Spot-checked OK on the mirror cases
  (`$"'"`, `$"a'b"`, `$"a\"b"`), so no known bug — but it deserves the same
  dedicated-parser treatment for the audit pass rather than the construct
  scanner.
- 🔴 **`declare -f` printing doesn't round-trip heredocs**: the AST Display
  wraps nested bodies in the `indenter` crate, which space-indents heredoc
  bodies *and* the `<<-EOF` delimiter (tabs-only strip ⇒ never terminates), and
  splits the trailing redirection onto the next line. Any dump containing e.g.
  `_tc-has-openmp` (toolchain-funcs) is unparseable. em sidesteps it — the
  `worker-env` handoff dumps variables only — but the VDB `environment.bz2`
  still embeds it (compat gap for consumers that re-source, and blocks any
  future function-carrying handoff). Fix belongs in brush's printer: emit
  heredoc bodies verbatim with the delimiter unindented, escaping the
  indenting writer.

## Method

For each: pick 3-5 representative inputs (including the `-*` and USE_EXPAND
edge cases), run both em's parser and portage's reference (`portage.config` /
`portage.dep` / `portage.cache.metadata`), and diff the resolved values.
`portage-repo/bench.sh`'s `compare_caches` example is the template for the cache
field comparison (semantic, order-independent).

Record divergences here as 🔴 items; the known-intentional ones (install-order,
flag ordering in display) are in `docs/architecture.md` § "Known divergences".

## Why now

These parsers feed the solver, the USE fold, the license/keyword gates, and the
fetch SRC_URI — i.e. everything the binhost/stage work leans on. A silent parse
regression there would mismatch `emerge -p` or mis-merge before the binpkg layer
can catch it.

---

## Audit report (Fable, 2026-07-20)

Scope covered: the 8 numbered items plus the 3 brush shell parser/printer notes above. Method: read the actual Rust source, traced representative inputs by hand against PMS/`make.conf(5)` text, cross-checked against unit tests already in the tree, and spot-checked against the vendored real Gentoo tree in `portage-repo/gentoo/`.

### 1. Incremental `-*` clear-all (USE / ACCEPT_LICENSE / ACCEPT_KEYWORDS / USE_EXPAND colon form)

> **UPDATE 2026-07-21 (full-pass): re-verified, confirmed correct — existing tests green.**
> Re-read both `merge_flag_lists_signed` copies and `expand_use_expand_colon`;
> the "`-*` clears everything accumulated so far in group order, then rebuild"
> rule and the `pkginternal < defaults/conf < pkg < env` precedence hold. All the
> cited `-*`/USE_EXPAND unit tests pass in the full-workspace run.

**[Fable's original assessment]** Confirmed correct, and unusually thoroughly tested. The core fold lives in two intentionally-duplicated (documented, reviewed) copies of `merge_flag_lists_signed`: `portage-solver/src/use_config.rs:265` and `portage-repo/src/repo/profile.rs:727`. Both correctly implement "`-*` clears everything accumulated so far in this call's own group order," preserve explicit `-flag` disables so they can later override a `+flag` IUSE default, and thread a leading `-*` marker forward when the result feeds a further fold (`ResolvedUse::pre_env`/`env_use` in `portage-repo/src/build/profile.rs:273`).

The `pkginternal < defaults/conf < pkg < env` layer order (`resolve_effective_use` in `portage-solver/src/use_config.rs:190`) is live-verified against real `emerge` per its own doc comment (`package.use` survives a `make.conf`-level `-*` but not an env-level one; a `+`-defaulted IUSE flag is wiped by either). `expand_use_expand_colon` (`portage-resolve/src/use_env.rs:357`) correctly implements the `L10N: -* en` clear-then-rebuild rule, gated on the token actually being a known `USE_EXPAND` key.

**Already covered by existing tests — no further audit needed**: `portage-solver/src/use_config.rs` tests (baseline, conf-level `-*` survival, env-level `-*` wipe, IUSE-default suppression by either layer), `portage-repo/src/build/profile.rs` tests (`use_flags_dash_star_clears_accumulated`, `use_expand_defaults_reach_pre_env`, the parent/child expansion-wipe tests), `portage-resolve/src/use_env.rs` colon-form tests, `portage-repo/src/repo/license_groups.rs`'s `accept_license_dash_star_clears_accumulated`.

### 2. `package.use` / `package.license` / `package.accept_keywords` — profile stack + `/etc/portage`

> **UPDATE 2026-07-21 (full-pass): FIXED in `0bcdafa`.** Exposed the profile
> stack's directory reader as `portage_repo::read_config_lines` (extended to
> also skip `~` backups, fully matching portage's `_recursive_basename_filter`
> which skips both `.`-prefixed and `~`-suffixed) and reused it in all four
> `use_env.rs` loaders — closing the dotfile gap and eliminating the third
> hand-rolled directory walk in the process. Fail-first regression tests added.
> Not changed: em (like the pre-existing profile reader) does **not** recurse
> into subdirectories the way portage's `_recursive_file_list` does — PMS 5.2.4
> mandates only the directory's files, so the non-recursive behavior is
> PMS-faithful and shared by both readers; left as-is.

**[Fable's original finding]** Confirmed divergent (concrete, narrow-impact). There are two independent directory-reading implementations in this codebase:

- The profile stack's canonical `read_lines` (`portage-repo/src/repo/util.rs:23`, used via `read_profile_file` in `profile.rs:786`) correctly implements PMS 5.2.4: sorts entries, **skips dotfiles**, tested explicitly (`read_lines_directory_concatenates_sorted_skipping_dotfiles`).
- The `/etc/portage/package.*` readers — `load_package_use`, `load_package_keywords`, `load_package_license`, `load_dep_list`, all in `portage-resolve/src/use_env.rs` (lines 303, 403, 448, 490) — are hand-rolled duplicates that sort but **do not filter dotfiles** (`filter(|p| p.is_file())` only). This is a real function-name/no-reuse gap, not just style: `portage-repo`'s `read_lines` is `pub(crate)`, so `portage-resolve` literally cannot call it without it being exposed.

Concrete repro: an editor backup or placeholder file whose name doesn't start with `#` and happens to parse as `atom flag` (or a stray `.keep`) inside `/etc/portage/package.use/` will be read as real data by `em` but ignored by real Portage. Severity: **low-to-moderate** — most stray dotfiles won't parse as valid atoms and are silently skipped by the `Dep::parse` fallback, so the practical blast radius is small, but it's a genuine, demonstrable spec deviation and an easy one to fix.

Per-package atom matching itself is otherwise **identical and correct** across all three consumers — same whitespace/`#`-comment parsing, same directory-vs-file dispatch, same precedence order (profile stack → site `/etc/portage` → config overlay). `package.accept_keywords`/`package.license` correctly preserve a bare atom with an empty token list (portage's "accept `~arch`" idiom); `package.use` correctly does not.

**Fix sketch**: expose a `pub` directory-aware line reader from `portage-repo` (or lift it into a shared low-level module) and have the four `use_env.rs` loaders call it instead of re-implementing the walk.

### 3. `ACCEPT_LICENSE` `@GROUP` expansion (`license_groups`)

> **UPDATE 2026-07-21 (full-pass): re-verified against `LicenseManager._expandLicenseToken`, confirmed correct.**
> em's `AcceptLicense::from_tokens` matches portage's `_expandLicenseToken`
> (`package/ebuild/_config/LicenseManager.py`): `@group` allows each member,
> `-@group` negates (denies) each member, nested groups recurse, cycles are
> broken via the shared `expand_group` visited-set. One benign edge difference:
> portage warns-and-skips a `-`-prefixed member *inside* a group definition
> (invalid data), whereas em would intern it as a never-matching literal license
> — functionally identical (no real license is named `-foo`), fires only on
> malformed `profiles/license_groups`. Not worth a fix.

**[Fable's original assessment]** Confirmed correct. `LicenseGroupRegistry::expand` (`portage-repo/src/repo/license_groups.rs:59`) is cycle-safe via the shared `expand_group` helper. `AcceptLicense::from_tokens` correctly implements `*` (allow-all), `-name`/`-@group` (deny), `-*` (clear-all, tested), and `@GROUP` expansion recursively through nested groups. `AcceptLicense::merge` correctly distinguishes an *additive* `package.license` overlay from one that itself contained `-*` (replaces rather than unions) — this exact case is tested (`package_license_clear_replaces_global_allow_all`) and matches the real-world need (`-* @FREE` restricting a global `*`). USE-conditional `LICENSE` expressions are evaluated against the package's actual effective USE, not walked blindly (`conditional_license_respects_use` test, guards against a real regression class: ffmpeg's `fdk? ( all-rights-reserved )` under a disabled `fdk`).

**Already covered by existing tests** — no further audit attention needed.

### 4. `@set` expansion (`@system`/`@world`/`@profile`/`@selected` + user sets)

> **UPDATE 2026-07-21 (full-pass): two low-severity divergences found, documented not fixed.**
> Checked em's `SetResolver` against portage's shipped default
> `/usr/share/portage/config/sets/portage.conf` and `_sets/ProfilePackageSet.py`
> / `_sets/profiles.py`. Portage's real default `@world` is
> `@profile @selected @system`, and its `@profile` (`ProfilePackageSet`) reads
> only the *non-`*`* `packages` entries and *only* from profiles that opt into
> the `profile-set` format (`profile_formats` in `layout.conf`). em's `@profile`
> is every `packages` line and its `@world` is `@selected ∪ @system` (omitting
> `@profile`). For every standard Gentoo profile these coincide exactly — no
> shipped profile declares `profile-set`, so portage's `@profile` is empty and
> `@world` collapses to `@selected ∪ @system` on both sides. The gap is only
> observable under the niche `profile-set` format, which em does not model.
> Recorded in `docs/architecture.md` § "Known divergences from emerge" rather
> than fixed. Everything else below (system/selected/world folding, `*`/`-`/`-*`
> accumulator, `sets.conf` ini, nested-ref cycle safety) confirmed correct.

**[Fable's original assessment]** Confirmed correct. `SetResolver` (`portage-repo/src/repo/sets.rs`) correctly implements: `@system` = `*`-marked `packages` entries only (`ProfileStack::system_set`, `profile.rs:486`), `@profile` = every `packages` line, `@world` = `@selected ∪ @system`, `@selected` = `var/lib/portage/world` + `world_sets`. The `packages` accumulator (`profile.rs:461`) correctly folds `*cat/pkg` (system add), `-cat/pkg` (removal, matching regardless of whether the removed entry was system-marked — this exact form was found and fixed for the riscv profile's `-*sys-apps/busybox` case per the doc comment), and `-*` (full clear), ancestors-first. `sets.conf`'s `StaticFileSet` ini form is parsed correctly (`lookup_sets_conf`/`finish_static_file`). Nested `@set` references are cycle-safe (tested: `set_cycle_is_broken_not_errored`).

**Already covered by existing tests** — no further audit needed.

### 5. USE-dep evaluation (`[flag?]`/`[flag=]`/`[flag]`)

> **UPDATE 2026-07-21 (full-pass): REAL BUG FOUND — `!flag?` was inverted, fixed in `ee6fc15`.**
> Fable's original read below said "confirmed correct"; it was not. `foo[!bar?]`
> per PMS 8.2.6.4 expands to `bar? ( foo ) !bar? ( foo[-bar] )` — when the
> *parent's* `bar` is OFF, the *target's* `bar` must be **disabled**. Both
> `check_use_deps` (`validate.rs`) and `eval_violated_use_dep`
> (`provider/post_solve.rs`) required the target flag *enabled* instead
> (parent-off/dep-off was wrongly reported as a conflict). Cross-checked against
> portage's `_use_dep.evaluate_conditionals` (`dep/__init__.py`) `!?` branch,
> which appends `-flag` when the flag is absent from the parent's USE. The other
> five forms (`flag`, `-flag`, `flag?`, `flag=`, `!flag=`) were correct.
> Regression tests added in both files (fail-first verified). The blocker
> `Conditional`/`Equal` always-satisfied simplification noted below stands.

**[Fable's original assessment, now superseded for `!flag?`]** Confirmed correct against PMS 8.2.6.4, precisely matching the six-form table (`enabled`, `disabled`, `flag?`, `!flag?`, `flag=`, `!flag=`) in `portage-atom-pubgrub/src/validate.rs:47-109`. Cross-checked the exact semantics that are easy to invert: `Conditional` (`flag?`) only imposes a requirement when the **parent's** flag is enabled — checked, correct (not the target's). `ConditionalInverse` (`!flag?`) only imposes when the parent's flag is disabled — checked, correct. `Equal`/`EqualInverse` compare target-vs-parent state directly — correct. Undeclared-flag defaults (`flag(+)`/`flag(-)`) are resolved in `resolve_flag_state` (`validate.rs:413`) per PMS: fall back to the declared default, or `Disabled` if none given.

One narrow, already-documented simplification: `blocker_satisfied_by` (`validate.rs:296-298`) treats `Conditional`/`Equal` USE-deps on **blockers** as always-satisfied ("don't occur on blockers in practice"). This is a known, low-severity gap — worth a one-line note if anyone ever sees a real `!pkg[flag=]` blocker, but not worth prioritizing.

**Already covered by existing tests** for the core six-form table — no further audit needed there.

### 6. IUSE defaults (`+flag`/`-flag`) — merge path vs. depgraph display path

> **UPDATE 2026-07-21 (full-pass): re-verified, still in sync — no live divergence, not refactored.**
> Re-read all three copies: `effective_use::effective_use`
> (`portage-resolve/src/effective_use.rs:85`), the `-p` display path
> (`portage-cli/src/query/depgraph/output.rs:702`), and the no-cache merge-plan
> branch (`portage-cli/src/query/depgraph/mod.rs:1141`). All three perform the
> identical `resolve_effective_use → apply_force_mask → apply_ceded` sequence and
> agree step-for-step; IUSE defaults fold identically on the merge path and the
> depgraph path. No live bug. The recommended collapse-into-one-call refactor is
> a maintenance-hygiene change with no behavioral anchor, deliberately **not**
> bundled into this correctness audit to avoid destabilizing the display/merge
> paths; left as a tracked follow-up.

**[Fable's original assessment]** Confirmed in sync today, but structurally fragile. The canonical fold is `effective_use::effective_use` (`portage-resolve/src/effective_use.rs:85`): `resolve_effective_use` → `apply_force_mask` → `apply_ceded`, in that order (documented as load-bearing: force/mask must survive an env-level `-*`, and ceded flags must survive both). This exact three-step sequence is **independently re-implemented** in two other places:

- `portage-cli/src/query/depgraph/output.rs:702-717` (the `-p` display path) — re-derives `defaults`, calls `resolve_effective_use`, then `apply_force_mask` (when a cache entry exists), then `apply_ceded`. Currently matches step-for-step.
- `portage-cli/src/query/depgraph/mod.rs:1140-1162` (the merge-plan path, "no cache" branch for cross-derived/virtual packages with no metadata) — same three steps, `stable` hardcoded to `false` (unavoidable: no cache means no keywords to judge stability from).

Both currently agree with the canonical fold, so **no live divergence found**. But this is exactly the trap `resolve_effective_use`'s own doc comment warns against ("do not reimplement any part of this fold elsewhere") — there are now three hand-synced copies of a security-relevant-to-correctness sequence instead of one call site. This is the project's own previously-fixed disease ([[useconfig-clone-elimination]], 2026-07-12) recurring in a smaller form. **Fix sketch**: have `output.rs` and the no-cache branch in `depgraph/mod.rs` call `effective_use::effective_use` directly (it already accepts its inputs via a `cache: &CacheEntry` parameter — would need a small refactor to accept the no-cache case, e.g. an empty synthetic `CacheEntry`-shaped input) instead of re-sequencing the three calls by hand.

### 7. `make.conf`/`make.globals`/`make.defaults` sourcing — incremental `USE_EXPAND`/`FEATURES`/`ACCEPT_*`

**Confirmed divergent — this is the most significant finding of the audit** (spot-check-verified independently, not just by the audit agent). `make.conf(5)`'s "Incremental Variables" section lists `ACCEPT_KEYWORDS`, `ACCEPT_LICENSE`, `CONFIG_PROTECT`, `CONFIG_PROTECT_MASK`, `FEATURES`, `IUSE_IMPLICIT`, `PROFILE_ONLY_VARIABLES`, `USE`, and the `USE_EXPAND` family as **all** incrementally merged across profile → `make.conf` layers, regardless of whether the file's assignment textually interpolates the prior value (real Portage does this merge itself, outside the shell, in `config.py` — it is not a bash feature).

This codebase's `incr` list — the set of variables that get the "reset to empty before sourcing, then reset+merge after" treatment — appears in exactly two places, `ProfileStack::profile_env` (`portage-repo/src/build/profile.rs:120`) and `source_incremental` (`portage-repo/src/build/profile.rs:502`), and in **both** it is hardcoded to `USE`, `USE_EXPAND`, `USE_EXPAND_HIDDEN`, `USE_EXPAND_IMPLICIT`, `USE_EXPAND_UNPREFIXED`, plus whatever keys `USE_EXPAND` itself lists. **`FEATURES`, `ACCEPT_KEYWORDS`, `ACCEPT_LICENSE`, `CONFIG_PROTECT`, `CONFIG_PROTECT_MASK`, and `IUSE_IMPLICIT` are not in this list.** Every file in the profile+make.conf chain is sourced into one continuous brush shell (by design, so non-incremental vars like `EAPI` stay visible cross-file) — which means for these variables, whichever file in the chain assigns them *last*, in plain bash-overwrite form, silently wins; no merge happens at all unless the file itself happens to write `${FEATURES} …`.

Concrete demonstration: a profile's `make.defaults` sets `FEATURES="test-fail-continue"`; the user's `/etc/portage/make.conf` — as virtually every real-world make.conf does — sets `FEATURES="candy ccache"` without referencing `${FEATURES}` (this is the standard, documented, expected user idiom precisely *because* portage does the incremental merge on its own). Real `emerge`'s effective `FEATURES` is the union (`test-fail-continue candy ccache`); this codebase's effective `FEATURES` is just `candy ccache` — the profile's flag is silently dropped. `FEATURES` is consumed for real behavior (`portage-cli/src/ebuild.rs:853`, gates sandbox/keepwork/etc.), and `CONFIG_PROTECT` likewise (`ebuild.rs:1952`, `ConfigProtect::from_shell`) — both read a single final `shell.get_var(...)` with no cross-layer merge. `ACCEPT_KEYWORDS`/`ACCEPT_LICENSE` have somewhat lower real-world hit rates (usually make.conf-only in practice), but the same root-cause gap applies to them too.

The one existing test in this area, `source_env_file_composes_features_and_overrides_flags` (`profile.rs:690`), only demonstrates that FEATURES composes when the *later* file explicitly writes `${FEATURES} ccache` — it doesn't cover (and doesn't disprove) the plain-assignment case that's the actual real-world default.

**Severity: high** for `FEATURES`/`CONFIG_PROTECT` — silently-wrong build behavior a user would not notice until something profile-mandated (a feature flag, a protected path) mysteriously stops applying. Lower urgency for `ACCEPT_KEYWORDS`/`ACCEPT_LICENSE`/`CONFIG_PROTECT_MASK`/`IUSE_IMPLICIT` given they're less commonly set at multiple layers, but the fix is the same code path so there's no reason to do it partially.

**Fix sketch**: extend the existing `incr` list in both `profile_env` and `source_incremental` (`portage-repo/src/build/profile.rs`) to the full make.conf(5) incrementals set, using the existing (unsigned) `merge_flag_lists` — these variables don't need the "preserve explicit disable" signed variant `USE` uses, just ordinary incremental `-token` removal and `-*` clear-all.

### 8. `md5-cache`/metadata parsing (`portage-metadata`)

> **UPDATE 2026-07-21 (full-pass): re-verified against portage's `auxdbkeys`, confirmed complete.**
> Diffed `ParseState::feed` (`portage-metadata/src/cache.rs`) against the real
> `auxdbkeys` tuple (`portage/__init__.py:576`): all 18 non-`INHERITED` keys plus
> `_md5_`/`_eclasses_` are handled; `INHERITED` is correctly excluded (PMS 14.3,
> md5-dict carries the eclass list via `_eclasses_`). Unknown keys are silently
> tolerated (`_ => {}`), the correct forward-compatible behavior. The
> EAPI-conditional field-presence question (generation-time enforcement) still
> needs a live-portage comparison — unchanged, low priority.

**[Fable's original assessment]** Confirmed correct — cache field set is complete. Spot-checked `ParseState::feed` (`portage-metadata/src/cache.rs:156`) against the actual field set present in the vendored real Gentoo tree (`portage-repo/gentoo/metadata/md5-cache/`, sampled ~4000 files): every observed key (`EAPI`, `DESCRIPTION`, `SLOT`, `HOMEPAGE`, `SRC_URI`, `LICENSE`, `KEYWORDS`, `IUSE`, `REQUIRED_USE`, `RESTRICT`, `PROPERTIES`, `DEPEND`, `RDEPEND`, `BDEPEND`, `PDEPEND`, `IDEPEND`, `INHERIT`, `DEFINED_PHASES`, `_md5_`, `_eclasses_`) is handled. Verified `INHERITED` (a runtime-only concept, distinct from `INHERIT`) is correctly and deliberately excluded from md5-cache parsing (`cache.rs:272`'s own comment cites PMS 14.3; confirmed zero of the ~32,000 vendored cache files contain an `INHERITED=` line — it's derived at ebuild-eval time, not persisted).

`RequiredUseExpr::is_satisfied` (`required_use.rs:83`) matches PMS 7.3.4 exactly for all four operators, including the easy-to-invert edge cases (`|| ( )` vacuously true, `^^` exactly-one via count, `??` at-most-one via count). `SrcUriEntry` (`src_uri.rs`) correctly models plain/renamed/USE-conditional/group forms per PMS 7.3.2, with EAPI 8's `fetch+`/`mirror+` restriction prefixes represented.

The computed-SRC_URI fix (`2965fa2`) is a build-phase re-sourcing bug (fetch phase re-sourcing the ebuild and dropping eclass-computed `SRC_URI`/`S`), not a parser bug — out of scope for this file, but confirmed fixed and unrelated to the tree-shape parsing reviewed here.

`portage-repo/examples/compare_caches.rs` and `portage-repo/bench.sh` both **still exist** and remain usable as the semantic/order-independent cache-diff template if a live-portage comparison is ever run.

**Already covered by existing tests** for `REQUIRED_USE`/`SRC_URI` tree parsing — no further audit needed. The one thing genuinely worth a live-portage comparison (not achievable from source alone) is whether every EAPI-conditional field-presence rule (e.g. `BDEPEND` only valid EAPI 7+) is enforced at generation time rather than just tolerated at parse time — **uncertain, needs a live comparison**, low priority since malformed input here would come from `em`'s own cache generator, not external data.

### Brush shell parser/printer notes (2026-07-01 `__worker` findings)

All three are resolved, and the resolution is confirmed to actually be in the pinned dependency (`portage-cli/Cargo.toml` pins `brush-core`/`brush-parser`/`brush-builtins` at rev `92ebb646`, which is the fork's current `HEAD`):

- ✅ **`$'…'` ANSI-C quoting** — fixed by `6038e073` (dedicated parser, no longer routed through the generic `parse_balanced_delimiters` construct scanner). Confirmed `6038e073` is an ancestor of the pinned rev.
- ✅ **`$"…"` gettext quoting** — has its own dedicated parser support upstream (`d10db772`, "add gettext enabled quotes"), not the generic construct scanner the audit brief worried about. Confirmed ancestor of the pinned rev.
- ✅ **`declare -f` heredoc round-trip** — was 🔴 as of the doc's last update; now **fixed** by `daa421cd` ("here-document bodies survive a declare -f round-trip", 2026-07-10). The fix is unusually thorough: it fixes the indenter blindly space-indenting `<<-` heredoc bodies/delimiters (the originally-reported bug), plus two more bugs found reproducing the exact `toolchain-funcs.eclass`/`_tc-has-openmp` shape named in the audit brief (same-line trailing redirect after a heredoc rendering out of order; a stray `;` after a heredoc-terminated statement). Compat suite: 2109 passed / 0 failed / 138 known-fail (down from 139) after the fix. Confirmed ancestor of the pinned rev, so the VDB `environment.bz2` compat gap this blocked is closed.

**No outstanding brush-side work from this list.**

### Prioritized punch list

1. **[High] Item 7 — extend incremental-variable handling to `FEATURES`/`ACCEPT_KEYWORDS`/`ACCEPT_LICENSE`/`CONFIG_PROTECT`/`CONFIG_PROTECT_MASK`/`IUSE_IMPLICIT`.** This is the one finding in this pass that produces silently-wrong, user-visible behavior in the overwhelmingly common case (any make.conf that sets `FEATURES` without `${FEATURES}` interpolation — i.e., nearly every real-world make.conf). Fix is mechanical: widen the existing `incr` list in `profile.rs`'s `profile_env`/`source_incremental` and reuse the existing `merge_flag_lists`.
2. **[Medium, cheap] Item 2 — dotfile-skip gap in the `/etc/portage/package.*` readers** (`portage-resolve/src/use_env.rs`). Concrete, PMS-deviating, low blast radius today but easy to fix by exposing and reusing the profile stack's already-correct `read_lines`.
3. **[Low, maintenance risk] Item 6 — collapse the three hand-synced copies of `resolve_effective_use → apply_force_mask → apply_ceded`** (the canonical `effective_use::effective_use`, the depgraph display path, and the no-cache merge-plan branch) into calls to the one canonical function, per the project's own established anti-duplication rule. No live bug today, but it's the exact shape of bug this codebase has been bitten by twice before (2026-07-12 duplicate-fallback cleanup).
4. **[Low] Item 5 — blocker USE-dep `Conditional`/`Equal` forms treated as always-satisfied.** Real-world hit rate is very low (rare on blockers); note-only unless someone reports a false negative.

Everything else audited (items 1, 3, 4, 8, and all three brush notes) is confirmed correct and, in most cases, already covered by existing targeted unit tests — no further audit attention needed there absent a future feature touching the same code.
