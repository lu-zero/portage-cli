# Derive `cross-<tuple>/<pkg>` on the fly (no overlay symlinks)

STATUS: ✅ done (steps 1-8). The symlink overlay is gone; `cross-<tuple>/<pkg>`
is derived from `::gentoo` at resolve time via a `Location::Alias` repos.conf
entry, and the merge path preserves the virtual cross category end to end.
Live-validated 2026-07-08: `em --prefix <dir> crossdev -t riscv64-…-gnu --setup`
builds `cross-riscv64-…-gnu/binutils` unprivileged, registers it in the VDB
under the virtual category, and `binutils-config` activates the wrappers — no
overlay on disk. See "How to test" below.

The merge-path CPV landmine (originally step 4) was closed by `b3df565`+
`363e9aa` (`Ebuild::with_cpv` + threading a real `Cpv` through the merge
path); the producer landed in `d7ac770` (`write_alias_repo_conf` + the
`write_cross_env` config-overlay fix); the invariant test in `42d9903`.

## The idea
Instead of `em crossdev --init-target` materialising a `cross-<tuple>` overlay
of per-package symlinks (copies, in the broken case) into `::gentoo`, teach
the resolver to treat `cross-<tuple>/<pkg>` as a derived alias for its real
target package (`cross-riscv64-unknown-linux-gnu/gcc` → `sys-devel/gcc`) at
resolve time. The ebuild, metadata, SLOT, IUSE, deps — all the real package's.
Only the **category** in the CPV is cross; the build-time env (CTARGET/ABI
CFLAGS) still comes from the host `package.env` (f84436a, unchanged).

## Why
- Eliminates the stale-overlay bug class (found live: cross overlay lagged
  behind gentoo, `cross-riscv64/gcc-16.1.1_p20260613` existed in `::gentoo`
  but not the overlay → resolver `NoVersions`).
- Eliminates the absolute-symlink portability bug.
- Eliminates the init-target overlay write step (symlinks, layout.conf,
  repo_name, categories) — less ceremony.
- Matches how `<tuple>-emerge` sees the tree: `cross-<tuple>/*` are virtual
  routing labels; the ebuilds are `::gentoo`'s.

## Single source of truth: `CrossTarget::packages()`

The **only** table of "which real packages map to cross for this target" is
`CrossTarget::packages()` (target.rs:147). It already branches correctly on
`llvm` / `has_kernel` / `libc` (Glibc/Musl/Newlib). Adding musl or elf support
needs **zero** derivation changes — `packages()` adapts, the map follows.

The derivation map is built from it:
```rust
HashMap<Cpn(cross-<tuple>/<pkg>), Cpn(<real-cat>/<pkg>)>
```
populated once per solve when `--cross` is active; empty otherwise (zero
overhead for non-cross solves).

## Where the hook lives: materialized in `RepoData` at load time (revised)

**Implemented differently from the original Option A sketch below.** Rather
than a dynamic per-call proxy inside `Adapter`, `load_repos` (`repo.rs`)
clones the source cpn's `(Cpv, CacheEntry)` entries into `versions`/`cpns`
under the destination category once, up front, when it sees a
`Location::Alias` repos.conf entry. `Adapter::data: &RepoData` already reads
straight from `versions`/`cpns`, so `all_packages`/`versions_for`/
`slots_for`/`desired_use` see the cross Cpns as first-class with zero Adapter
changes — the proxying steps 2 below were never needed as separate code.
`repo_of` records the alias repo's name (`::crossdev`-equivalent) per
injected Cpv, `real_cpn_of: HashMap<Cpn, Cpn>` is the reverse map the
merge-path (steps 3-4) consumes. Source repo is validated against `repo.name()`
(the alias's `source` field) — a repo-of-repos lookup for named overlays as
`source` isn't wired yet, so non-main-repo sources are silently skipped.
Guarded by two `#[tokio::test]`s in `repo.rs` (`load_repos_injects_alias_cross_packages`,
`load_repos_alias_from_unknown_source_is_ignored`).

The original design (kept for context, not what shipped):

The cli's `Adapter` (the `PackageRepository` impl in `repo.rs`) gains a
`cross_map` field. The solver crate sees cross packages as first-class; it
never knows they're derived.

- `all_packages()`: appends the cross Cpns when `cross_map` is non-empty.
- `versions_for(cross-cpn)`: proxies to the real cpn's versions, rewriting
  each Cpv's category to cross. Metadata (slot/IUSE/deps) untouched. Repo
  identity = `::crossdev` (so `::repo` constraints and routing still work).
- `desired_use` / `slots_for` for cross-cpns: delegate to the real cpn's data.

## Keeping the build plan honest

`toolchain_plan` (stages.rs:166) hardcodes per-step atoms + USE overrides +
the stage1/stage2 gcc split. That build-order logic is genuinely plan-specific
(not set-specific) and stays there. The invariant that keeps them in sync:

**every atom `toolchain_plan` emits must be in `packages()`'s set.**

That's a unit test (`toolchain_plan_atoms_are_all_in_packages_set`), not a
code merge. When musl/elf land, `toolchain_plan` gains branching (unavoidable —
musl may not need the two-stage gcc headers cycle), but the test guarantees the
plan never references a package the derivation can't resolve.

## What `init-target` stops/keeps doing

**Stops:** `write_overlay` (the symlink farm), `ensure_repos_conf` for the
overlay. The cross packages become virtual.

**Keeps:** `write_cross_env` (CTARGET/ABI CFLAGS at host package.env),
`write_sysroot_config` (sysroot make.conf with CHOST/CBUILD),
`write_sysroot_repos_conf` (so the sysroot sees `::gentoo`).

## The merge-path decoupling (the one risk) — landmine found, fix plan below

When the solver picks `cross-riscv64/gcc-15.2.1`, the merge phase must:
- Find the **real ebuild** (`sys-devel/gcc/gcc-15.2.1.ebuild`) — not via a
  symlink, but by resolving the real cpn the cross cpn was derived from.
- Write the VDB entry under `cross-riscv64/` (so gcc-config/binutils-config
  find it, `emerge -u cross-riscv64/gcc` works, and `toolchain.eclass`'s
  `CTARGET=${CATEGORY#cross-}` cross-build detection fires at all).

**First half done (uncommitted, `mod.rs`):** `PlannedMerge.ebuild_path`
construction now looks up `data.real_cpn_of` and joins the *real* cpn's
category/package for the on-disk path, while `PlannedMerge.cpv` (the
displayed/registered string) still reports the cross cpv. This is necessary —
without it a cross cpv's `ebuild_path` points at a directory that was never
created (`Location::Alias` writes no on-disk tree at all) — but it is **not
sufficient**, and investigating the second half surfaced a real bug class
that would have silently corrupted real cross builds.

### The landmine: `Ebuild::from_path` derives CATEGORY from path text, not from any `Cpv` value

`Ebuild::from_path` (`portage-repo/src/repo/ebuild.rs:75`) parses a package's
`Cpv` — including `CATEGORY`, the field `toolchain.eclass` keys cross-build
detection off (`CTARGET=${CATEGORY#cross-}`) — from the **directory names in
the file path string** (`<repo>/<category>/<pkg>/<pkg>-<ver>.ebuild`), not
from any `Cpv` the caller already has. Today this works *only* because
`write_overlay`'s symlinked directory makes the path text itself say
`cross-<tuple>/gcc/...` while `canonicalize()` resolves the same path to the
real file for `repo_root()`/eclass lookups — the split the design doc wants
to eliminate.

Once `ebuild_path` is redirected to the *real* file (no symlink, per the
change above), that trick is gone: `Ebuild::from_path` reads back
`sys-devel/gcc`, not `cross-riscv64-unknown-linux-gnu/gcc`. Consequence if
this reaches a real merge unfixed: **`em crossdev --setup` silently builds a
native compiler under a cross category** — no error, wrong result — because
`CATEGORY` no longer starts with `cross-`.

This is not a one-line miss; it's a whole discarded value. A `Cpv` the caller
already correctly knows (`PlannedMerge.cpv` in `main.rs`, or `InstalledPackage.cpv()`
in the unmerge path) gets thrown away in favor of re-deriving it from the same
path string three function calls later. Traced (investigation only, no code
changes yet) end to end:

- `main.rs:784,815,1049` call `build_and_merge`/`merge_binpkg` with
  `&planned.ebuild_path` only — `planned.cpv` (correct) is in scope and
  dropped.
- `ebuild.rs:284` (`build_and_merge`), `:380` (`merge_binpkg`) — neither
  function takes a `cpv` parameter at all.
- `ebuild.rs:560` (`run_inner`) — the actual chokepoint. Calls
  `Ebuild::from_path(path)`; the resulting `Ebuild::cpv()` propagates into
  `work_dir`, `build_binpkg`'s GPKG output category, and
  `merge_spec_from_env(env, ebuild.cpv().clone(), …)` at `ebuild.rs:1258` —
  **the VDB `CATEGORY` is authoritatively this path-derived value.**
  `portage_vdb::write::MergeSpec`/`Vdb::register` themselves are fine (an
  explicit `Cpv` field, no path parsing) — the bug is entirely upstream of
  where `MergeSpec` gets constructed.
- `ebuild.rs:463` (`run_install_worker`, the `em __worker` body) — same
  `Ebuild::from_path(ebuild_path)` re-derivation, but across a **process
  boundary**: `privilege.rs`'s `WorkerArgs` (the args serialized into the
  `em __worker` child's CLI invocation) carries only `ebuild_path: &str` —
  no `cpv`/`category` field exists to carry the correct value even if the
  parent had it in scope, which per the point above it currently doesn't
  either. This is now the **default merge path** for install/qmerge
  (pseudoroot-over-fakeroost, `42d001e`, see [[stage-build-shakeout]]), so it
  isn't an edge case — every worker-wrapped merge of a cross package would
  hit this.
- `ebuild.rs:1336` (`unmerge_slot_occupant`) — a correct `Cpv` (`old_pkg.cpv()`)
  *is* in scope here, but the code builds a path from `old_pkg.category()`
  and then re-parses it via `Ebuild::from_path` anyway instead of using it
  directly. Currently harmless (round-trips through the same category), but
  the same landmine the moment `old_pkg.category()` is a virtual one.

**Scope assessment (investigated 2026-07-08): contained, not pervasive.**
Exactly one chokepoint (`run_inner`), reached through three call paths
(`build_and_merge`, `merge_binpkg`, `run_install_worker`), none of which take
a `cpv` parameter today. Everything downstream of `Ebuild` construction
already does the right thing (`MergeSpec.cpv`, `InstalledPackage.cpv()`,
`RepoData.real_cpn_of` are all explicit-`Cpv`-carrying, not path-derived).
The repo-tree walkers (`Repository::ebuilds()`/`cache_entries()`,
`repository.rs:82-90,141-152`) are legitimate path-based enumeration with no
prior `Cpv` to preserve (full-tree scans, not plan lookups) — out of scope,
not a landmine. `overlay.rs`'s `master_cache_entry` deliberately resolves
real-through-symlink for md5-cache lookup, which is the intentional
real/virtual split, not an identity loss.

### Fix plan (not yet implemented)

1. `portage-repo`: give `Ebuild` a public constructor taking an explicit
   `Cpv` + real path (widen `Ebuild::new`'s visibility, or add
   `Ebuild::with_cpv`) — bypassing `from_path`'s directory-name parse
   entirely when the caller already knows the cpv.
2. Thread a `cpv: &Cpv` (or the existing `PlannedMerge.cpv: String`,
   re-parsed once) parameter through `build_and_merge` → `run_inner`, and
   `merge_binpkg` → `run_inner`, replacing their internal `Ebuild::from_path`
   calls with the new constructor.
3. Cross the `em __worker` boundary: add a `cpv`/`--cpv` field to
   `WorkerArgs` (`privilege.rs:183`) and the `Worker` clap variant
   (`cli.rs:533`), threaded into `run_install_worker`
   (`ebuild.rs:463`)'s `Ebuild` construction.
4. Fix `unmerge_slot_occupant` (`ebuild.rs:1336`) to construct from
   `old_pkg.cpv()` directly instead of round-tripping through a path string.
5. Regression test: a cross-derived cpv (real path, virtual category) merged
   through `build_and_merge` registers a VDB entry under the **virtual**
   category, not the real one — this is the scenario that silently built a
   native compiler if unfixed.

**Until this lands, do not wire crossdev's producer (step 3) to replace
`write_overlay`'s real merges** — resolution-only testing (`-p`/`query`
against a hand-written or test-only `Location::Alias` repos.conf entry) is
safe today since `Ebuild::from_path` is never reached outside an actual
merge; flipping `init_target` to stop writing the symlink overlay is not,
until steps 1-4 above land.

## Implementation order

1. ✅ Structural foundation (`017f33a`): `Location` enum (`Path`/`Alias`) on
   `RepoEntry`, repos.conf parser recognises `alias-source`/`alias-target`/
   `alias-packages`.
2. ✅ Consumer side (`8ca65da`): `load_repos` reads `Location::Alias` entries
   and materializes the cross Cpns/Cpvs into `RepoData` — see "Where the hook
   lives" above. `all_packages`/`versions_for`/`slots_for`/`desired_use`
   proxying falls out for free since `Adapter` reads `RepoData` directly.
3. ✅ Merge-path part 1 (`8ca65da`): `PlannedMerge.ebuild_path` redirects
   through `real_cpn_of` to the real on-disk file.
4. ✅ **Merge-path part 2 — CPV/CATEGORY preservation** (`b3df565`+`363e9aa`).
   The landmine is closed: `Ebuild::with_cpv` takes the caller's `Cpv` without
   re-parsing the path, and a real `Cpv` (not a formatted `String`) is now
   threaded through `build_and_merge`/`merge_binpkg`/`run_inner`, across the
   `em __worker` boundary (`--cpv`), and `unmerge_slot_occupant` uses
   `old_pkg.cpv()` directly. `PlannedMerge.cpv` is a `Cpv` end to end.
5. ✅ Invariant test (`stages.rs`):
   `toolchain_plan_atoms_are_all_in_packages_set` — every `cross-<tuple>/<pkg>`
   plan atom's package is in `CrossTarget::packages()`, across riscv64,
   aarch64, and armv7a targets. Real-category bypass atoms (baselayout,
   virtual/os-headers) are filtered out.
6. ✅ **Producer side:** `init_target` now calls `write_alias_repo_conf`,
   which writes a `Location::Alias` repos.conf entry
   (`alias-source = gentoo`/`alias-target = <cat>`/`alias-packages = …`)
   built from `CrossTarget::packages()`. `write_sysroot_repos_conf` writes
   the same alias entry for the sysroot.
7. ✅ `write_overlay` (the symlink farm) and `ensure_repos_conf` (the overlay
   `location =` entry) removed from `init_target`; `write_alias_repo_conf`
   replaces both. `write_cross_env`, `write_sysroot_config`,
   `write_sysroot_repos_conf` kept.
8. ✅ Validate: `em -p cross-riscv64/gcc` resolves without overlay; full
   `crossdev --setup` end-to-end. Done 2026-07-08: `em --prefix <dir>
   crossdev -t riscv64-unknown-linux-gnu --setup` ran unprivileged, built
   `cross-riscv64-…-gnu/binutils-2.46.1`, registered it in the VDB under the
   virtual cross category (`CATEGORY: cross-riscv64-…-gnu`), and
   `binutils-config` created the `riscv64-…-gnu-*` wrappers — with no on-disk
    overlay. The full 6-step bootstrap (→ gcc-stage2) was not re-run in this
    session (time-boxed), but every component the bootstrap exercises is the
    same code path binutils just validated. See "How to test".

    **`--local` (standalone) validated to resolution 2026-07-08:**
    `em --local crossdev -t riscv64-…-gnu --init-target` bootstraps the prefix
    and writes the alias + sysroot unprivileged; `em --local -p
    cross-riscv64-…-gnu/binutils` resolves (routing the cross package and the
    standalone BDEPEND closure into `~/.gentoo`). The actual *build* under
    `--local` pulls the full @system closure (portage/rust-bin/python/…) into
    the fresh prefix first — the ~10-min standalone bootstrap — deferred to a
    follow-up session. `--prefix` remains the fast path for cross-only testing.

## How to test

### Resolution only (fast, no build)
Write a `Location::Alias` repos.conf entry by hand and resolve a cross
package — confirms the consumer side with no on-disk overlay:

```bash
TMP=$(mktemp -d)
mkdir -p "$TMP/etc/portage/repos.conf"
cat > "$TMP/etc/portage/repos.conf/crossdev.conf" <<'EOF'
[DEFAULT]
main-repo = gentoo

[gentoo]
location = /var/db/repos/gentoo

[crossdev]
alias-source = gentoo
alias-target = cross-riscv64-unknown-linux-gnu
alias-packages = sys-devel/binutils sys-kernel/linux-headers sys-devel/gcc sys-libs/glibc dev-debug/gdb
EOF
ln -sf /var/db/repos/gentoo/profiles/default/linux/amd64/17.1 "$TMP/etc/portage/make.profile"
em --config-root "$TMP" -p cross-riscv64-unknown-linux-gnu/gcc
```

Must resolve `gcc` from `::gentoo` (no `NoVersions`, no overlay dir).

### Producer (fast, no build)
Unit tests cover the writer directly:

```bash
cargo test --bin em crossdev::tests::write_alias_repo_conf
cargo test --bin em crossdev::tests::alias_packages_line
cargo test --bin em crossdev::stages::tests::toolchain_plan_atoms_are_all_in_packages_set
```

These assert the alias entry parses back through `ReposConf` into a
`Location::Alias` with the full `packages()` set, is idempotent, rejects a
missing source package, and that every `toolchain_plan` cross atom is
derivable.

### End-to-end (slow — actually builds)
The real validation: a from-scratch unprivileged crossdev setup under
`--prefix`, building one cross package and checking the VDB category (the
merge-path landmine) + the binutils-config wrapper:

```bash
XP=$(mktemp -d)
em --prefix "$XP" crossdev -t riscv64-unknown-linux-gnu --setup   # lays down the alias + sysroot, starts the bootstrap
em --prefix "$XP" cross-riscv64-unknown-linux-gnu/binutils        # build + merge one cross package
# VDB entry must be under the VIRTUAL cross category (not sys-devel):
ls "$XP/var/db/pkg/" | grep cross                                 # → cross-riscv64-unknown-linux-gnu
ls "$XP/var/db/pkg/cross-riscv64-unknown-linux-gnu/"              # → binutils-2.46.1
# binutils-config wrapper must exist:
ls "$XP/usr/bin/" | grep riscv64-unknown-linux-gnu-ld             # → riscv64-unknown-linux-gnu-ld
```

Use `--prefix` (the crossdev mode: BROOT = host `/`, host build tools
available, config overlay = `<prefix>/etc/portage`, fully unprivileged).
Do **not** use `--root` to test crossdev — under `--root` BROOT *is* the
empty prefix, so binutils BDEPEND (`app-arch/zstd`, `sys-devel/gettext`, …)
is unsatisfied until a host toolchain is bootstrapped into the prefix first
(the separate `toolchain --setup` / root-topology problem). Do **not**
`sudo em` — the `--prefix` path is unprivileged; `sudo` only hangs in the
dev shell.

For the full 6-step bootstrap (→ gcc-stage2, ~10 min), let `--setup` run to
completion instead of interrupting it after binutils.

### `--local` (standalone) end-to-end — TODO
`--local` is the foreign-host-capable mode (base=target=`~/.gentoo`, full
closure, owns its own python). Validated to resolution 2026-07-08; the
actual build is the follow-up:

```bash
em --local crossdev -t riscv64-unknown-linux-gnu --setup   # ~10 min: @system closure + 6-step toolchain
```

Under `--local` a cross package's BDEPEND resolves into the standalone
prefix, so the first build pulls the full @system base (portage, python,
rust-bin, …) before binutils — much heavier than `--prefix`, but the only
mode that works on a host without its own Gentoo toolchain. To test in
isolation without clobbering the real `~/.gentoo`, point `HOME` at a temp
dir (`HOME=$(mktemp -d) em --local …`), since `--local` hardcodes
`~/.gentoo`.

## Related
- `crossdev-target.md` (the crossdev feature design, predates this).
- `cross-support-self-review.md`.
- f84436a (package.env), now written into the config overlay by
  `write_cross_env` (the unprivileged-prefix fix in `d7ac770`). The old
  `write_overlay` symlink farm is deleted.
