# For opencode — implement the remaining built-in `@name` sets

STATUS: **✅ done (2026-08-13).** All four scope items + the refactor enabler
landed as six commits (plus a clippy cleanup), split one-per-concern and each
verified to build + test at its intermediate state. See **Results** at the
bottom for the commit SHAs, divergences from this plan, and the live-parity
table. `@security` remains deferred (needs a GLSA subsystem). The research
and implementation narrative below are kept as the historical record.

Companion doc: `done/package-sets-support.md` — the original audit that
found these gaps, plus the two things already fixed in that earlier session
(`world_sets` add/remove, `em --info -v` set listing). This file is the
implementation plan for what that audit left open.

---

## Scope for this pass

Implement, in whatever order makes sense:

1. `@selected-packages` / `@selected-sets` — trivial, no VDB needed.
2. `@live-rebuild` / `@deprecated-live-rebuild` — VDB metadata query.
3. `@module-rebuild` / `@x11-module-rebuild` — VDB CONTENTS-ownership query.
4. The `@profile` bug fix (already diagnosed, see below) — bundle it in
   since it touches the same dispatch table.

**Out of scope for this pass:** `@security`. It needs a whole GLSA data
source (fetch/parse/match), not just a new `SetResolver` match arm — see
the dedicated section at the bottom explaining why, so it isn't
accidentally scoped into "just add a match arm" work.

---

## Where the dispatch lives today

`portage_repo::SetResolver::direct_members` (`portage-repo/src/repo/sets.rs:63`):

```rust
fn direct_members(&self, name: &str) -> Result<Vec<GroupEntry<Dep>>> {
    match name {
        "system" => ...,
        "profile" => ...,
        "selected" => self.selected_members(),
        "world" => { ... }
        other => self.user_set_members(other),
    }
}
```

This only has access to the profile stack + `eroot` (files on disk) — no
VDB. That's why `@preserved-rebuild` was never added here: it's special-
cased *above* this layer, in two separate places that both need to agree:

- `emerge.rs::expand_sets` (`portage-cli/src/emerge.rs:68`) — for
  `TargetOrigin`-tagged root targets (what a real merge resolves against).
- `maint::world::resolve_set` (`portage-cli/src/maint/world.rs:497`) — for
  everything display/audit-only: `@world`/`@selected` composition,
  `-vv` purple highlighting, and now `em --info -v`'s set listing.

Both currently duplicate the same 4-line "open VDB, load preserve-libs
registry, call `preserved_rebuild_atoms`" special case. Adding
`@live-rebuild`/`@deprecated-live-rebuild`/`@module-rebuild`/
`@x11-module-rebuild` the same way would make that duplication 5 sets ×
2 call sites — worth factoring before or during this pass, not after.

**Suggested refactor:** pull the VDB-aware built-in sets into one shared
function, e.g. `maint::sets::resolve_vdb_set(name: &str, eroot: &Utf8Path)
-> Option<Result<Vec<Dep>>>` (or similar — `None` for "not a VDB-aware
name, try `SetResolver` instead"), called from both `expand_sets` and
`resolve_set`. Where exactly it lives (`maint::sets` vs a new
`maint::dbapi_sets`) is an implementation call — `maint::sets.rs` already
holds `KnownSets`, which is the closest existing home.

**Whichever names get resolved through this path, don't forget:**
`portage_repo::is_world_candidate` (`portage-repo/src/repo/sets.rs:169`)
already excludes all of these by name (it's built from the *full* real
`cnf/sets/portage.conf` list, not just what's implemented — see that
function's own doc comment) — no change needed there when these land,
which is the point of having built it that way up front.

---

## 1. `@selected-packages` / `@selected-sets` (trivial)

Real portage: `WorldSelectedPackagesSet` / `WorldSelectedSetsSet`
(`portage/_sets/files.py:294` and `:390`, vendored at
`/home/lu_zero/Sources/portage-3.0.79`). `WorldSelectedSet` (real
portage's `@selected`) is literally just these two chained together
(`files.py:256`).

`em`'s `SetResolver::selected_members` (`portage-repo/src/repo/sets.rs:92`)
already computes the exact same two halves before combining them:

```rust
fn selected_members(&self) -> Result<Vec<GroupEntry<Dep>>> {
    let mut out: Vec<GroupEntry<Dep>> = read_atoms(&self.eroot.join("var/lib/portage/world"))?
        .into_iter()
        .map(GroupEntry::Leaf)
        .collect();
    for set_ref in read_lines(&self.eroot.join("var/lib/portage/world_sets"))? {
        ...
        out.push(GroupEntry::Ref(name.to_string()));
    }
    Ok(out)
}
```

**Plan:** split this into `selected_packages_members` (just the `world`
half) and `selected_sets_members` (just the `world_sets` half, as `@name`
refs — real `WorldSelectedSetsSet` does *not* expand them, it returns the
literal `@name` strings, matching `GroupEntry::Ref` already), have
`selected_members` call both and chain, and add two new `direct_members`
match arms:

```rust
"selected-packages" => self.selected_packages_members(),
"selected-sets" => self.selected_sets_members(),
```

No VDB needed, no `is_world_candidate` change needed (already excluded).
Add unit tests mirroring the existing `selected_cpns_keeps_slotted_...`
style in `maint/world.rs` and/or directly in `sets.rs`'s own test module —
whichever is more natural given how the split lands.

---

## 2. `@live-rebuild` / `@deprecated-live-rebuild`

Real portage: `VariableSet` (`portage/_sets/dbapi.py:146`), configured via
default `sets.conf`:

```
[live-rebuild]
class = portage.sets.dbapi.VariableSet
variable = PROPERTIES
includes = live

[deprecated-live-rebuild]
class = portage.sets.dbapi.VariableSet
variable = INHERITED
includes = bzr cvs darcs git-2 git-r3 golang-vcs mercurial subversion
```

**Confirmed VDB-only** (checked `singleBuilder`, `dbapi.py:222-252`):
`metadata-source` defaults to `"vartree"` when not set in `sets.conf`
(neither of these two sections sets it), so `VariableSet._filter`
(`dbapi.py:170`) reads `PROPERTIES`/`INHERITED` off the **installed
package's own VDB record**, not off the ebuild in the tree. This is a
pure "walk every installed package, check a space-separated field for
intersection with a fixed word list" query — no repo/tree access needed.

Matching semantics (`_filter`, non-`*DEPEND` branch, `dbapi.py:213-219`):
non-empty intersection between the field's space-split values and
`includes` → included. (The `*DEPEND` branch a few lines up doesn't apply
here — `PROPERTIES`/`INHERITED` aren't `*DEPEND` variables.)

**Plan:** this codebase already has `pkg.field(name) -> Result<Option<String>>`
on `InstalledPackage` (`portage-vdb/src/package.rs:67`, used e.g. in
`info.rs`'s `pkg.field("repository")`). A resolver looks like:

```rust
fn variable_set_atoms(vdb: &Vdb, variable: &str, includes: &[&str]) -> Vec<Dep> {
    vdb.packages()
        .into_iter()
        .filter(|pkg| {
            pkg.field(variable)
                .ok()
                .flatten()
                .is_some_and(|v| v.split_whitespace().any(|tok| includes.contains(&tok)))
        })
        .map(|pkg| Dep::parse(&format!("{}:{}", pkg.cpn(), pkg.slot().unwrap_or_default())).expect("..."))
        .collect()
}
```

(Real portage emits `cp:slot` atoms, not full `=cp-version`, matching
`EverythingSet::load`'s `Atom(f"{pkg.cp}:{pkg.slot}")` — check whether an
empty slot (`SLOT="0"` vs unset) needs special-casing; `pkg.slot()`'s
existing behavior elsewhere in this codebase should already answer that.)

The two sets are just two different `(variable, includes)` pairs through
the same function — wire them into whatever shared VDB-set dispatcher
gets built for section 0 above, alongside `preserved-rebuild`.

**Test idea:** same `write_pkg`-style VDB fixture helper used in
`emerge.rs`'s existing tests (`write_pkg(vdb_root, cat, pf)` writes
`SLOT`/`EAPI`) — extend it (or a local variant) to also write `PROPERTIES`
or `INHERITED`, and assert the right subset comes back.

---

## 3. `@module-rebuild` / `@x11-module-rebuild`

Real portage: `OwnerSet` (`portage/_sets/dbapi.py:65`), configured via:

```
[module-rebuild]
class = portage.sets.dbapi.OwnerSet
files = /lib/modules
exclude-files = /usr/src/linux*

[x11-module-rebuild]
class = portage.sets.dbapi.OwnerSet
files = /usr/lib*/xorg/modules
exclude-files = /usr/bin/Xorg
```

This one is **more open than the others — needs a live-verification step
before locking in the exact matching semantics**, not just a port. What's
confirmed from source:

- `mapPathsToAtoms` (`dbapi.py:78`) first `glob.iglob`s each `files`/
  `exclude-files` pattern **against the live filesystem** (so
  `/usr/lib*/xorg/modules` resolves to whatever concrete directories
  currently exist, e.g. `/usr/lib64/xorg/modules`) — this is filesystem
  globbing of the *pattern itself*, not a CONTENTS search.
- The resulting concrete paths are then looked up via
  `vardb._owners.iter_owners(paths)` → `_match_contents`
  (`portage/dbapi/vartree.py:1519`, `_match_contents` referenced at
  `:1598`), which does an **exact CONTENTS-entry path match** (with
  symlink-aware ancestor resolution), not a directory-subtree/prefix
  match. So it matches packages whose CONTENTS has a literal entry for
  that resolved directory path (typically a `dir` entry) — not "any file
  anywhere under that directory."
- `exclude-files` narrows the *result set*: any matched atom that also
  owns one of the excluded paths is dropped (`dbapi.py:104-121`).

**This codebase already has most of the primitive needed:**
`Vdb::owner(&self, file_path: &Utf8Path) -> Option<InstalledPackage>`
(`portage-vdb/src/vdb.rs:83`) and `InstalledPackage::owns(&self, path)`
(`portage-vdb/src/package.rs:241`) do exact-path CONTENTS matching
already — `Vdb::owner` is O(n) over installed packages per call (its own
doc comment says so), fine for a handful of fixed directory paths, no
need for real portage's basename-hash index.

**Before implementing, verify against a real host** (this box has one):
does `/lib/modules` (or whatever directories exist on it) actually carry
a `dir` CONTENTS entry for any installed package, or does the intuitive
"any file whose path starts with `/lib/modules/`" reading turn out to be
closer to what real `emerge --info`/`equery` would show for this set in
practice? If a real out-of-tree-module package is available to test
against (e.g. `x11-drivers/nvidia-drivers`, `app-emulation/virtualbox-
modules` — check what's actually installed first, `em query list` or
similar), compare directly. If exact-match genuinely produces the
(mostly-empty) result real portage's own source implies, implement that;
don't substitute a "more useful"-seeming prefix match without confirming
that's not just papering over a misunderstanding of the real algorithm.

**Plan sketch** (pending the verification above):

```rust
fn owner_set_atoms(vdb: &Vdb, files: &[&str], exclude_files: &[&str]) -> Vec<Dep> {
    let expand = |patterns: &[&str]| -> Vec<Utf8PathBuf> {
        patterns.iter().flat_map(|p| glob::glob(p).ok().into_iter().flatten().flatten())
            .filter_map(|p| Utf8PathBuf::try_from(p).ok())
            .collect()
    };
    let paths = expand(files);
    let excluded = expand(exclude_files);
    // for each installed package, does it own any `paths` entry, and not
    // any `excluded` entry? (own = exact CONTENTS match, per above)
    ...
}
```

Check whether a `glob` crate dependency already exists in the workspace
(`Cargo.lock`/other crates' `Cargo.toml`) before adding one — this
codebase generally prefers reusing an existing dependency over adding a
new one for a single call site.

---

## 4. `@profile` bug fix (bundle-in, already diagnosed)

Documented in `todo/package-sets-support.md`'s "Bug found while auditing"
section — not re-derived here, just linking it in since whoever picks up
this file will be touching the same `direct_members` match block anyway:

- `direct_members("profile")` currently returns `@system ∪ (real
  @profile)` (both `*`-prefixed and plain `packages` lines, undivided).
  Real `@profile` (`ProfilePackageSet`) is *only* the non-`*` advisory
  lines, and only from profiles whose `profile-formats` declares
  `profile-set` — a rare flag essentially never set in practice, so real
  `@profile` is almost always empty.
- `ProfileStack::packages()` (`portage-repo/src/repo/profile.rs:460`)
  already returns `Vec<(bool, Dep)>` with the system/plain split done —
  `direct_members("profile")` just isn't using the bool.
- `profile-formats` is already parsed at the repo `layout.conf` level
  (`Layout::profile_formats: Vec<String>`, `portage-repo/src/repo/
  layout.rs:25`) but **not** currently threaded through per-profile or
  checked against `"profile-set"` anywhere — confirm PMS 5.1's exact
  resolution rule (repo-level default vs. any per-profile override) before
  wiring the gate, since that hasn't been verified yet.
- There's also a mislabeled doc comment to fix in the same pass:
  `portage-repo/src/repo/profile.rs:485`, on `ProfileStack::system_set()`,
  incorrectly claims to match `ProfilePackageSet` (it matches
  `PackagesSystemSet`, i.e. `@system`).

---

## Why `@security` is out of scope here

Real portage: `SecuritySet`/`NewAffectedSet`
(`portage/_sets/security.py:12`), which depends on `portage.glsa` — GLSA
(Gentoo Linux Security Advisory) XML documents, normally synced alongside
the main tree under `metadata/glsa/glsa-*.xml`. Implementing this needs,
at minimum: a GLSA XML parser (`<affected>`/`<package>` version-range
elements), a "which GLSAs are already applied" tracker
(`get_applied_glsas`), and version-range-vs-installed matching logic
(`Glsa.isVulnerable`/`getMergeList`) — genuinely a new subsystem, not a
`SetResolver` match arm. `em glsa <list|check|fix>` already exists as its
own applet (per `todo/for_vibe.md`'s applet catalog) — worth checking
whether that applet already has *any* of this machinery before scoping a
`@security` set implementation, since duplicating it would be wasteful if
so. Left as a separate, dedicated task.

---

## Testing / verification checklist

- Unit tests per set, in the style already established in this file's
  siblings (`maint/world.rs`, `portage-repo/src/repo/sets.rs`,
  `portage-cli/src/emerge.rs`'s `write_pkg`-based VDB fixtures).
- `cargo fmt`, `cargo clippy -p portage-cli -p portage-repo --all-targets`,
  full `cargo test` for both crates — this repo's standing bar (see
  `AGENTS.md`).
- Live-verify with `em --info -v` (landed this session specifically to
  make this checklist item cheap) against a real host: each newly-
  resolvable set should show a sane atom list instead of "not
  resolvable"; the still-deferred ones (`@security`) should still show
  their error, not silently vanish.
- If any of these sets are wired into `expand_sets` (not just
  `resolve_set`), also confirm `em @live-rebuild` (etc.) actually shows up
  correctly in a `-p` plan with the right `TargetOrigin::Set` provenance —
  `expand_sets_resolves_preserved_rebuild_via_the_vdb`
  (`portage-cli/src/emerge.rs:1147`) is the existing precedent test to
  mirror.

## Out of scope for this pass

- `@security` (see above).
- Any actual add/remove-to-world wiring for these — they're all
  non-world-candidate per `is_world_candidate`, so (per real portage) they
  were never supposed to be recordable in `world_sets` in the first place.
  Nothing to do there.
- The tree-rendering rework in `todo/for_maki.md` — unrelated area, don't
  let the two collide if worked in parallel.

## Results

Implemented everything in scope (all four items + the refactor enabler), in
the order `@profile` fix → refactor → `@selected-*` → `@live-rebuild` pair →
`@module-rebuild` pair. Landed on `master` as six commits, split
one-per-concern (each builds at its intermediate state):

| SHA     | Commit |
|---------|--------|
| `3b991d5` | `chore(lint): clear two clippy failures left by the sync-gix handover` |
| `c805c1f` | `fix(sets): @profile respects the profile-set profile-format gate` |
| `b072add` | `feat(sets): @selected-packages and @selected-sets` |
| `349c894` | `refactor(sets): route @preserved-rebuild through a shared resolve_vdb_set` |
| `35b1ee5` | `feat(sets): @live-rebuild and @deprecated-live-rebuild` |
| `059079d` | `feat(sets): @module-rebuild and @x11-module-rebuild` |

(`641dace` is the docs commit that first wrote this Results section.) A
later parallel session then added the `news`/`glsa` applets on top; the two
streams integrate cleanly (full workspace `clippy`/`fmt`/`test`/`doc` green
on the combined tree, 1844 tests).

**Files touched:** `portage-repo/src/repo/{profile,sets}.rs`,
`portage-cli/src/{emerge,maint/sets,maint/world,info}.rs`,
`portage-cli/Cargo.toml` (`glob = "0.3"` — already transitive in `Cargo.lock`,
needed for `OwnerSet`'s `files`/`exclude-files` FS globbing; no other dep
added).

**What landed:**

- **`@profile` fix** — `ProfileStack::profile_set()` returns only non-`*`
  `packages` lines, and only from profile nodes whose enclosing repo declares
  `profile-set` in `profile-formats`. `profile_formats` is resolved per-profile
  by walking up to the nearest `metadata/layout.conf` (mirrors portage's
  longest-path-wins `intersecting_repos`); `with_user_profile` hardcodes
  `["profile-bashrcs", "profile-set"]` for `/etc/portage/profile` (portage's
  `LocationsManager.py:182`). Fixed the mislabeled `system_set()` doc comment
  (it's `PackagesSystemSet`, not `ProfilePackageSet`) and the stale module doc
  in `sets.rs`. **Correction to this file's own claim:** `@profile` is not
  unconditionally empty on stock Gentoo — a site-local `/etc/portage/profile`
  with plain `packages` lines *does* contribute (portage hardcodes that layer
  to `profile-set`). 8 new tests.
- **Refactor** — `maint::sets::resolve_vdb_set(name, eroot) -> Option<Result<Vec<Dep>>>`
  is the single home for all VDB-aware built-ins, called from both
  `emerge::expand_sets` and `maint::world::resolve_set`. `@preserved-rebuild`
  (previously duplicated 4 lines × 2 sites) routed through it; VDB opened once
  per call, guarded by `is_vdb_set_name` so `@system`/`@world`/user sets never
  require a readable `var/db/pkg`. 2 dispatch tests.
- **`@selected-packages` / `@selected-sets`** — split `selected_members()` into
  `selected_packages_members` (world file) + `selected_sets_members`
  (`world_sets` refs as `GroupEntry::Ref`, expanded by the resolver). 4 tests.
- **`@live-rebuild` / `@deprecated-live-rebuild`** — `variable_set_atoms(vdb,
  variable, includes)` over the VDB; `(PROPERTIES, [live])` and `(INHERITED,
  [LIVE_ECLASSES…])`. Atoms emitted as `cat/pkg:{main_slot}` (portage's
  `_pkg_str.slot` is main-slot-only; `"0"` fallback). 5 tests.
- **`@module-rebuild` / `@x11-module-rebuild`** — `owner_set_atoms` via
  `OwnerSet` semantics. **Live-verified first** (per the plan's emphasis):
  `emerge -p @module-rebuild` returns empty on this host because no installed
  package has a CONTENTS entry for `/lib/modules` (despite the dir being
  populated with hand-built kernel modules) — confirming **exact-path match,
  not subtree/prefix**. `contents_contains` matches any CONTENTS kind
  (incl. `dir` — `InstalledPackage::owns` is `Obj`/`Sym`-only, so a dedicated
  helper was needed) and is symlink-aware via `canonicalize` (so a
  `/lib/modules` query matches a `/usr/lib/modules` entry when
  `/lib` → `/usr/lib`). 5 tests.

**KnownSets** now hardcodes all five resolvable VDB sets (not just
`preserved-rebuild`) so `em --info -v` advertises them on `em`-only roots too.

**Divergences from the plan, all minor:**
- The `@profile` gate is per-profile (faithful walk-up to the nearest
  `layout.conf`) rather than a single repo-level value — costs one tiny file
  read per profile at stack-build, chosen for correctness on multi-repo stacks.
- `@module-rebuild`'s symlink-aware match uses full-path `canonicalize` rather
  than portage's exact parent-inode comparison — equivalent for the
  merged-usr `/lib`↔`/usr/lib` case (the one that matters), simpler to express.
- Did **not** reuse `InstalledPackage::owns` for `OwnerSet` (it excludes `dir`
  entries and does no symlink resolution); added `contents_contains` instead
  rather than change `owns`'s established `qfile`-facing semantics.

**Testing:** 22 new unit tests across `portage-repo` (12) and `portage-cli`
(10). Full pre-PR checklist green: `cargo fmt --all --check`, `cargo clippy
--workspace --exclude portage-bench -- -D warnings` (zero new diagnostics —
the two pre-existing ones, `set_test_git_identity` dead code and
`depgraph/output.rs` type-complexity, are unrelated and were not touched),
`cargo nextest run --workspace --exclude portage-bench` (1819 passed, 5
skipped), `cargo test --workspace --exclude portage-bench --doc`,
`RUSTDOCFLAGS='-D warnings' cargo doc --workspace --exclude portage-bench
--no-deps`.

**Live parity check** (this Gentoo host, `em` built `--profile quick`):

| set | `em --info -v` | real `emerge -p` |
|-----|----------------|------------------|
| `@profile` | 0 atoms | empty (matches — stock gentoo has no `profile-set`) |
| `@selected-packages` | 113 | = world file line count (emerge itself fails resolve on a masked pkg, unrelated) |
| `@selected-sets` | 21 | = members of the one `world_sets` ref (`@openwrt-prerequisites`) |
| `@selected` | 134 | = 113 + 21 (no overlap) |
| `@live-rebuild` / `@deprecated-live-rebuild` | 0 | empty |
| `@module-rebuild` / `@x11-module-rebuild` | 0 | empty |

`em -1p @selected-packages` reaches the resolver with `TargetOrigin::Set`
provenance (the masked-pkg error cites `required by "@selected-packages"`),
confirming the `expand_sets` wiring end-to-end.

**Still deferred (unchanged):** `@security` (needs a GLSA subsystem — separate
task; worth checking whether the existing `em glsa` applet already has any
reusable machinery before scoping it).
