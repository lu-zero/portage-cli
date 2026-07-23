# License filter ignores USE conditionals → silent dep drop

STATUS: FIXED 2026-06-18 (both bugs). Clean-slate `em -pe firefox` went 357 → 382
(emerge 383). The pre-existing slotless-rust `|| ( rust-bin rust )` preference
gap is also **FIXED 2026-06-19** — see the section at the bottom.


Discovered 2026-06-18 via the clean-slate stage3 chroot test (crossdev-stages
aarch64 sandbox, `ACCEPT_LICENSE="@FREE"`, the Gentoo default for that profile).

## Symptom

`em -pe www-client/firefox` in the clean sandbox produced **357** packages vs
emerge's **383** — em a strict subset, missing **26** packages, all hanging off
one cut edge:

```
firefox → media-video/ffmpeg → virtual/libudev → virtual/udev → systemd-utils
                              ↘ media-libs/libass        → acct-group/*, kmod, udev-init-scripts
```

`em -p media-video/ffmpeg` (and `=media-video/ffmpeg-8.1.1`) fails with raw
`NoVersions` — em sees **zero** versions of ffmpeg, although the md5-cache has
them and ffmpeg-8.1.1 is stable arm64.

## Root cause

NOT the `@GROUP` set expansion — that is present and correct. `AcceptLicense::
from_tokens` expands `@FREE` via the group registry into `allowed`, and
`accepts(name)` checks it; `cairo` (LGPL-2.1) passes `@FREE` in the sandbox,
proving expansion works. The bug is one level up, in the LICENSE-*expression*
evaluator against USE. Both `AcceptLicense` methods in
`portage-repo/src/repo/license_groups.rs` share it:

```rust
// licenses_needed:
LicenseExpr::UseConditional { entries, .. } =>
    entries.iter().flat_map(|e| self.licenses_needed(e)).collect(),
// accepts_expr:
LicenseExpr::UseConditional { entries, .. } =>
    entries.iter().all(|e| self.accepts_expr(e)),
```

Both drop the USE flag (`..`) and walk **every** conditional branch as if
always-active. ffmpeg's LICENSE:

```
gpl? ( GPL-2+ ... fdk? ( all-rights-reserved ) ) !gpl? ( LGPL-2.1+ ... )
```

With default USE (`gpl` on, `fdk` off) the active license is `GPL-2+` (in
`@FREE`). But em collects `all-rights-reserved` (behind disabled `fdk`) plus the
`!gpl?` branch, so under `@FREE` every ffmpeg version is license-filtered →
`data.versions` has no ffmpeg → `NoVersions`.

Permissive `ACCEPT_LICENSE="* -@EULA"` (host default) hides the bug because
everything is accepted regardless. Reproduced on the host with:

```
ACCEPT_LICENSE="@FREE" em -p =media-video/ffmpeg-8.1.1   # → NoVersions
```

## Two bugs, really — both fixed

1. **License eval was USE-blind.** FIXED. `AcceptLicense::{accepts_expr,
   licenses_needed}` (`portage-repo/.../license_groups.rs`) now take an
   `enabled: &dyn Fn(&str) -> bool` predicate and skip inactive
   `flag?`/`!flag?` branches. The version filter (`Adapter::license_ok`),
   `target_package`, and `find_autounmask_candidates`
   (`query/depgraph/repo.rs`) compute the version's effective USE via the new
   `effective_use_config`/`license_ok_for` helpers — but only when the LICENSE
   actually has conditionals (`license_has_conditional` hot-path shortcut).
   Regression test: `license_groups::tests::conditional_license_respects_use`.

2. **Silent drop of an unsatisfiable unconditional dep.** FIXED. The autounmask
   candidate report (the keyword/mask/license "necessary changes" block) is now
   shown **unconditionally** when a required, no-`||`-alternative dep was dropped
   for lack of an installable version — previously gated behind the opt-in
   `--autounmask` flag (default off). `--autounmask-write` still gates *writing*.
   `DepgraphOutcome::exit_code` is also non-zero when such candidates exist
   (`query/depgraph/mod.rs`), so an incomplete plan never exits 0 silently.
   The vestigial `DepgraphOpts::autounmask` field was removed.

## Note on the test harness

The 26-pkg gap is NOT an emptytree bug — it reproduces in normal mode too
(`em -p firefox` also drops ffmpeg). The emptytree/​slot-conflict rewrite
(commit 2999e46) is unaffected; this is an independent, pre-existing bug the
clean-slate test surfaced.

## Slotless-rust `||` preference — FIXED 2026-06-19

Under `-pe`, em picked `rust-bin-1.95.0 [NS]` where emerge keeps installed
source `rust-1.95.0 [R]`. Emptytree-only; normal `-p` matched (both keep the
installed provider). Edges pulling rust-bin-1.95 (via `--json`): cbindgen,
cargo-c, librsvg, maturin, ast-serialize, firefox-151, and rust self-bootstrap —
all declaring `|| ( >=rust-bin-1.74.1:* >=rust-1.74.1:* )` (rust-bin first,
`:*` any-slot).

**Mechanism (corrected — the all-branches-installed fall-through is the
*CPN-level* arm, not the direct one):** the `:*` form produces a Choice (the
`||`) whose two branches are each a nested SlotChoice virtual (rust-bin's slots /
rust's slots). In `choose_version`'s installed-preference heuristic, the
`direct_installed` check (`self.installed.contains_key(pkg)`) is **always false**
for these branches — the branch's merged dep is the nested SlotChoice *virtual*,
which is never in `self.installed` → `directly_installed_count == 0`, so the
whole directly-installed block is **skipped** (confirmed via `EM_DBG_CHOICE`
instrumentation: `direct_inst=0`). Execution reaches the **CPN-level** arm, whose
`deps_reach_installed(vd, 2)` *does* traverse virtuals and reports **both**
branches as reaching an installed package → `installed_count == candidates.len()`
→ the old `< candidates.len()` guard failed → fall to `max()` → first-listed →
**rust-bin**. (The earlier note pinning this on the `directly_installed_count ==
candidates.len()` arm was wrong; that arm never fires for virtual branches.)

emerge's `dep_zapdeps` instead prefers the branch whose installed version is
**newer** (source rust-1.95.0 > rust-bin-1.93.1), avoiding a needless `[NS]`.

**Why firefox's own rust BDEPEND works:** it uses **specific slots**
(`rust-bin:1.94.1[llvm_slot_21]`), so the Choice branches are direct real
packages (not virtuals), and `direct_installed` sees rust-1.94.0 installed but
rust-bin-1.94.0 NOT → `directly_installed_count < candidates.len()` → the
some-not-all arm correctly picks the installed source-rust branch.

**Fix (landed):** new `PortageDependencyProvider::newest_installed_choice_branch`
(`provider/mod.rs`) returns, for an all-installed provider `||` Choice, the
candidate branch reaching the **newest** installed version — resolving each
branch one level (virtual → `branch_best_installed`, else direct
`self.installed`). Tie-break is emerge-faithful: only re-pick on a *strictly
newer* version; on a tie keep the first-listed alternative. Called from **both**
all-installed arms in `choose_version` — the direct one and, decisively, the
**CPN-level** `installed_count == candidates.len()` case where the rust Choice
actually lands. Regression test: `or_group_no_preference_when_both_installed`
(equal installed versions still pick first-listed). Verified: `em -pe cbindgen`
and `em -pe firefox` now keep `rust-1.95.0 [R]`; normal `-p` unchanged; full
pubgrub suite 141 passed.

**Caveat:** the version comparison is cross-package (rust-bin vs rust) and only
meaningful because they share upstream versioning — not a general mechanism, but
matches how `dep_zapdeps` treats this exact provider pair.
