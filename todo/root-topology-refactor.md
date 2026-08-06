# Root topology refactor — tracked tasks

Design doc: [`docs/root-topology.md`](../docs/root-topology.md). This file
tracks the implementation work it implies. Status: 🔴 not started · 🟡 partial · ✅ done.

---

## Current plan (2026-08-05 reframing)

A full code census overturned the original premise (below) that this
refactor's goal is "replace the `Roots` flat bag with a `RootTopology`/
`RootSet` enum." Three findings reframe it:

1. **That enum never landed.** The only trace is a private `RootSet`
   (`portage-cli/src/cli.rs:197`) whose `Dual`/`Overlayed` variants are
   `#[allow(dead_code)]`. It was deliberately scoped down on 2026-07-09
   to `satisfaction_root(DepClass)` as two fields on the existing struct
   — "same payoff, far less churn." That was the right call: the
   filesystem-role question ("which path answers which role?") is
   essentially solved.
2. **`satisfaction_root(DepClass)` works** (`portage-resolve/src/roots.rs:232`,
   17 lines, 16 call sites). `Roots` carries 7 path fields + 3 bools
   (not "five `Option<PathBuf>`" as the docs still claim).
3. **The acute pain (the crossdev guard-narrowing instability in the
   2026-08 review) is a *different* problem:** per-package **build-class**
   discrimination in the build shell, not per-invocation topology. The
   build shell re-derives "is this a host-class or target-class build?"
   from `cross_host_tool_tuple` (a `category.strip_prefix("cross-")` +
   name allowlist, `portage-repo/src/build/shell.rs:1584`), the
   `CTARGET`/`TARGET_ABI`/`CHOST`/`CBUILD` shell-env vars, and a runtime
   `bypass_cross_root: bool` thread — across 5 special-casings in
   `shell.rs` plus the `portage-cli/src/setup.rs:145` bashrc guard.
   Every "narrow the guard again" commit is one of these re-derivations
   gaining another term.

So: kill the shadow-discrimination (centerpiece), then the slow-burn
type-discipline cleanup, then reconcile the docs. The full enum
migration of all 69 `.roots()` callers is **not recommended** — high
churn, low payoff now that `satisfaction_root` exists.

### Track A (centerpiece): a typed `BuildClass`, planner-stamped

> **2026-08-06 reversal.** Track A **landed**, but re-review against
> bash-crossdev concluded that a full `BuildClass` for `cross-*` /
> `cross_llvm-*` packages is between pointless and dangerous: those atoms
> already get a durable host/target identity via **package.env** (host
> emerge path), and the stamp is a second authority that can override the
> env sniff. Plan to drop CrossTool stamps and keep at most a narrow
> HostCodegen flag: **[[drop-buildclass]]**. Letter/env matrix:
> [`docs/bash-crossdev-matrix.md`](../docs/bash-crossdev-matrix.md). The
> A1–A3 history below stays as the audit trail of what shipped.

The planner already *knows* each entry's class — it has `MergeRoot`
(`portage-solver/src/solution.rs:16`), `bypass_cross_root`, the
`PackageArch` table (`portage-cli/src/crossdev/target.rs:196`), and the
`cross-<tuple>` category — then throws it away, forcing the build shell
to re-derive it. Stamp a typed enum onto each plan entry and read it
directly:

```rust
enum BuildClass {
    NativeHost,                              // unsatisfied BDEPEND → BROOT
    NativeTarget,                            // plain em/--root/--prefix target pkg
    CrossTarget  { triple: String },         // --target foreign-arch pkg (CBUILD≠CHOST)
    CrossTool    { triple: String },         // cross-<tuple>/{binutils,gcc,…} & libc/headers
}
```

Replaces every shadow: `cross_host_tool_tuple` → `CrossTool`;
`bypass_cross_root: bool` (8 sites) → planner stamps `CrossTool`;
`CTARGET`/`TARGET_ABI`/`CHOST==CBUILD` sniff (`shell.rs:1368`,
`setup.rs:145`) → `match build_class`; `build_sysroot.is_none()`
two-meaning-`None` (`shell.rs:1911`) → the arms carry the triple;
`PackageArch` becomes the input to `BuildClass`, not a parallel vocab.

- ✅ **A1 (landed)** Define `BuildClass` (`portage-solver/src/solution.rs`)
  + stamp it next to `MergeRoot` on `SelectedPackage`. Classification lives
  in `BuildClass::classify(cpn, merge_root, cross_active, is_cross_arch)`,
  invoked at the pubgrub→`SelectedPackage` conversion (`to_selected`,
  `portage-atom-pubgrub/src/solver_impl.rs`) where the entry identity and
  cross context are both in scope. Parallel field, no shell changes;
  pinned by `solution::tests::*` (4 cases). The libc/headers `CrossTool`
  case stays `NativeTarget` here (it needs `bypass_cross_root`, not in the
  solver) and folds in at A3.
- ✅ **A2a (landed)** Add a typed `build_class: Option<BuildClass>` field +
  `set_build_class` setter to `EbuildShell` (`portage-repo/src/build/shell.rs`),
  and replace the 4 `cross_host_tool_tuple` read sites (PATH prepend, EPREFIX
  flip, ESYSROOT doubling, `-idirafter` host fallback — all `CrossTool`-gated)
  with typed `matches!(build_class, BuildClass::CrossTool{..})` reads. `Option`
  so `None` falls back to deriving `CrossTool` from the ebuild's `cross-<tuple>/`
  category — the exact current logic, name-allowlist preserved. Pure refactor,
  no behaviour change, all 442 portage-repo tests green. `portage-repo` gains
  the lightweight `portage-solver` vocab dep (precedent: `portage-atom-pubgrub`).
- 🟡 **A2b (core landed; worker deferred)** The planner-stamped `build_class`
  now flows to the shell for the in-process build path: `PlannedMerge`
  (`query/depgraph/mod.rs`) carries `build_class` (computed once at
  construction via `BuildClass::classify` with the depgraph's cross context);
  `BuildAndMerge` + `RunInner` (`ebuild.rs`) thread it; `run_inner` calls
  `shell.set_build_class(...)` when `Some`. The 4 in-process phase groups
  (FetchOnly/BuildOnly/Compile/Full) pass the planner value; the standalone
  `em ebuild` debug path, the `__worker` install child, the `merge_binpkg`
  binpkg path, and the unmerge path pass `None` (shell falls back to the
  `cross-` category — correct for every current package, since the planner
  value only diverges from the category once A3 widens the enum for
  libc/headers). `BuildClass` re-exported via `portage-atom-pubgrub`
  (alongside `MergeRoot`). Regression-safe via the `Option` fallback.
- ✅ **A2b-worker (landed)** `BuildClass: Display + FromStr` (stable tokens
  `native-host`/`native-target`/`cross-target[:<triple>]`/`cross-tool-host:<triple>`/
  `cross-tool-target:<triple>`), threaded across the `__worker` subprocess via a
  hidden `--build-class=` flag: `WorkerArgs` → `build_worker_command` → the
  `__worker` CLI → `run_install_worker` → `run_inner` (now `Some` instead of
  `None`). `build_and_merge`'s compile→worker handoff passes the planner value;
  `merge_binpkg`'s handoff still `None` (deferred — separate `MergeBinpkg`
  struct). Offline-verified (Display/FromStr round-trip tests + compile-correct
  wiring + behaviour-equivalence: `Some(CrossTool*)` vs `None`→category-fallback
  agree for every `cross-` package); in-process path live-confirmed. **Caveat:**
  the `__worker` subprocess itself isn't live-exercised here — the
  unprivileged `--prefix` sandbox has no privilege backend (install runs
  in-process); a privilege-backed (sudo/pseudoroot) sandbox is needed to
  observe the `--build-class` flag live. Unblocks A3b-③ (the bashrc re-key).
- ✅ **A3-step1 (landed)** Widened `BuildClass::CrossTool` into
  `CrossToolHost{triple}` + `CrossToolTarget{triple}`, split the way bash
  crossdev does (`/usr/bin/crossdev` `set_env` `case ${l} in K|L) ... *)`): the
  libc (glibc/musl) + kernel-headers are the **target** sysroot libraries
  (`CrossToolTarget` — they ARE `CTARGET` code); everything else —
  binutils/gcc/gdb/wrappers and every `--ex-pkg` extra — defaults to **host**
  (`CrossToolHost` — host binaries generating target code). `classify` inverted
  accordingly (default Host, explicit Target = `{linux-headers, glibc, musl}`);
  the 4 A2a shell fixups (PATH/EPREFIX/ESYSROOT/`-idirafter`) narrowed to
  `CrossToolHost` only. This also **restores pre-A2b behaviour** for sysroot
  libs: A2b-core's category-only `classify` had made those fixups fire for
  `cross-<tuple>/glibc` (the old `cross_host_tool_tuple` name-allowlist used to
  exclude it); narrowing to `CrossToolHost` excludes them again. Caveat: the
  libc name varies per target, so the name set is an approximation of
  crossdev's planning letter — the principalled source is
  `CrossTarget::packages()`'s `PackageArch`; routing the refinement through
  that (at the depgraph layer) is a follow-up.
- **A3b-① (analysed → intentionally not pursued as a behaviour change):**
  retire `bypass_cross_root: bool` (16 sites) + stamp the *native* libc/headers
  steps `CrossToolTarget`. Audit of every `build_class` consumer shows the
  native flip is a **no-op**: all consumers are either the §4 block / bashrc
  (gated behind `chost != cbuild || build_eprefix`, which is false for a plain
  native `--root`), `CrossToolHost`-only matches (irrelevant to the
  `NativeTarget`↔`CrossToolTarget` distinction), or the `EM_BUILD_CLASS` export
  (read only by the `EPREFIX`-gated bashrc). So flipping native `sys-libs/glibc`
  to `CrossToolTarget` changes the token but no observable behaviour — and
  doing it would create a latent surprise if a future consumer adds a
  native-`--root`-active arm. Left as `NativeTarget`. The `bypass_cross_root`
  flag itself is clean (one load-bearing read at `emerge.rs:325`, not a
  multi-site shadow like `cross_host_tool_tuple` was); an audit showed full
  removal just relocates it (the staged driver fundamentally must signal
  "merge to the outer EROOT"), so the cleanup landed was a **rename to
  `use_outer_eroot`** — self-documenting at every call site (the historical
  "forgot to set it true" bug class becomes harder to miss).
  ② ✅ **(landed)** rewire the §4 toolchain-var sniff (`shell.rs:init_build_env`)
  to read `build_class` for the `CrossTool` family (`CrossToolHost`→`${CHOST}-*`,
  `CrossToolTarget`→`${CTARGET}-*`+`BUILD_*`), falling back to the
  `TARGET_ABI`/`CTARGET` env sniff for unhooked paths (`__worker` install,
  standalone). The gate's `build_eprefix.is_some()` double-duty (crossdev
  libc/headers under `--prefix` + native-prefix compiler activation) is
  preserved by changing only the `tool_tuple` *derivation*, not the gate.
  Live-confirmed: `cross-riscv64…/glibc` rebuild passes with the
  `build_class`-driven `tool_tuple`; 2 new unit tests pin the typed path
  (incl. overriding a conflicting `CTARGET` env);
  ③ ✅ **(landed)** re-key the `setup.rs:145` `BASHRC_PREFIX` guard on
  `EM_BUILD_CLASS` (exported by `run_phase` from the resolved `BuildClass`):
  inject host search paths for host-side packages
  (`native-host`/`native-target`/`cross-tool-host:*`), skip for target-code
  producers (`cross-tool-target:*` = libc/headers sysroot lib,
  `cross-target*` = genuine `--target`). Replaces the old
  `CBUILD==CHOST && (CTARGET empty || TARGET_ABI set)` env sniff with the
  typed value. Live-confirmed: `cross-riscv64…/gcc` (CrossToolHost, fires) and
  `cross-riscv64…/glibc` (CrossToolTarget, skips) rebuild cleanly; 4
  `overlay_bashrc_*` unit tests re-keyed on `EM_BUILD_CLASS`.
- **A4** Lift `CHOST`/`CBUILD`/`CTARGET` out of shell-env rediscovery
  into a typed `Triples { build, host, target }` constructed once at
  profile load (`portage-repo/src/build/profile.rs:2339`), threaded with
  `Roots`/`BuildClass`. `is_cross_arch: bool` stays on `Roots` for
  `satisfaction_root`'s `IDEPEND` cell but becomes `build != host`.

Gate (must stay green throughout): the root-routing tests in
`cli.rs` (13), `portage-repo/src/build/shell/tests.rs` (11),
`portage-atom-pubgrub/src/provider/tests.rs` (11), plus
`root_aware.rs`/`bdepend_avail.rs`/`installed.rs`/`merge/mod.rs`.

### Track B (slow-burn): topology type-discipline + `DepStampPolicy`

- ✅ **B1 (landed)** `DepStampPolicy` extraction. The two stamp routes
  (`cross_target_runtime_deps` / `host_native_deps`) now share one
  `stamped_deps` body parameterised by a `DepStampPolicy { runtime_stamp,
  broot_unsatisfied_stamp, include_depend, include_binclude }` — so the two
  can't drift apart on the next IDEPEND/BDEPEND shift. `broot_filtered` is
  deliberately NOT folded in: it's a host-satisfaction *filter* (with its own
  `prefer_update` keep-all-as-is branch), not a stamp, and forcing it into the
  policy would obscure it. Pure `portage-atom-pubgrub`, behaviour-identical,
  152 crate tests + 1675 workspace tests green.
- ✅ **B2 (landed — removed `RootSet` rather than un-deading it).** On
  inspection "un-dead" was the wrong action: `RootSet` was a *path-only*
  summary whose `Single` collapsed `Local`+`Host` (which `base_roots` must
  distinguish for config/eprefix), so it couldn't drive the full `Roots`
  construction — making it canonical was the high-churn/low-payoff reshape the
  field-based `satisfaction_root` already superseded (the maintainer's own
  2026-07-09 direction). Its only read was `.broot()`
  (`Local → prefix`, else `/`); inlined at the two call sites
  (`base_roots`' `Root|Host` arm → `/`; `broot`'s non-overlay branch →
  `match topology_source { Local(p) => p, _ => "/" }`), and the dead enum +
  `root_set()` + `TopologySource::Root`'s now-unused payload deleted.
  `--local` vs `--prefix` unchanged (the `broot` site's non-overlay branch
  still gives `--local` its own prefix; `--prefix` still returns early via
  `is_overlay`). fmt/clippy/rustdoc clean, 1675 tests green.
- ✅ **B3 (landed)** Collapsed the `Cli::broot()` vs `Roots::broot()` name
  footgun (two different methods — Cli's returned a full `Roots`, Roots's
  returned the `Option<&Utf8Path>` BROOT path — under the same name).
  Renamed `Cli::broot()` → `Cli::host_roots()` (it returns the `Roots` a
  `MergeRoot::Host` entry merges into; matches the `host_roots` variable
  convention already used at most call sites). `Roots::broot()` stays as the
  canonical BROOT-path accessor. 10 call sites updated; fmt/clippy/rustdoc
  clean, 1675 tests green.

### Track C: doc reconciliation — ✅ landed

Reconciled `docs/root-topology.md` + `docs/root-model.md` with the shipped
code (one pass, after A/B stabilized):
- `root-topology.md` slop warning now states the enum proposal was superseded
  by the field-based approach (`satisfaction_root` + `BuildClass`), `RootSet`
  removed; the "variant enum (target design)" section carries a superseded
  banner + the correct `Roots` field count (7 path + 3 bool); the Status
  section records `BuildClass` (Track A), the `RootSet` removal (B2), the
  `Cli::broot`→`host_roots` rename (B3), and `use_outer_eroot`.
- `root-model.md` BROOT no longer claims "always host `/`" (table, derived-
  values, per-phase block, and the `--local` section all now state the
  `--local` exception); the broken `root_set().broot()` reference is replaced
  with the `Cli::host_roots()` non-overlay rule; a stray `//`-comment line in
  `root-topology.md` is fixed.

### Deferred / rejected

- Full `RootTopology` enum migration across all 69 `.roots()` callers —
  rejected (again): `satisfaction_root` captured the payoff; most sites
  consume a `Roots`, they don't choose between views. B2 covers the
  subset that does.
- Tier 3 mutable-BROOT on a foreign host (`build-environment.md`);
  zero-config merged sysroot via `fuse-overlayfs` (M3).

### Sizing & order

| track | files | LOC est. | risk | order |
|---|---|---|---|---|
| A | portage-solver/resolve/atom-pubgrub + `build/shell.rs` + cli emerge/crossdev/setup | +300/−250 | medium | A1→A2→A3→C(A) |
| B1 | `provider/solve.rs` | +60/−40 | low | after A3 |
| A4 | profile.rs + Roots + shell | +120/−80 | medium | after B1 |
| B2 | `cli.rs` + 33 `.outer_roots()` | +150/−120 | low-medium | after A4 |
| C | 3 docs | docs | none | with each track |

Recommended: **A1 → A2 → A3 → C(A) → B1 → A4 → C(rest) → B2 → B3.**
A1–A3 alone retire the crossdev guard instability the review flagged.

---

**Correction (2026-07-23), two points the body below doesn't yet know about:**
- The "`--root` config resolution" entry right below says the 2026-07-09
  landed fix made `Roots::config()` default to `config_root.or(root)` (own
  everything, self-contained). **That was itself reverted 2026-07-11
  (`b9d4fbb`)**, back to true `ROOT=` parity (`--config-root`, else host
  `/`, never derived from `--root`) — see `portage-cli/src/cli.rs`'s
  `roots()`, which now comments on exactly this history. The rest of that
  entry (the `select/mod.rs` explicit-config-root fix, `--local` staying
  standalone, etc.) is unaffected and still accurate.
- The "Known gap" that self-contained-bootstrap DEPEND/RDEPEND aren't
  checked against BROOT (also carried into `docs/root-topology.md`) is
  **DONE** — closed by the same `b9d4fbb` commit's `broot_filtered` +
  `host_satisfied_on_broot`/`virtual_satisfied_on_broot` checks
  (`portage-atom-pubgrub/src/provider/solve.rs`). `docs/root-topology.md`
  itself hasn't been updated to reflect this yet.

## Why

The cross/stage session exposed structural debt: `Roots` is a flat bag of five
`Option<PathBuf>` fields, and `host_roots`/`base_roots()` is threaded
positionally across 9 files. Three "wrong root at one site" bugs (`c421c95`,
`732aefe`, `0e9b3e0`) and the `host_aliases` invariant violation (`208c818`)
all stem from no type telling callers which root answers which role. The
refactor replaces the bag with a `RootTopology` enum whose variant answers
`satisfaction_root(dep_class)` as a pure function.

## Behaviour changes (correctness, not just types)

These are the divergences between current code and the target model in
`docs/root-topology.md` § "Override semantics". Each is a real behaviour change
to land as part of (or before) the refactor.

- ✅ **`--root` config resolution — resolved 2026-07-09, not the way originally
  proposed.** Original framing: "`--root` no longer moves config" (portage
  `ROOT=` parity, `config: config_root.or(root)` → `config: config_root`).
  Attempted, reverted, and replaced with a narrower, correct fix after two
  rounds of live findings:
  - **First attempt**: make `config()` default to host, falling back to the
    offset only when `<offset>/etc/portage/make.profile` already exists
    (real ROOT= parity as the common case, self-contained roots still "just
    work" once bootstrapped). **Broke `em select`'s toolchain-slot lookup**:
    a live test (`current_slot_reads_the_active_gcc_config_profile`) caught
    `select/mod.rs`'s `config_portage_dir_for` — a *second*, independent
    consumer of `roots.config()` beyond `crossdev`'s bootstrap — silently
    falling through to the **real host's** `/etc/env.d/gcc` for a
    freshly-created, not-yet-bootstrapped self-contained root (proven with
    real host state: this dev machine's own `riscv64-unknown-linux-gnu`
    gcc-config slot 16 leaked into a supposedly-isolated tempdir test).
  - **Checked real `eselect` for precedent** (at the user's suggestion):
    `/usr/share/eselect/modules/profile.eselect`'s `get_symlink_location`
    does `local root=${PORTAGE_CONFIGROOT-${EROOT}}` — it only ever honours
    an *explicit* `PORTAGE_CONFIGROOT` (or `EROOT`, which a standalone
    invocation never has set); it never cleverly derives a config root from
    `ROOT` alone. `select/mod.rs`'s own doc comment already said as much
    ("`--config-root`... else `--prefix`/`--local` overlay, else `/`" — no
    mention of `--root`) — the actual code just didn't match its own
    documented intent, a pre-existing bug the first attempt's revert
    happened to expose, not something the first attempt caused.
  - **Landed fix**: `Roots::config()` (the merged, build-facing value used by
    `profile_stack`/`expand_sets`/`repos_conf`/crossdev's own bootstrap) is
    reverted to its original, unconditional `config_root.or(root)` — this is
    `em`'s own deliberate self-contained-bootstrap default (own config, own
    everything), not a portage `ROOT=` gap, and touching it broke more than
    it fixed. New, separate `Roots::config_root_explicit()` — *only*
    `--config-root`, never derived from `--root` — is what `select/mod.rs`'s
    `config_portage_dir_for`/`is_prefix_context_for` now use instead of
    `config()`, matching real eselect: `em --root R select ...` reads the
    **host's** config unless `--config-root R` is also given. New
    `Roots::is_self_contained_root()` (topology-only: no EPREFIX, base ==
    target, not bare host) replaces the old `config().is_some()` proxy in
    `crossdev/mod.rs`'s `ensure_self_contained_prefix`/`ensure_prefix_profile`
    — behaviourally identical to before, just no longer coupled to
    `config()`'s exact definition. New
    `Roots::with_own_config_root_if_self_contained()` covers the *internal*
    orchestration case (`crossdev::activate_toolchain`'s own
    gcc-config/binutils-config slot activation for a root it just
    bootstrapped itself) — it forces its own config root without requiring
    the user to also type `--config-root` on every crossdev invocation,
    exactly mirroring how portage's own `{target}-emerge`/build tooling
    exports `PORTAGE_CONFIGROOT` internally rather than expecting the user to.
  - **`--config-root /` already gives literal portage `ROOT=` parity** for
    anyone who wants config to stay at the host for a plain `--root` build
    (e.g. sharing config with an already-installed host system) — no new
    code needed for that direction, it was already the existing escape hatch.
  - Regression tests: `cli.rs` unaffected (no `config()` behaviour change);
    `select/compiler.rs`'s existing
    `current_slot_reads_the_active_gcc_config_profile` updated to pass
    `--config-root` explicitly (the new correct way to test this), plus a
    new `current_slot_ignores_bare_root_without_explicit_config_root`
    asserting the reverse — bare `--root` must *not* pick up the offset's
    env.d, and the internal `with_own_config_root_if_self_contained()` path
    does. Live-verified end-to-end: `em --root R setup` +
    `em --root R --target T crossdev --init-target` still bootstraps
    `R/etc/portage/{make.conf,make.profile,repos.conf}` correctly
    (unaffected); `em --root R --config-root R select compiler show -t T`
    reads a slot written into `R/etc/env.d/gcc` while `em --root R select
    compiler show -t T` (no `--config-root`) reads the real host's instead.
- ✅ **`--local` becomes standalone, not overlay.** Landed in `b3f20c1`.
  `base` goes from None (host) to Some(prefix), so base == target ==
  ~/.gentoo — full closure, self-contained VDB. Live-verified in
  crossdev-stages: `em --local -p bzip2` shows `[N] bzip2` +
  `[N] app-alternatives/bzip2` (full closure; reads the empty prefix VDB,
  not the host's). Previously base=`/` would have hidden both.
- ✅ **Host-python/host-tool symlinks moved from `--local` to `--prefix`.**
  Landed in `b3f20c1`. setup.rs's three-mode split (self-contained /
  standalone / overlay) gates `link_host_pythons`/`link_host_base_tools` on
  `is_overlay` (--prefix), not `is_local`. Live-verified:
  `--local`'s `usr/bin/` is empty; `--prefix`'s has python3.13/3.14/find/xargs
  symlinked to /usr/bin.
- ✅ **`--prefix` sets EPREFIX=P.** Landed in `b3f20c1`. Live-verified:
  `em --prefix /opt/test-prefix dev-python/jinja2` builds and merges clean —
  host python3.14/gpep517/flit-core drive the build (BROOT=host), result lands
  in the prefix VDB (counter=1), host VDB untouched (jinja2 counter stays
  395).
  scripts shebang to `${EPREFIX}/usr/bin/...`, so EPREFIX=P is required for
  the host-python symlinks (above) to actually fire.
- ✅ **Split BROOT from install target under `--prefix`.** Landed in
  `21638aa`. `base_roots()` now returns a BROOT view (merge_root=`/` under
  --prefix), and `roots()` reconstructs the prefix-target view on top. Without
  this, `preflight::check` read BDEPEND from the *prefix's* empty VDB instead
  of the host's, failing the jinja2 build with "not satisfied" even though the
  host had all of gpep517/flit-core/python:3.14. Regression test:
  `prefix_overlay_broot_is_host_not_prefix`.
- ✅ **`--root`'s BROOT is the host, not the offset (portage `ROOT=`
  parity).** The fifth behaviour change, missing from this list until
  2026-07-09: `base_roots()` had `base: R, target: R` for plain `--root R`,
  so `merge_root()` (read as BROOT by `preflight`/`bdepend_avail`/
  `load_host_installed`) was the offset itself — BDEPEND satisfaction
  checked the (usually near-empty) offset VDB instead of the real host's.
  Found live: task #17's `--root .../cross-stage1-riscv64 --cross riscv64...
  systemd-utils` kept failing on `jinja2 found: NO` even though the real
  host already has jinja2 for its own python.
  - **The fix went through two passes.** First pass introduced a `RootSet`
    enum (`Single`/`Dual`/`Overlayed`, matching `docs/root-topology.md`'s
    proposed shape) and made `base_roots()` itself return the host for
    `--root`. That broke a *different* thing: `base_roots()` is also relied
    on as "the outer EROOT, `--cross`-substitution undone" by
    `crossdev/mod.rs`'s `bypass_cross_root` (where crossdev's own
    toolchain-bootstrap packages install) and by `write_cross_env`/
    `write_sysroot_config` (which write config those steps read back) — all
    of which correctly need the *offset* for `--root`, not the host. Caught
    it by re-testing `em --root R --cross T crossdev --init-target`, which
    started hitting a real, *new* permission error (`write_cross_env` trying
    to write `/etc/portage/env/...` — the real host — instead of `R/etc/portage`).
  - **Second pass, landed:** reverted `base_roots()` to its original
    behaviour (still "the outer EROOT", unchanged for every flag) and added
    a new, dedicated `Cli::broot()` — the *only* thing that differs from
    `base_roots()`, and only for plain `--root` (BROOT = host `/` there;
    identical to `base_roots()` for `--prefix`/`--local`, where the two
    already agreed). Repointed the four call sites that actually mean BDEPEND
    satisfaction (`emerge.rs`, `dispatch.rs`'s `equery depgraph`,
    `crossdev::resolve_gcc_version`, `merge/mod.rs`'s `entry_roots` host
    routing) from `base_roots()` to `broot()`; left `bypass_cross_root`/
    `write_cross_env`/`write_sysroot_config`/`activate_toolchain` on
    `base_roots()`, untouched. Regression test: `root_broot_is_host_not_offset`
    (checks `broot()` **and** `base_roots()` diverge correctly for `--root`).
  - **Re-verified end-to-end after the second pass**: `em --root R --cross
    riscv64-unknown-linux-gnu crossdev -t riscv64-unknown-linux-gnu
    --init-target` now completes cleanly, unprivileged, with no `/etc/portage`
    write at all — `write_cross_env` correctly lands in `R/etc/portage`. The
    permission wall was **our own bug** from the first pass, not an inherent
    `--root --cross` limitation — corrected the record here (an earlier
    version of this note wrongly called it expected/by-design).
  - The old self-contained-BROOT-in-an-offset workflow (build everything,
    including BDEPEND tools, into the offset itself — what
    `/var/tmp/cross-stage1-riscv64` was actually doing) still has a home:
    `--local`, parameterized to accept a path (`--local DIR`, was a bare
    bool hardcoded to `~/.gentoo`) instead of plain `--root`.
  - Also found while verifying: the solver's BDEPEND routing genuinely
    differs by scenario, and this is by design, not a bug — `broot_filtered`
    (same-arch native `--root`, no `--cross`) routes an unsatisfied BDEPEND
    to `MergeRoot::Target` (build it into the offset itself); only
    `cross_target_runtime_deps` (true cross-arch, `--cross` with
    `CHOST != CBUILD`) routes it to `MergeRoot::Host`, which is what
    `broot()` now correctly feeds. So this fix's effect is specific to cross
    builds — a same-arch `--root pkg` (no `--cross`) was never affected by
    the BROOT bug in the first place, since that path doesn't consult BROOT
    for BDEPEND routing at all.

- ✅ **`crossdev -t T` doubly-nested the sysroot when a global `--cross T`
  was also set, and `--cross`/`-t` were two separate flags for the same
  concept.** Found while reviewing this arc: `crossdev/mod.rs`'s own
  `sysroot()`/`setup_root()`/`main_repo()`/`ensure_self_contained_prefix()`/
  `ensure_prefix_profile()` (the setup-action helpers) used `globals.roots()`
  — which is *already* `--cross`-substituted to the sysroot when the global
  flag is set — so appending `usr/<tuple>` again doubly-nested it
  (`<EROOT>/usr/T/usr/T`). Reproduced live with matching tuples (not just
  mismatched ones). Fixed by adding `Cli::outer_roots()` (extracted from
  `roots()`'s own "no `--cross`" branch, deduplicating that logic) and
  repointing every setup-only helper to it instead of `roots()`;
  `stage1()`/`profile_stack()`/`resolve_gcc_version` correctly keep `roots()`
  (they genuinely want the sysroot substitution).
  - User pushed back on the follow-up fix (a "reject if `-t` and `--cross`
    disagree" guard): two flags for the same concept that need a mismatch
    check are the smell, not something to validate around. Resolved by
    **removing `crossdev`'s local `-t`/`--target` entirely** and renaming
    the global `--cross` to **`--target`/`-T`** (no clash — `t`/`T` were
    unused everywhere). One flag now serves both roles: `em --target T
    crossdev --init-target` sets T up; `em --target T stages --stage1` (or
    any plain atom build) uses it. `CrossdevArgs.target` is gone;
    `crossdev::run` reads `globals.target` directly. Verified live: `em
    --root R --target T crossdev --init-target` (no local `-t` at all) lays
    down the sysroot at `R/usr/T` correctly, and running with no `--target`
    at all gives a clear error instead of silently guessing.
  - This is a case of the same underlying issue as the enum-migration
    item below, one level up: not just "which of several `Roots`-returning
    methods do I call", but "which of several *flags* mean the same thing".
    Worth keeping in mind during the `RootTopology` migration — check for
    other near-duplicate flag pairs while touching this code, not just
    near-duplicate accessor methods.

- ✅ **`--prefix`'s unsatisfied BDEPEND now weaves host∪prefix VDB and merges
  into the prefix, never the real host.** Found 2026-07-09 by re-deriving
  the topology from scratch: user's stated model — "if you are in --prefix
  you are supposed to install on the prefix the bdepends, the host vdb is
  weaved in ... what is in the prefix drives, but anything that host
  satisfies is not merged again if not explicitly requested" — didn't match
  the code. `Cli::broot()` (the only caller: `merge/mod.rs`'s
  `entry_roots`, used to physically merge a `MergeRoot::Host`-stamped plan
  entry) returned `root_set().broot()` uniformly — host `/` for both
  `--root` (correct, privileged) and `--prefix` (wrong: an unprivileged
  overlay can't write the real host). Latent, not yet hit live: every
  existing `--prefix` test/run happened to have its BDEPEND already
  satisfied by the host (`"host python3.14/gpep517/flit-core drive the
  build"` in this same file's live-test log below — no rebuild ever fired),
  so the wrong-merge-destination path was never exercised.
  - Fix: `Cli::broot()` now returns `outer_roots()` (merge_root == prefix)
    when `base_roots().is_overlay()`, instead of a host-anchored `Roots`;
    unchanged for `--root`/`--local`/bare. `.broot` (the satisfaction root)
    still resolves to the host either way — only the merge destination
    differs.
  - `Avail::initial_bdepend` (`bdepend_avail.rs`) and `load_host_installed`
    (`query/depgraph/installed.rs`) now additionally read the prefix's own
    VDB under `--prefix` (`roots.is_overlay()`), so a BDEPEND already built
    into the prefix by a previous run counts as satisfied. `load_host_installed`
    reads host first, prefix second — `add_host_installed`'s plain
    `HashMap::insert` then makes the later (prefix) entry win for a package
    present in both, matching "what is in the prefix drives".
  - "Not merged again if not explicitly requested" needed no new code — the
    solver's existing `host_satisfied_on_broot`/`append_unsatisfied_broot`
    (`provider/solve.rs`) already drop a satisfied BDEPEND edge outright,
    and an atom named explicitly on the command line is a separate,
    already-existing root-target path unaffected by this.
  - New regression tests: `cli.rs`'s
    `prefix_overlay_broot_merges_into_prefix_not_host`,
    `bdepend_avail.rs`'s `initial_bdepend_weaves_in_the_prefix_vdb_under_overlay`
    / `initial_bdepend_still_finds_host_only_entry_under_overlay`,
    `installed.rs`'s `load_host_installed_weaves_prefix_over_host_under_overlay`
    / `load_host_installed_still_finds_host_only_entry_under_overlay`,
    `merge/mod.rs`'s `host_entry_installs_into_the_prefix_under_overlay_not_the_host`.
    Added `Roots::for_test_overlay(host, prefix)` (test-only constructor)
    since the existing `for_test` collapses base/target/broot to one path.
  - Live-verified: `em --prefix <dir> setup` then `em --prefix <dir> -p
    dev-python/pip` (a real package with genuine `MergeRoot::Host`-routed
    build-time deps, not just the historically-tested host-already-satisfied
    jinja2 case) shows every single line — Host- and Target-routed alike —
    landing `to <prefix>/`, none on the real host. Confirms both the actual
    merge-destination fix and the sibling display fix below together.
  - **Found live, same pass: the `-p` display was a separate, stale code
    path.** `query/depgraph/root_aware.rs`'s `display_root` hardcoded
    `MergeRoot::Host => "/"` — correct before this fix (when `Cli::broot()`
    always *was* host), now stale: the pretend-mode merge list kept showing
    Host-routed entries as landing on `/` even though the real merge
    (`entry_roots`) now correctly sends them to the prefix. Fixed by adding
    `CrossContext.host_target` (computed once in `root_aware::detect`,
    mirroring `Cli::broot()`'s own `is_overlay()` check) and having
    `display_root` read it instead of a hardcoded path. Caught by actually
    reading live `-p` output line-by-line rather than trusting unit tests
    alone — the unit tests cover `Cli::broot()`/the weave correctly, but
    display formatting is a third, independent piece of code that was never
    exercised by them.
  - **Residual gap closed same day, on request ("low hanging fruit").** The
    combined `em --prefix P --target T` case still showed a `MergeRoot::Host`
    entry landing on `/` in `-p` output, because `CrossContext.host_target`
    was derived from `depgraph()`'s `roots` parameter (`cli.roots()`), whose
    `--target`-active branch always clears `eprefix`/`is_overlay()` — losing
    the very signal `host_target` needs. Fixed by threading the correct value
    in from outside instead of re-deriving it from the (possibly-substituted)
    `roots`: new `DepgraphOpts::host_merge_root: &'a Utf8Path` field, set by
    each of the 3 construction sites (`emerge.rs`, `dispatch.rs`,
    `crossdev::resolve_gcc_version`) from `cli.broot().merge_root()` — the
    same authority `merge/mod.rs`'s `entry_roots` already uses for the real
    merge, unaffected by `--target` substitution since it's derived from
    `base_roots()`. `root_aware::detect` now takes `host_merge_root` as a
    parameter instead of computing it from `roots.is_overlay()`.
    Regression test added (`root_aware.rs`'s
    `host_entry_displays_as_landing_in_the_prefix_even_when_roots_is_target_substituted`)
    using a `--target`-shaped `Roots` with a separately-passed prefix path,
    reproducing exactly the bug this closes. **Live-verified**: `em --prefix
    P --target riscv64-unknown-linux-gnu crossdev --init-target` then `-p
    --with-bdeps sys-apps/systemd-utils` shows the Host-routed build chain
    (dev-lang/python + its own openssl/sqlite/glibc/timezone-data) landing
    `to P/`, while the Target-routed sysroot packages land `to
    P/usr/riscv64-unknown-linux-gnu/` — both correct, distinguishable in one
    `-p` run.

## The variant refactor (structural)

> **Note (2026-08-05):** the enum-migration framing of this section is
> superseded by the "Current plan" at the top of this file — the
> `RootTopology`/`RootSet`-as-storage migration was not pursued (and is
> not recommended). What this section documents as ✅ is accurate: those
> fixes *did* land. The remaining 🟡 (`DepStampPolicy`) is now Track B1.

- ✅ **`Roots.satisfaction_root(DepClass)` — landed 2026-07-09.** Scoped down
  from the doc's original `RootTopology`/`RootSet`-as-storage proposal to a
  smaller, lower-churn fix with the same payoff: rather than replacing
  `Roots`'s flat-field shape with the enum (and renaming the type), added
  two fields — `broot: Option<Utf8PathBuf>` and `is_cross_arch: bool` — so
  **one** `Roots` value carries BROOT correctly even under an active
  `--target` sysroot substitution (previously `roots()`'s `--target`-active
  branch built a fresh `Roots` with `base = target = sysroot`, silently
  dropping BROOT — *that* was why a second `host_roots: &Roots` had to be
  threaded everywhere). `satisfaction_root(class)` is a small match using
  the table in `docs/root-topology.md` § "What `satisfaction_root` returns":
  `Bdepend` → `broot`; `Idepend` → `broot` if `is_cross_arch` else
  `merge_root()`; `Depend` → `base` when it genuinely differs from
  `merge_root()` (an overlay, e.g. `--prefix`) else `merge_root()`;
  `Rdepend`/`Pdepend` → `merge_root()`. Reused the **existing** canonical
  `portage_atom_pubgrub::DepClass` (`Bdepend`/`Idepend`/`Depend`/`Rdepend`/
  `Pdepend`, already shared by the solver's own dependency graph) instead of
  inventing a second, near-identical enum — caught this mid-implementation
  by the same "don't add something redundant" instinct this whole session
  has been about.
  - Migrated every call site that threaded a `roots`+`host_roots` pair
    purely to answer "where does BDEPEND resolve": `preflight::check` (now
    one `roots` param), `bdepend_avail::Avail::initial_bdepend`,
    `bdepend_trim::TrimCtx` (now one `roots` field), `query/depgraph/mod.rs`'s
    `DepgraphOpts` (dropped `host_roots`), `installed::load_host_installed`,
    `crossdev::resolve_gcc_version`, `dispatch.rs`'s `equery depgraph`,
    `emerge.rs`.
  - **`base_roots()`/`broot()` (the method) were *not* fully retired** —
    caught this correcting the plan mid-implementation: `merge/mod.rs`'s
    `entry_roots` needs a *full* `Roots` for a Host-routed entry (its own
    `config()`/`build_sysroot()`/`eprefix()`, to actually merge the package
    there), not just a satisfaction path — `satisfaction_root` can't replace
    that need, only the path-only call sites above. `broot()` stays, now
    documented as explicitly distinct from `satisfaction_root` (a full
    merge-destination `Roots` vs. a bare VDB-lookup path) rather than one of
    several same-shaped near-duplicates.
  - Regression tests updated to call `.satisfaction_root(DepClass::Bdepend)`
    instead of the old `.broot()`-as-a-path pattern; `Roots::for_test` now
    also sets `broot` so BDEPEND-satisfaction tests still see the same root
    without a separate `host_roots` value. Full workspace fmt/clippy/test
    clean; live-reverified `em --root R --target T crossdev --init-target`
    (single-nested sysroot, unprivileged) and a `--target`-active BDEPEND
    satisfaction path.
  - Did not pursue: the `CrossArch`-as-triples enum, or normalizing
    `Dual{broot,target}` with `broot == target` to `Single` — the `Roots`
    struct's own `is_cross_arch: bool` field covers the one thing the doc's
    `CrossArch` was needed for (the `IDEPEND` cell), and there was no
    `Single`/`Dual` variant distinction to normalize once the fix stayed
    field-based rather than enum-based.
- ✅ **Privatize `provider.packages` behind `package_data()` — landed
  2026-07-09.** `host_aliases` (`provider/mod.rs`) maps `Host`→`Target`
  identity, and every consumer must remember to call the alias-resolving
  `package_data()`. `dependency_graph` forgot once already (`208c818`);
  a full sweep found **12 more sites with the identical bug**, all reachable
  via `solution.iter()` (which legitimately yields `Host`-flavored entries
  under `--target`/`--prefix` builds) or public-API arguments:
  - `validate.rs`: `check_use_deps`, `check_repo_constraints`,
    `check_blockers`, `slot_operator_bindings` (6 call sites) — each silently
    skipped validation for a `Host`-routed package's USE-deps/repo-constraint/
    blocker/slot-binding.
  - `provider/post_solve.rs`: `compute_use_flag_requirements` (3 sites) and
    `effective_flag_new` — a `Host`-routed package's USE-flag-requirement
    cascade silently under-computed.
  - `provider/mod.rs`'s public `versions_for_pkg`/`deps_for` — currently
    unused by `portage-cli`, but broken for any future `Host`-flavored caller.
  - Also converted `branch_best_installed` (currently safe — its one caller
    always passes a virtual package — but converted anyway for
    defense-in-depth at zero cost) to the same accessor.
  - Confirmed safe, left untouched: `graph.rs`'s `self.packages.get(dp)` for a
    *virtual* `dp` (virtuals are never aliased — `ensure_host_instances`
    filters `!p.is_virtual()` before creating an alias) — converted to
    `package_data()` anyway purely because the field is now private and this
    site is in a different module; `provider/mod.rs`'s own internal uses
    (`add_installed`, the synthetic solver root insert/remove,
    `deps_reach_installed`'s virtual-guarded lookup) — genuinely not
    alias-sensitive, left as direct field access (same module as the
    declaration).
  - **Fix**: `packages` field changed from `pub(crate)` to fully private (no
    modifier) — a compile-time enforcement, not just convention: `graph.rs`/
    `validate.rs` are sibling modules of `provider`, not descendants, so a raw
    `.packages.get()` there is now a hard compile error, catching exactly the
    7 sites the privatization was meant to catch (confirmed by temporarily
    reverting the field to `pub(crate)` and one call site back to
    `.packages.get()` — it compiled again, proving the enforcement is real,
    not incidental). `post_solve.rs`/`solve.rs` are `provider`'s own
    submodules (private fields stay visible to descendants), so those needed
    manual conversion — not compiler-forced, but done for the same
    correctness reason.
  - New regression test: `validate.rs`'s
    `check_blockers_fires_from_a_host_routed_packages_own_blocker` — a
    `Host`-routed package (an unsatisfied BDEPEND, same `set_cross_active`/
    `set_with_bdeps` setup as `graph.rs`'s existing
    `host_package_bdepend_on_another_host_package_orders_correctly`) declares
    a blocker against a normal Target-side RDEPEND; verified this test
    actually fails without the fix (reverted the field + one call site
    temporarily, confirmed red, restored). Full workspace fmt/clippy/test
    clean (141 passing in `portage-atom-pubgrub`, was 140).
- 🟡 **Extract `dep_satisfaction_root(class, merge_root)` table** shared by
  the three solver functions (`cross_target_runtime_deps`/`host_native_deps`/
  `broot_filtered` in `solve.rs`) so they don't drift from `preflight`'s
  routing on the next IDEPEND shift.
  - **2026-07-09: re-checked, description still accurate** (confirmed via
    `git diff`/`git log` that `solve.rs` hasn't changed since the original
    read). The three functions differ along exactly two axes — which
    `MergeRoot` DEPEND/RDEPEND/PDEPEND get stamped with (`Target`/`Host`/
    unstamped) and which `MergeRoot` an *unsatisfied* BDEPEND/IDEPEND edge
    gets stamped with — so the extraction is a small `DepStampPolicy { runtime_stamp:
    Option<MergeRoot>, broot_unsatisfied: MergeRoot, include_depend: bool,
    include_bdepend: bool }` struct plus one shared body, not a literal
    per-`DepClass` table. Still valid, still low priority.

## Live test results (2026-07-05, crossdev-stages aarch64 sandbox)

Cluster A + the BROOT/target split were live-verified end-to-end in the
`crossdev-stages` aarch64-20260618T101350Z sandbox (full isolation, real
stage3, no host contamination):

- ✅ `em setup --local` — "standalone Gentoo-Prefix", empty `usr/bin/` (no
  host-python symlinks).
- ✅ `em setup --prefix /opt/test-prefix` — "ROOT-offset overlay",
  python3.13/3.14/find/xargs symlinked into `${EPREFIX}/usr/bin`.
- ✅ `em --local -p bzip2` → `[N] bzip2` + `[N] app-alternatives/bzip2`
  (standalone full closure; base reads the empty prefix).
- ✅ `em --prefix -p bzip2` → `[R] bzip2` only (overlay delta; base reads host).
- ✅ `em --prefix /opt/test-prefix dev-python/jinja2` — built + merged clean,
  host VDB untouched.
- ✅ `em --prefix /opt/xp crossdev -t riscv64-unknown-linux-gnu --init-target`
  — sysroot at `/opt/xp/usr/<tuple>`, overlay + make.conf routing correct
  (`PKG_CONFIG_SYSROOT_DIR`=sysroot, `BUILD_PKG_CONFIG_LIBDIR`=host).
- ✅ `em --prefix /opt/xp cross-riscv64.../binutils` — built + merged
  (counter=1), cross wrapper layout correct, host VDB untouched.
- ✅ `em --prefix /opt/xp select binutils list/show/set` — fully prefix-aware:
  sees host (aarch64) + prefix (riscv64) profiles, distinguishes them, writes
  selection to prefix's env.d, installs the two-hop wrapper symlinks under the
  prefix. **No code changes needed** — `select/mod.rs:config_portage_dir_for`
  already honours `config_overlay`.

## Open follow-ups (found during live testing)

- ✅ **MAKEOPTS not parallelising gcc's build — re-verified 2026-07-09 via a
  real, complete gcc-stage1 + gcc-stage2 build.** Confirmed the sysroot's
  make.conf carries `MAKEOPTS="-j128"` (the earlier `crossdev-sysroot-
  makeopts` fix, still landed and test-guarded) and that `toolchain.eclass`'s
  `gcc_do_make` goes through `emake` (not bare `make`). The full cross
  toolchain bootstrap below (both gcc stages) completed in this session's
  timeframe rather than hanging at a serial compile, which is the real-world
  answer the original "load avg 1.15" observation needed. Not instrumented
  down to an exact parallelism measurement, but no longer an open question
  blocking anything — closing as resolved.
- **Top-level `em -j N` also setting MAKEOPTS — rejected 2026-07-09, not
  pursuing.** Decided against per-package/per-invocation MAKEOPTS
  auto-derivation from `--jobs`; `--jobs` stays scoped to parallel package
  merges only, MAKEOPTS stays purely a make.conf/env concern.
- ✅ **Full cross toolchain under `--prefix` — DONE, completed end-to-end
  2026-07-09**, resumed in a fresh `crossdev-stages` aarch64 sandbox (the old
  `/opt/xp` state from the previous session's host didn't exist on this
  machine). Found and fixed three real bugs and corrected one wrong fix along
  the way (full story below). Final live result: `em --prefix /opt/xp
  --target riscv64-unknown-linux-gnu crossdev --setup --jobs 4 --keep-going`
  completed all 6 steps clean —
  `binutils(1)→linux-headers(2)→glibc-headers(3)→gcc-stage1(4)→glibc(5)→
  gcc-stage2(6)`, ending `>>> cross toolchain riscv64-unknown-linux-gnu ready
  in /opt/xp/usr/riscv64-unknown-linux-gnu` with the compiler activated
  (`Switching cross-compiler to riscv64-unknown-linux-gnu-15 ... [ ok ]`).
  Verified no host contamination: `/opt/xp/var/db/pkg/cross-riscv64-…/`
  correctly holds all 4 packages; the sandbox's real `/var/db/pkg` has zero
  `cross-*` entries. This is the first time this exact combination
  (unprivileged `--prefix` overlay + a genuine foreign-arch crossdev
  toolchain bootstrap) has completed successfully.
  - ✅ **Bug 1 — `bypass_cross_root` regression, the real root cause.**
    `em --prefix P --target T crossdev --setup` failed step 1 (binutils) with
    a 47-package DEPEND explosion tripping the os-headers preflight, then
    (once superficially "fixed") with `gcc: error: unknown value 'rv64gc' for
    '-march'`. Root cause: the `--cross`/`-t` -> `--target` unification
    earlier this same session (`bcde18a`) made `crossdev --setup` always run
    with the global `--target` flag active — but `crossdev::setup`'s own
    `run_staged` call still passed `bypass_cross_root: false` (harmless
    *before* the unification, since the tuple used to arrive via crossdev's
    own separate `-t` flag, which never touched `globals.target`). After the
    unification this silently made every toolchain-bootstrap step resolve
    against the *sysroot* (`cli.roots()`) instead of the outer EROOT
    (`cli.base_roots()`) — so `cross-<tuple>/binutils`, a host-arch tool,
    read the sysroot's target-arch make.conf (`CHOST=riscv64`,
    `CFLAGS=-march=rv64gc`) to compile itself, and its DEPEND closure
    (including `debuginfod`'s elfutils/curl/glibc chain) was checked against
    the empty sysroot instead of the host that actually satisfies it. Fixed:
    `crossdev::setup`'s `run_staged` call now passes `bypass_cross_root: true`.
    This is a **regression from earlier in this same session**, not a
    pre-existing bug — never caught because `--init-target` (the only
    crossdev operation live-tested right after the unification) doesn't reach
    `run_staged` at all.
  - ⚠️ **False fix, corrected on the user's pushback.** Before finding bug 1,
    the os-headers explosion looked like it needed `binutils`'s `debuginfod`
    USE flag force-dropped unconditionally (previously only dropped for
    `is_self_contained_bootstrap`). The user flagged this immediately
    ("smells a lot" / "you are tapering around") — rightly: once bug 1 was
    actually fixed, a live `-p` preview confirmed `debuginfod` can stay **on**
    (binutils shows `[ebuild R]` alone, no explosion) because `binutils`'s
    DEPEND now correctly routes to the host, which already satisfies the
    whole closure. Reverted the debuginfod change back to its original
    `is_self_contained_bootstrap`-gated form (and the two tests with it) —
    the real fix was `bypass_cross_root` alone. Lesson: a "fix" that makes a
    symptom go away isn't verified until you check whether a more targeted
    fix (the actual root cause) makes the workaround unnecessary.
  - ✅ **Bug 2 — found and fixed, the actual remaining blocker.** Step 3
    (`libc headers`) failed: `checking installed Linux kernel header
    files... missing or too old!` even though step 2 (`linux-headers`)
    reported a clean merge. Extensive live tracing (temporary `eprintln!`
    instrumentation in `ebuild.rs`, since reverted) confirmed `CTARGET`/
    `CHOST` were correctly different in the build shell, ruling out the
    package.env/CTARGET theory and a suspected `brush`-interpreter
    variable-scoping issue. **Independent review by a second model (Fable,
    at the user's request — "switch the investigation to fable and have a
    second look at the changes you made") found the real cause in ~25
    minutes by reading the VDB directly**: `bypass_cross_root: true` (bug 1's
    fix) routes through `cli.base_roots()`, but under `--prefix`,
    `base_roots()`'s `merge_root()` is deliberately the **BROOT** view (host
    `/`, `target: None` — see its own doc comment) — not the outer EROOT
    `bypass_cross_root` actually needs. Every toolchain step was merging onto
    the *sandbox's real host root* instead of `/opt/xp` — confirmed via the
    VDB (`cross-riscv64-unknown-linux-gnu/linux-headers` registered under the
    sandbox's real `/var/db/pkg`, not `/opt/xp/var/db/pkg`) and `walk_image`
    stripping the `P` subtree out of `${ED}` (since `eprefix=Some(P)` makes
    `ED = D + P`, so a merge rooted at `/` writes real files at `D/P/...`
    while `${ED}` search only ever looks under `D/`). Binutils "worked" only
    by accident (its real-arch binaries landing on the real `/usr/bin` is
    harmless to *notice*, unlike headers going missing from the sysroot).
    **Fixed**: every `bypass_cross_root`-adjacent call site changed from
    `base_roots()` to `outer_roots()` — `emerge.rs`'s own `roots` selection,
    plus `crossdev/mod.rs`'s `activate_toolchain`, `maybe_weave_in_gcc_update`,
    and `write_sysroot_config` (three more call sites with the identical bug,
    found by grepping for `base_roots()` after the first fix). `--root`
    (where `outer_roots() == base_roots()`, no `eprefix`) is a no-op change;
    `write_cross_env` already used `config_overlay()` rather than
    `merge_root()` and needed no change. Live-verified end-to-end (see the
    ✅ summary above) — this was the last blocker.
  - Sandbox: destroyed the ad-hoc `em-item6-9-test` sandbox (it had gotten
    contaminated by bug 2 merging onto its real root) and switched to the
    pre-existing `~/.cache/crossdev-stages/sandboxes/aarch64-20260618T101350Z`
    — already prepared from the 2026-07-05 session, so no re-sync needed;
    wiped its stale `/opt/xp` before retesting. `em` binary copied to
    `/opt/em/em` inside it, driven via `crossdev-stages sandbox run --name
    aarch64-20260618T101350Z "..."`.
- 🟡 **Full cross stage1 under `--prefix` — plan now computes, found and
  fixed a fourth real bug (host-arch cross-tool keyword acceptance), real
  build not yet run.** Attempting `em --prefix /opt/xp --target
  riscv64-unknown-linux-gnu stages --stage1` hit `maybe_weave_in_gcc_update`'s
  gcc-refresh sub-resolve failing outright: `resolution failed: __internal__/
  root 0 depends on cross-riscv64-unknown-linux-gnu/gcc 16.1.1_p20260613`.
  - **Root cause, precisely isolated**: `query/depgraph/mod.rs`'s
    `accept_arch = cross.target_arch().unwrap_or(arch)` is one blanket arch
    for the *entire* resolve. `cross-<tuple>/{binutils,gcc,
    clang-crossdev-wrappers}` are host-arch tools (they run *on* the build
    host; only their *output* targets the CTARGET), but get keyword-checked
    against whichever arch happens to be active for the invocation — the
    sysroot's target arch under `--target` (whose own generated make.conf
    happens to permissively accept `"{arch} ~{arch}"`, masking the problem),
    the bare host's real arch otherwise (typically stable-keywords-only, so
    a not-yet-stable gcc version is genuinely rejected there). Confirmed by
    isolating the exact repro: `em --prefix P -p '=cross-.../gcc-16.1.1...'`
    (no `--target`) failed the same way; the identical atom with `--target`
    also set succeeded — same package, same version, different result,
    purely from which arch axis was active.
  - Initially misdiagnosed as a generic "autounmask doesn't cover a masked
    *top-level/pinned* target atom" gap (traced `find_autounmask_candidates`,
    `query/depgraph/repo.rs:1037`, to confirm it only computes suggestions
    from `dropped_deps` — droppable dependency *edges* with an alternative,
    not a hard top-level atom pin with none) — real gap, but not what this
    needed; corrected after re-reading `accept_arch`'s own construction.
  - **Fix, much smaller than either of the above**: real portage's `**`
    keyword token ("accept regardless of keywords") already exists in `em`
    (`AcceptToken::Any`/`ArchAccept.any`, `query/depgraph/repo.rs:48,95-99`)
    and is arch-agnostic by construction. `write_cross_env`
    (`crossdev/mod.rs`) now also writes a `package.accept_keywords` entry
    (`{category}/{pkg} **`) for the host-arch tools (`!is_target_package`),
    reusing the exact same directory-of-files convention already used for
    `package.env` there. No solver/AcceptKeywords changes needed at all —
    this was the user's own suggested direction ("use the autounmask
    machinery we have... crossdev hacks in sad ways the right masks in a
    very ad hoc way") once the real per-package mechanism was found instead
    of a per-resolve dual-config-read.
  - Live-verified: after re-running `crossdev --init-target` (regenerates
    the config), `em --prefix P -p '=cross-.../gcc-16.1.1_p20260613'` (no
    `--target`) now resolves; `em --prefix P --target T stages --stage1 -p`
    now computes a full, real stage1 plan (hundreds of packages) instead of
    hard-failing. Remaining output at that point is a normal `-p` REQUIRED_USE
    advisory (`sys-apps/util-linux`'s `su? ( pam )`, `net-misc/curl`'s `quic?
    (...)`) — expected `-p` behaviour (matches emerge's own "changes are
    necessary" preview semantics), not a bug.
  - Not yet attempted: the actual (non-`-p`) stage1 build — a long,
    real compile, natural next step but its own separate pass.
- ✅ **Host-arch classification made robust, not a hardcoded name list.**
  The `**`-keyword fix above shipped with `is_target_package(pkg: &str)
  -> bool` — `!matches!(pkg, "binutils" | "gcc" | "clang-crossdev-wrappers")`
  — a name list kept separately from `CrossTarget::packages()`, the actual
  source of the package set. User flagged this directly as "a crossdev
  limitation we should avoid" (relevant if `--ex-pkg`ing the clang wrappers
  or a future `rust-std` for LLVM+Rust cross builds) — and it was already a
  *live* bug, not just a future risk: `("dev-debug", "gdb")` is in the
  GCC-mode package list but isn't in `is_target_package`'s exclusion set, so
  `gdb` (which runs on the host to debug target binaries, exactly like
  binutils/gcc) was silently getting the *target* multilib env block and no
  `**` keyword entry.
  - **Fix**: `CrossTarget::packages()` now returns
    `Vec<(&'static str, &'static str, PackageArch)>` — a new
    `PackageArch { Host, Target }` enum stated at each package's own push
    site in `target.rs`, the single place a cross package is declared.
    `gdb` is now `Host`. Adding a future package (`rust-std`, etc.) forces
    picking `Host`/`Target` right there — no separate list to remember to
    update. `is_target_package` removed entirely; `write_cross_env` reads
    `arch.is_target()` (for `multilib::env_block`'s ABI selection) and
    `arch == PackageArch::Host` (for the `**` keyword entry) straight off
    the tuple instead.
  - All non-classification callers of `.packages()` (`show_target_cfg`,
    `write_alias_repo_conf`, `alias_packages_line`, and their tests) just
    destructure and ignore the third field — no behaviour change for them.
  - Verified: `cargo fmt --check`, `cargo clippy --workspace --exclude
    portage-bench --tests -- -D warnings`, `cargo test --workspace --exclude
    portage-bench` all clean; `crossdev::target::tests::
    riscv_gnu_is_glibc_with_kernel` now asserts `gdb` is `PackageArch::Host`
    directly (previously only checked glibc/linux-headers presence).
  - While fixing this, found and fixed two **pre-existing, unrelated**
    clippy breaks already on `master` before this fix (from the earlier
    `--prefix` BDEPEND-weave commit): `clippy::err_expect` in
    `write_alias_repo_conf_rejects_a_missing_source_package`, and
    `clippy::items_after_test_module` from `merge/mod.rs`'s
    `entry_roots_tests` sitting before later non-test items — moved that
    module to the end of the file, no logic change.
  - Live-verified in the pre-existing `aarch64-20260618T101350Z` sandbox
    (before the correction below was applied): with the *old* binary still
    copied in, `package.accept_keywords/cross-riscv64-unknown-linux-gnu` had
    only `binutils`/`gcc` (`**`), no `gdb` line. After copying in the fixed
    binary and re-running `crossdev --init-target`, `gdb **` appeared and
    `gdb.conf` carried the host ABI matching `binutils.conf`/`gcc.conf`. This
    confirmed the classification mechanism itself works — but see below,
    the premise that `gdb` belongs in the base set at all was wrong.
  - **Correction (same session, user caught it): `gdb` shouldn't have been
    classified as `Host` — it shouldn't be in the base package set at all.**
    Asked "why gdb entered the list though?", then "gdb is optional in
    crossdev. did you get confused?" — yes. Checked `/usr/bin/crossdev`
    directly (`ex_gdb() { [[ ${EX_GDB} == "yes" ]]; }`, `--ex-gdb` sets
    `EX_GDB=yes`, `ex_gdb && doemerge ${DPKG}` at the very end): real
    crossdev only builds a cross gdb when `--ex-gdb` is explicitly passed —
    it's an opt-in "extra" alongside `--ex-pkg`, not part of the base
    binutils→headers→gcc→libc toolchain. `em`'s own design notes
    (`todo/crossdev-target.md:358`) already documented this correctly:
    `"Extra (after stages): --ex-gcc→$GPKG-extra, --ex-gdb→$DPKG, --ex-pkg
    X→doemerge X"`. `em` has no `--ex-gdb`/`--ex-pkg` mechanism yet, so
    `dev-debug/gdb` being unconditionally in `CrossTarget::packages()`'s
    GCC-mode list (since the very first commit introducing it, `a3c7727`)
    was simply a mistake, not a deliberate "always build it" choice — it
    had nothing to opt out into. Fix: removed the `gdb` push from
    `packages()` entirely (not reclassified — removed); updated the one test
    that asserted its presence to assert its *absence* instead. Re-verified
    `cargo fmt --check`/clippy/full test suite clean, and the live sandbox
    toolchain preview (`--show-target-cfg`) no longer lists a `.../gdb` row.
    The `PackageArch` classification refactor itself (previous bullet)
    stands on its own merits independent of this correction — `binutils`/
    `gcc`/`clang-crossdev-wrappers` are still `Host`, `linux-headers`/libc/
    LLVM runtimes still `Target`, still declared at each push site instead
    of a separate name list.
  - **Correction #2 (same thread, user caught it again): `--ex-pkg` is
    already fully supported — there is no missing mechanism, and my
    "confirmed live bug" claim below this line in an earlier edit was a
    self-inflicted false alarm.** I tested `em --prefix P --target T -p
    cross-riscv64-unknown-linux-gnu/gdb` (with `--target` *and* an explicit
    `cross-<tuple>` atom together) and saw it merge into the sysroot
    instead of the host, and went and read `repo::target_package`/
    `solve.rs`/`host_copies.rs` hunting for a classification gap. User:
    "for cross-{riscv64-unknown-linux-gnu} you simply do `em -p` if you
    pass a `--target T` it means that you are trying to set CHOST=T
    CTARGET=riscv64-unknown-linux-gnu, and it would be a quite different
    thing, isn't it?" — yes. The `cross-<tuple>` **category already fully
    identifies the cross target**; naming `cross-riscv64-unknown-linux-gnu/
    gdb` needs no `--target` at all. `--target T` is a separate, session-
    wide concern (dual-root CHOST/CTARGET context for resolving *ordinary*
    non-cross-category packages against the target sysroot, e.g. for `em
    stages --stage1`) — combining both for a directly-named cross-category
    atom is a redundant/conflicting invocation, not the real usage shape.
    Re-tested without `--target`: `em --prefix /opt/xp -p
    cross-riscv64-unknown-linux-gnu/gdb` → `cross-riscv64-unknown-linux-gnu/
    gdb-9999 ... to /opt/xp/` — correctly lands in the prefix, no sysroot
    involved. *(Superseded below — this test was unknowingly run against a
    stale alias file that still declared `gdb`; see the staleness bug and
    correction #3.)*
  - **Correction #3: `--ex-pkg` IS a real, currently-missing feature —
    "no --ex-pkg work needed" above was also wrong**, caught by re-testing
    properly after fixing the staleness bug below. Once the sandbox's alias
    file was actually regenerated fresh (deleting the stale one, since
    `write_if_absent` never refreshes it — see next item), `em --prefix
    /opt/xp -p cross-riscv64-unknown-linux-gnu/gdb` correctly failed with
    `no ebuilds found` — `gdb` is no longer in `CrossTarget::packages()`'s
    fixed compile-time list, so the alias declaration no longer exposes it,
    and there is no CLI mechanism to add it (or any other extra) back. User:
    "so --ex-pkg it is a concern for the __crossdev__ applet and in our
    case it means adding an entry to the alias map. And --ex-pkg packages
    need to be aware of ctarget to be meaningful." Confirmed against
    `/usr/bin/crossdev` directly: `for_each_extra_pkg set_portage X` (line
    1675) calls `set_portage` with `l=X`, and `set_env`'s `case ${l} in K|L)
    ... ;; *) ... ;; esac` (line 1483) means `X` always takes the **host**
    ABI branch — every `--ex-pkg` extra gets host-ABI env, unconditionally,
    same as binutils/gcc/gdb. So `--ex-pkg` in `em`'s model means: (1) add
    the atom to the alias-packages set (so `cross-<tuple>/<pkg>` resolves
    at all), (2) write its `package.env` entry via the same `write_cross_env`
    mechanism, always on the host-ABI branch. `--ex-gdb` is pure sugar for
    `--ex-pkg dev-debug/gdb` — no separate code path. **Not yet implemented
    — this is the next concrete task**, tracked below.
  - **Staleness bug found and fixed while re-testing**: `write_alias_repo_conf`
    (and `write_sysroot_repos_conf`'s own copy of the same alias entry) wrote
    via `write_if_absent` (`util.rs:9`), which never overwrites an existing
    file regardless of content — so the drift-detection check above it was
    dead code; a stale alias from a prior run (e.g. still declaring `gdb`
    after it was removed from `packages()`) was never refreshed by a later
    `--init-target`, only by deleting the file by hand. Fixed: extracted
    `write_or_refresh_alias_conf(file, category, packages_line)`, used by
    both call sites — absent → write fresh; present and matching → no-op;
    present, ours (`alias-target =` key) but stale → overwrite; present,
    foreign (no `alias-target =` key, e.g. a real crossdev/eselect-managed
    physical overlay) → never touch. Also fixed `write_sysroot_config`'s
    `make.conf`, which had the identical bug (`write_if_absent`, content
    derived from `target`/`outer_root`, both able to legitimately change
    across `--init-target` re-runs, e.g. a different `--prefix`) — switched
    to an unconditional `std::fs::write` like `write_cross_env` already
    correctly does; no foreign-entry concern there (entirely em-managed,
    unlike the host's real make.conf). Left `ensure_self_contained_prefix`'s
    and `write_sysroot_repos_conf`'s `gentoo.conf` (bare `location =
    <host-repo-path>`, no per-target content, no real drift scenario) as
    `write_if_absent`. New regression tests:
    `write_alias_repo_conf_refreshes_a_stale_own_entry`,
    `write_alias_repo_conf_never_touches_a_foreign_entry`. Full workspace
    `fmt`/clippy/test clean.
- ✅ **`--init-target` now honours `-p`/`-a` like every other mutating `em`
  path.** Before this, `init_target()` (the standalone `--init-target` flag's
  entry point, `crossdev/mod.rs:132`) wrote every file unconditionally and
  immediately — no preview, no confirmation, unlike `-p`/`-a` everywhere else
  in `em`. User: "let's try to understand better, we can weave -p and -a to
  cover the regeneration of make.conf and package.env and such."
  - **Design**: new `crossdev/config_plan.rs` module. A `ConfigEntry` enum
    (`File` — always regenerated, em owns the content; `CreateOnly` — write
    only if absent, e.g. a bare `location =` string with no real drift
    scenario; `Alias` — the `[crossdev]` alias entry's existing absent/
    match/stale-own/foreign logic from the previous fix, generalised;
    `Dir`; `Symlink`) separates *computing desired state* (no I/O beyond
    validation) from *diffing against disk* from *applying*. `config_plan::
    apply(entries, globals)` diffs the whole batch, then: `-p` prints what
    would change and writes nothing; `-a` prints the same and confirms once
    (`confirm_config_write`, mirroring `merge/mod.rs`'s `confirm_merge`)
    before writing; otherwise applies directly. Returns an `Outcome` so
    `init_target` only prints its "cross target ready" summary when
    something was actually applied (or there was nothing to do) — not after
    a preview or a decline.
  - Every existing write-helper (`write_alias_repo_conf` →
    `alias_repo_conf_entry`, `write_sysroot_config` → `sysroot_config_entries`,
    `write_sysroot_repos_conf` → `sysroot_repos_conf_entries`,
    `write_cross_env` → `cross_env_entries`, `ensure_prefix_profile` →
    `prefix_profile_entries`) now *collects* `ConfigEntry` values instead of
    writing directly; `init_target` gathers them all and makes one
    `config_plan::apply` call. `setup::bootstrap` (a separate, already
    pretend-aware subsystem — EPREFIX skeleton/bashrc) stays outside the
    plan, now gated by an explicit `!globals.pretend` inline in
    `init_target` (previously implicit via the whole-function `if
    !globals.pretend { init_target(...) }` gate at `setup()`'s call site,
    which is now removed since `init_target` handles pretend internally —
    meaning `em crossdev --setup -p` now *also* previews the config-plan
    changes, which it previously skipped silently).
  - `em toolchain --setup`'s native path keeps an eager, non-interactive
    `ensure_self_contained_prefix(globals) -> Result<Utf8PathBuf>` wrapper
    (bootstrap + `config_plan::apply_now`, no diff/preview/confirm) since
    that call site is already externally gated by `!globals.pretend` and
    doesn't need its own preview.
  - New tests in `config_plan.rs`: `pretend_writes_nothing`,
    `plain_run_applies_directly`, `no_change_is_reported_as_nothing_to_apply`,
    `create_only_never_overwrites_existing_content`,
    `dir_entry_creates_a_missing_directory`,
    `alias_entry_never_touches_a_foreign_file`. Existing
    `write_alias_repo_conf_*` tests kept via a test-only compatibility shim
    (`alias_repo_conf_entry` + `config_plan::apply_now`).
  - Live-verified in the `aarch64-20260618T101350Z` sandbox with an injected
    stale alias: `-p` printed `update .../crossdev.conf` and left the file
    untouched; `-a` piped `n` printed the same preview + `>>> Quitting.` and
    left it untouched; `-a` piped `y` applied it and printed the normal
    "ready" summary; a further plain re-run (nothing to change) printed only
    the "ready" summary with no `config changes` noise — matches emerge's
    own `-p`/`-a` UX exactly.
  - Full workspace `fmt`/clippy/test clean.
- ✅ **`--ex-pkg`/`--ex-gdb` implemented — crossdev's own "Extra Fun".** New
  `CrossdevArgs` fields (`cli.rs`): `ex_pkg: Vec<String>` (`CATEGORY/PN`,
  repeatable) and `ex_gdb: bool` (sugar for `--ex-pkg dev-debug/gdb`, per
  the user's own framing: "`--ex-gdb` should just be a shorthand for a
  matching `--ex-pkg`"). `ex_pkg_atoms(args) -> Result<Vec<Cpn>>`
  (`crossdev/mod.rs`) parses each with `portage_atom::Cpn::parse` (not a
  hand-rolled `split_once('/')` — user: "let's make it slightly less sloppy
  and parse as cpn and possibly validate against the main repo") and appends
  `dev-debug/gdb` for `--ex-gdb`.
  - Per the confirmed design (previous entries): `--ex-pkg` is a `crossdev`
    concern, not a general `em` one — it means adding an entry to the alias
    map, and extras are always host-arch to be meaningful (checked against
    `/usr/bin/crossdev` directly: `for_each_extra_pkg set_portage X` always
    takes `set_env`'s host-ABI branch for `l=X`).
  - `extras: &[Cpn]` threaded through `alias_repo_conf_entry` (existence
    validated against `::gentoo` in the same loop as the base set, same
    error shape, appended to the alias-packages line),
    `sysroot_repos_conf_entries` (same, for the sysroot's own copy),
    `cross_env_entries` (each extra gets a host-ABI env file — always
    `arch.is_target() == false`, never the target branch — plus a `**`
    `package.accept_keywords` entry, unconditionally) and
    `show_target_cfg` (an "Extra (--ex-pkg, host-arch)" section).
  - **Per-invocation, not sticky** — matches real crossdev's own `--ex-pkg`
    semantics exactly (`XPKGS` is a per-run CLI list, never persisted): a
    later `--init-target` that omits a previously-added extra regenerates
    the alias/env/keywords without it, same as the drift-refresh behaviour
    the staleness fix already established. Considered and rejected a
    "sticky" (union-with-existing-file) design — real crossdev has no such
    memory either, and it would contradict the "config always exactly
    reflects what this invocation asked for" philosophy just built.
  - New tests: `ex_pkg_atoms_parses_category_pn`,
    `ex_pkg_atoms_rejects_bad_shape`, `ex_gdb_is_sugar_for_ex_pkg_dev_debug_gdb`,
    `ex_pkg_extras_are_validated_aliased_and_host_classified` (existence
    check + alias-packages line; `cross_env_entries`'s host-ABI/`**`
    treatment is live-verified only, like the rest of `write_cross_env`'s
    multilib-dependent behaviour — no unit test sources a real
    `multilib.eclass`). Full workspace `fmt`/clippy/test clean.
  - Live-verified in the `aarch64-20260618T101350Z` sandbox: `--show-target-cfg
    --ex-gdb` previews the extra; `--init-target --ex-gdb` writes `dev-debug/
    gdb` into the alias, `**` into `package.accept_keywords`, and a
    host-ABI env file (`ABI='arm64'`, matching binutils/gcc, not the
    target's `lp64d`) — `em --prefix P -p cross-.../gdb` (no `--target`)
    then resolves it correctly to the prefix. A fresh `--ex-pkg dev-vcs/git`
    (never in the base set) resolves the same way. A malformed `--ex-pkg
    not-a-cpn` is rejected with a clear `Cpn::parse` error. A later
    `--init-target` without either flag correctly drops both extras again
    (confirming the per-invocation, non-sticky design).
- ✅ **Audited whether `--init-target`/`--setup` overwrites hand edits made
  between runs — found and fixed a real bug in the process.** User: "let's
  try to check if we did not leave gaps: --init-target, following by edits
  and then --setup would overwrite the edits?" Went through every
  `ConfigEntry` kind and live-tested each:
  - **`File` entries (sysroot `make.conf`, per-package `env/<cat>/<pkg>.conf`,
    `package.env`, `package.accept_keywords`) are unconditionally
    regenerated — hand edits never survive a later run.** This is by
    design (`em` owns this content entirely) and matches real crossdev's
    own behaviour for the same files (`set_env` always writes them via a
    plain `>` redirect) — not a gap, but worth being explicit about, so
    `docs/crossdev.md`'s gotchas section should say so plainly (not yet
    added there — see follow-up below).
  - **`CreateOnly` entries** (bare `gentoo.conf` location strings) correctly
    preserve hand edits — never a problem.
  - **`Alias` entries had a real bug**: `change()` compared with
    `.contains()` instead of exact equality, so a hand-edited
    `alias-packages` line that happened to contain the freshly-computed
    line as a *substring* (e.g. manually appending a package instead of
    using `--ex-pkg`) was wrongly reported "already up to date" and the
    hand edit silently survived — while the same kind of edit landing
    anywhere else in the line (not a clean prefix) would just as silently
    have been clobbered instead. No principled reason for the
    inconsistency. **Fixed**: extracted `alias_body(category,
    packages_line)`, used by both `change()` (now an exact `existing ==
    alias_body(...)` comparison) and `apply()` (previously duplicated the
    format string independently, drift risk of its own) — and both now
    reference the `OVERLAY_NAME` constant instead of a second hardcoded
    `"crossdev"` literal. New test:
    `alias_entry_treats_a_hand_extended_line_as_drift`.
  - **`Symlink` entries** (make.profile links) get corrected back to the
    target's own derived profile if hand-repointed — intentional
    self-healing (the profile is derived from the tuple, never meant to be
    hand-chosen), not a gap.
  - Live-verified the fix in the `aarch64-20260618T101350Z` sandbox: hand-
    appended `dev-vcs/git` to the alias-packages line, hand-added a bogus
    line to `package.env`, hand-added a var to the sysroot `make.conf`.
    With the pre-fix binary, `-p` only flagged `make.conf`/`package.env` as
    changing — the alias hand-edit was invisible. With the fix, `-p`
    correctly flags all three. Full workspace `fmt`/clippy/test clean.
  - Added an explicit "don't hand-edit the generated config" note to
    `docs/crossdev.md`'s gotchas covering this.
- ✅ **`--setup`'s implied config-laydown no longer clobbers hand edits made
  after an earlier `--init-target`.** User: "we should allow the hand edits
  to survive between init and setup." Real tension surfaced before landing
  this: the just-shipped drift-refresh behaviour (always resync) is also
  exactly what makes `--setup --ex-pkg X`/`--ex-gdb` work against an
  already-initialized target, and file content alone can't distinguish "the
  user hand-edited this" from "`--ex-pkg` legitimately changed what should
  be here." Presented the trade-off and asked; user picked the
  straightforward policy split over a fingerprinting scheme or a
  file-by-file split.
  - **Design**: new `config_plan::RefreshPolicy` (`Sync` — always reconcile
    to the freshly-computed state, what explicit `--init-target` uses;
    `FillGapsOnly` — only create what's missing, anything already present
    (hand-edited or not) is left alone, what `--setup`'s own implied
    config-laydown step now uses). `ConfigEntry::present()` is the
    existence-only check `FillGapsOnly` stops at, per entry kind. `apply()`
    (the top-level plan function) and `apply_now()` now skip entries whose
    `change()` is `Unchanged` in the final write loop, not just the printed
    summary — a real correctness requirement for `FillGapsOnly` (previously
    every entry was unconditionally re-applied at the end regardless of
    what `change()` said, harmless under `Sync`'s always-identical-content
    "Unchanged", but would have defeated `FillGapsOnly` entirely).
  - **Accepted, documented trade-off**: `--setup --ex-pkg X`/`--ex-gdb`
    against an *already-initialized* target does not add `X` — run
    `--init-target --ex-pkg X` (`Sync`) first. A fresh target being
    `--setup` directly still gets everything written correctly (nothing
    exists yet, so `FillGapsOnly` creates it all). Documented in both the
    `RefreshPolicy` enum's doc comment and `docs/crossdev.md`.
  - New tests: `fill_gaps_only_never_touches_an_existing_file`,
    `fill_gaps_only_still_creates_missing_files`,
    `fill_gaps_only_never_touches_an_existing_alias_even_with_a_different_packages_line`.
  - Live-verified in the `aarch64-20260618T101350Z` sandbox end to end: (1)
    `--init-target` (clean baseline); (2) hand-edited `make.conf` (added a
    var) and the alias-packages line (added a package by hand); (3) `--setup
    -p` showed no config-changes preview at all and both hand edits survived
    on disk; (4) ran the **real** (non-`-p`) `--setup` — both hand edits
    still survived after actual execution, confirming the fix holds under
    `apply()`'s real write path, not just the diff/preview path; (5)
    confirmed the accepted trade-off directly: `--setup --ex-gdb` against
    this already-initialized target correctly did *not* add `gdb` to the
    alias; (6) a subsequent explicit `--init-target` (no extras) correctly
    reverted both hand edits back to the clean computed state, confirming
    `Sync` vs `FillGapsOnly` are cleanly distinguished by caller.
  - **Found a real, pre-existing, unrelated bug while doing (4)**: the real
    `--setup` run failed at the pre-flight dependency check —
    `dev-perl/Digest-HMAC-1.50.0 needs: >=virtual/perl-Digest-MD5-2.0.0,
    >=virtual/perl-Digest-SHA-1.0.0` and `dev-vcs/git-9999-r3 needs:
    >=dev-vcs/git-1.8.2.1[curl], app-text/asciidoc` — both look like
    self-referential/ordering issues (a package's own BDEPEND pointing at
    itself, or at sibling `virtual/*` packages providing the exact thing
    just listed earlier in the same plan, not being recognized as satisfied
    by an earlier plan entry). Confirmed this reproduces identically on a
    *fully clean* target (no `--ex-pkg` involved at all) — it's a genuine,
    pre-existing preflight/dependency-graph gap surfaced by `app-text/
    asciidoc`'s doc-build closure (a BDEPEND of `sys-devel/binutils`'s doc
    USE flag, pulling in a perl+git chain), **not** anything caused by this
    session's crossdev work. Not investigated further — flagged here as a
    new, separate item for a future session. This is what actually blocked
    getting the real riscv64 toolchain bootstrap to completion this
    session (the "run the real --setup, ~20 min" goal from earlier).
  - **Also fixed while investigating**: `dev-vcs/git` was a poor choice of
    example package for `--ex-pkg` in this session's docs/tests (used
    earlier for the drift/hand-edit testing) — user: "dev-vcs/git makes no
    sense as --ex-pkg" — precisely because it's already an ordinary
    transitive dependency of things in the toolchain's own build closure
    (confirmed by the bug above), so using it as the `--ex-pkg` example
    conflated "did --ex-pkg do this" with "was this already going to be
    pulled in anyway." Tried `dev-debug/strace` next (better — a genuine
    standalone host-arch tool), but the user pointed at the real intended
    example instead: `sys-devel/rust-std` — its own `::gentoo` ebuild
    `DESCRIPTION` literally reads "Rust standard library, standalone (for
    crossdev)", confirmed by reading the ebuild directly. This is also the
    concrete package the user meant much earlier in the session ("if we want
    to --ex-pkg the clang wrappers or rust-std we need to autounmask them
    properly"). Replaced in `docs/crossdev.md` and all of `mod.rs`'s
    `--ex-pkg` tests.

## Verification (outstanding)

- 🔴 **Pre-flight dependency check failure — real, pre-existing, root cause
  narrowed to a duplicate/misordered plan entry in `install_order`, not yet
  fixed.** Found 2026-07-10 running the real (non-`-p`) `--setup` in the
  `aarch64-20260618T101350Z` sandbox: `dev-perl/Digest-HMAC-1.50.0 needs:
  >=virtual/perl-Digest-MD5-2.0.0, >=virtual/perl-Digest-SHA-1.0.0` and
  `dev-vcs/git-9999-r3 needs: >=dev-vcs/git-1.8.2.1[curl],
  app-text/asciidoc`. Surfaced via `app-text/asciidoc` (a doc-build BDEPEND
  of `sys-devel/binutils`) pulling in a perl+git closure.
  - **Confirmed pre-existing, not a session regression** — user asked
    directly ("you are telling me that setup is failing on normal usage
    now?"). Inference alone (no session commit touches `preflight.rs`/
    `portage-atom-pubgrub`, confirmed via `git log 71ff3bf..HEAD --
    portage-cli/src/preflight.rs portage-atom-pubgrub/src/` returning
    nothing) wasn't good enough — verified empirically. Built `65e91bf`
    (the commit on `origin/master` before this session's first push) in a
    sibling worktree, swapped it into the *same* sandbox (old `--cross`/
    `-t` flags, its old on-disk-symlink overlay, not this session's alias
    mechanism), ran the identical real `--setup`: byte-for-byte identical
    failure. Worktree removed, sandbox binary/config restored to current
    `em` afterward.
  - **False lead, corrected**: `-p --jobs 4 --keep-going` appeared to
    "succeed" (no error) right after the real run failed with plain
    defaults, and was briefly taken as "the flags fix it." Wrong —
    `emerge.rs:267`'s `if cli.pretend { return ...; }` returns *before*
    `preflight::check` is ever called (`emerge.rs:298`), so **no `-p` run
    has ever exercised this check at all**, regardless of flags. The real
    (non-`-p`) run fails identically every time, with or without
    `--jobs`/`--keep-going`, on both today's code and the `65e91bf`
    baseline. This is itself a separate, real gap — see the `-p`/`-a`
    depgraph item just below.
  - **Root cause, narrowed via the full untruncated plan output** (`em
    --prefix /opt/xp --target riscv64-unknown-linux-gnu crossdev --setup`,
    no `-p`, captured whole, only 38 lines): `dev-perl/Digest-HMAC-1.50.0`
    appears **twice** in the plan (once before `virtual/perl-Digest-MD5`/
    `Digest-SHA`, once correctly after them); `virtual/perl-Digest-MD5`/
    `Digest-SHA` themselves are also each listed twice. `preflight::check`
    is a strictly sequential scan (`for planned in plan`, checking each
    entry only against what's been "recorded" from *earlier* entries) — it
    is correctly reporting that the *first* occurrence isn't satisfied by
    what precedes it; the second, later, correctly-placed occurrence
    doesn't change that. So the bug is upstream of `preflight.rs`: the
    installed order (`PortagePackage::install_order`,
    `portage-atom-pubgrub/src/graph.rs:149`) or the plan-building pipeline
    around it (`query/depgraph/mod.rs`'s `order`/`full_order`/
    `bdepend_trim` handling, ~line 640-740) is emitting the same package
    twice at two different positions instead of once, correctly placed.
    `preflight.rs` itself looks sound for what it claims to do (a
    sequential growing-availability scan matching its own doc comment); the
    user's "possibly redundant" framing may still be right in spirit — if
    `install_order` genuinely guaranteed a valid, deduplicated topological
    order (which is what it's *supposed* to do per its own doc comment),
    this check should structurally never fire, making it a pure guard rail
    around a solver invariant rather than something doing its own
    independent work.
  - **Confirmed root cause (2026-07-10, Fable deep-dive)**: the user's own
    "host vs target" hypothesis is refuted — **both occurrences of
    `dev-perl/Digest-HMAC-1.50.0` are `MergeRoot::Host`.** `install_order`
    itself is innocent: instrumented `full_order` (its raw output) is
    clean, deduplicated, and correctly ordered — `Digest-HMAC:0@host`
    appears exactly once, correctly *after* its `Digest-MD5`/`Digest-SHA`
    deps.
    - The duplicates are injected **after** `install_order`, by
      `host_copies::compute` (`portage-cli/src/query/depgraph/host_copies.rs`),
      spliced onto the plan's front at `mod.rs:809-811`
      (`order.splice(0..0, host_copies)`).
    - `host_copies` exists for Tier-1 native-offset builds (`--root`/
      `--prefix`, same arch): a post-solve closure walk that schedules
      Host-arch build deps the host VDB lacks, kept *outside* the solver
      to avoid a Tier-1 aliasing blowup (its own doc comment,
      `host_copies.rs:11-17`).
    - Its guard, `!cross.active || cross.is_cross_arch()`
      (`host_copies.rs:66`), only excludes cross-*arch* contexts. But
      `crossdev --setup`'s host-arch step (e.g. `cross-<T>/binutils`) runs
      with `bypass_cross_root: true` — a same-arch offset cross context —
      and `set_cross_active(cross.active)` (`mod.rs:402`) enables the
      dual-root solver for *any* active cross context, not just cross-arch
      ones. So the solver **also** emits `pkg:0@host` nodes for this case
      (Block B in the captured plan) — and `host_copies::compute` runs too,
      independently re-deriving the *same* closure (Block A), because it
      has no way to see the solver's own `@host` entries in `target_order`
      (its seeding/version-reuse filters only look at `MergeRoot::Target`,
      `host_copies.rs:72-76,96-99`).
    - Block A isn't just a redundant copy of Block B — it's independently
      *wrong*: `resolve()`'s fallback (`host_copies.rs:144-159`) picks
      `versions.iter().max_by(version)` over `RepoData`'s **unfiltered**
      version list, ignoring keyword/mask acceptance despite the doc
      comment's "newest accepted repo version" claim — hence `dev-vcs/
      git-9999-r3` (a live, unkeyworded ebuild; the solver picked `2.53.0`)
      and `Crypt-URandom-0.550.0` (solver picked `0.540.0`). Its
      availability baseline is `vdb_cpvs(None)` — the *real* host VDB only
      (`host_copies.rs:87`), never woven with the `--prefix` VDB the way
      `Avail::initial_bdepend` is for `--prefix` — so it can't see Host
      packages already merged into the prefix by an earlier run either.
      And the BFS walk (`compute`'s `while let Some(cpn) = walk.queue
      .pop_front()`, `host_copies.rs:105-114`) pushes a resolved package
      onto `copies` *before* enqueueing that same package's own deps —
      dependent-before-dependency, exactly the misordering preflight
      caught (`Digest-HMAC@host` at position 4, its own `Digest-MD5`/
      `Digest-SHA@host` deps at 6/7; `git-9999@host` at 5, its `asciidoc`
      doc-BDEPEND at 8).
    - So this was a real, live bug, not a false alarm: a genuine `--setup`
      run would have attempted to build `dev-vcs/git-9999-r3` (nothing the
      solver ever chose) and the wrong `Crypt-URandom` version, in an order
      that would have failed regardless of preflight.
  - **Fix, landed and live-verified.** `host_copies::compute`
    (`portage-cli/src/query/depgraph/host_copies.rs`) now: (1) seeds its
    `seen`/`avail` from the `MergeRoot::Host` entries already present in
    `target_order`, so it never re-derives a closure the solver already
    emitted; (2) walks recursively (`visit_unsatisfied`) and appends each
    resolved copy to `copies` only *after* recursing into that copy's own
    edges — deps-first, not BFS discovery order; (3) `resolve()`'s
    fallback now filters candidate versions through `Adapter::
    version_accepted` (keyword/mask/license), instead of a bare
    `max_by(version)` over the raw unfiltered repo list; (4) the
    availability baseline is now `Avail::initial_bdepend(roots)` (host VDB
    + `--prefix` VDB weave) instead of `vdb_cpvs(None)` (real host VDB
    only), reusing the exact same weave `preflight` relies on instead of a
    second, incomplete copy of it. `compute`'s signature changed to take a
    `&repo::Adapter` (already has `data`/`use_config`/`package_use` plus
    the acceptance machinery) and `&Roots`, constructed at its one call
    site in `mod.rs` the same way `_display_adapter` already is.
    Verified: `cargo fmt --check`/`clippy -D warnings`/`cargo test
    --workspace --exclude portage-bench` all clean; live-reran `em -p
    --prefix /opt/xp --target riscv64-unknown-linux-gnu crossdev --setup`
    in the `aarch64-20260618T101350Z` sandbox — `dev-vcs/git`, `Digest-
    HMAC`, `Crypt-URandom` etc. each appear exactly once now, in correct
    dependency order, at the solver's own chosen versions (`git-2.53.0`,
    not `git-9999-r3`; `Crypt-URandom-0.540.0`, not `-0.550.0`); then ran
    the **real** (non-`-p`) `--setup` — preflight now passes cleanly and
    the run proceeds straight into actually emerging `git-2.53.0`, which
    then hit a *different*, genuine gap — see the next item, not "an
    unrelated pre-existing meson quirk" (first-draft framing, corrected
    after being called out for asserting "pre-existing" without actually
    checking, same mistake as earlier in this investigation).
- ✅ **Fixed 2026-07-11: a `--prefix` host-arch build's own environment
  didn't weave in the host, only the satisfaction check did.** Found immediately after the
  `host_copies` fix above, verified (not assumed) before writing this down.
  `git-2.53.0`'s meson build (a Host-arch tool, landing `to /opt/xp/`) failed:
  `Run-time dependency libpcre2-8 found: NO (tried pkgconfig and cmake)`, then
  fell through to an unavailable subproject (`wrap-mode nodownload`).
  - **Checked, not assumed**: `dev-libs/libpcre2-10.47` genuinely *is*
    installed on the sandbox's real host `/` (`/var/db/pkg/dev-libs/
    libpcre2-10.47`, a working `/usr/lib64/pkgconfig/libpcre2-8.pc`,
    `pkg-config --modversion libpcre2-8` → `10.47` on the bare host env).
    `host_copies` correctly recognized this and correctly did **not**
    schedule a rebuild — the satisfaction decision is right.
  - **The actual gap**: git's `meson setup` invocation (from the real
    `meson.eclass`, not `em`'s own code — `em` doesn't reimplement
    eclasses) was run with `--pkg-config-path /opt/xp/usr/share/pkgconfig`
    only. The real host's `/usr/lib64/pkgconfig`, where the
    dependency that was just judged "already satisfied, don't rebuild"
    actually lives, is never in the search path `em` hands to the build.
    So the decision not to rebuild is correct; the environment `em`
    constructs for the build can't see what it decided not to rebuild.
  - **This is the same gap the user described when dictating
    `todo/greedy-twirling-sparrow.md`**: *"the host vdb is weaved in and so
    the env"* — that plan's drafted scope only covers the satisfaction-check
    weave (`Avail::initial_bdepend`) and the merge-destination routing
    (`Cli::broot()`), not the build environment itself. This item **was**
    that under-scoping; now fixed, `greedy-twirling-sparrow.md`'s remaining
    scope is just the resolver-side pieces.
  - **Root cause found 2026-07-11, `2b4986b`** (initial diagnosis reviewed
    and corrected by an agent before landing — see below): the original
    ESYSROOT/PORTAGE_CONFIGROOT speculation above was wrong. The real
    mechanism: `portage-cli/src/setup.rs`'s `BASHRC_PREFIX` (the `--prefix
    DIR` overlay's search-path recipe, written into the prefix's portage
    config) had a ROOT-keyed guard (`ROOT != "/"`) that had been **silently
    dead since `b3f20c1`** (2026-07-05, "make `--local` standalone, `--prefix`
    the relocatable overlay") — that commit made `--prefix DIR` set `EPREFIX`
    instead, and `em` always resolves `ROOT` to `"/"` once `EPREFIX` is set
    (`build/shell.rs`'s `root_var`). So the guard's condition was
    unconditionally false for every real `--prefix` build: not just
    `PKG_CONFIG_LIBDIR` (which was never set at all, the piece originally
    suspected), the *entire* block — `PKG_CONFIG_PATH`, `CPPFLAGS`,
    `LDFLAGS`, `CMAKE_PREFIX_PATH` — never ran. Only a separate
    EPREFIX-keyed guard (added later, `7482a83`, to fix cross-toolchain-
    wrapper `PATH` specifically) survived. Confirms the original "trace
    which env var meson.eclass derives `--pkg-config-path` from" framing
    was the wrong mental model — nothing was being *derived wrong*, the
    whole recipe simply never executed.
  - **Fix**: consolidated both guards into one, keyed on `EPREFIX`
    (matching `BASHRC_LOCAL`'s existing model), restoring `PKG_CONFIG_PATH`/
    `CPPFLAGS`/`LDFLAGS`/`CMAKE_PREFIX_PATH`, and adding `PKG_CONFIG_LIBDIR`
    (prefix dirs, then host — `meson.eclass` pins `PKG_CONFIG_LIBDIR` to the
    prefix alone whenever the env var isn't already set, replacing
    pkg-config's own built-in default entirely, unlike `PKG_CONFIG_PATH`,
    which is additive — matching `BASHRC_LOCAL`'s already-correct handling
    of the identical meson quirk). Autotools was never affected: plain
    `pkg-config` (no `PKG_CONFIG_LIBDIR` override) searches `PKG_CONFIG_PATH`
    additively over its own compiled-in host default, so it never lost host
    visibility even while the block was dead — only lost prefix visibility
    (see next). `CPPFLAGS`/`LDFLAGS` have no equivalent override-vs-additive
    gap (compiler built-in `-I`/`-L` defaults already cover the host).
  - **Wider regression the same dead guard caused, also fixed by the same
    patch**: since the *whole* block was dead, not just `PKG_CONFIG_LIBDIR`,
    prefix-*installed* packages were also invisible to their dependents —
    an autotools build couldn't find a `.pc` merged into the prefix at all.
    Fixing only `PKG_CONFIG_LIBDIR` in a guard that still never executed
    would have fixed nothing; re-keying the whole block was required.
  - **Verified**: a new test (`overlay_bashrc_actually_exports_search_paths_
    at_runtime`) actually *sources* the recipe with the real runtime env
    (`ROOT="/"`, `EPREFIX=<dir>`) and checks the resulting exports — the
    prior test only grepped the source text for `"CPPFLAGS"`, which passed
    against the dead guard too and would never have caught this. Also
    live-verified directly on this host (which genuinely has
    `libpcre2-10.47` installed): sourced the real generated `bashrc` with
    `ROOT=/`,
    `EPREFIX=<scratch dir>`, and a stub `get_libdir` returning this host's
    real `lib64` — `pkg-config --modversion libpcre2-8` now succeeds
    (`10.47`) via the constructed `PKG_CONFIG_LIBDIR`, where it failed
    before the fix (no `PKG_CONFIG_LIBDIR` set at all → pkg-config's own
    default, which does include the host, but meson's own fallback template
    ignores that and hardcodes prefix-only once `PKG_CONFIG_LIBDIR` is
    unset).
  - **Separately flagged by the user**: this is now the *third* place doing
    a close variant of "walk DEPEND/BDEPEND, track a growing per-root
    availability set seeded from a VDB, decide what's missing" —
    `preflight::check`/`bdepend_avail::Avail`, `host_copies::compute`, and
    the solver's own dual-root `@host` aliasing in `portage-atom-pubgrub`.
    Asked Fable for a consolidation plan to reduce this triplication; see
    the new item below once that lands.
- ✅ **Fixed 2026-07-11, `d869c5c`: `em crossdev --setup -p`/`-a` now shows
  the full depgraph (including preflight validation), not just the
  config-init preview.** User: "setup -p and -a should provide the depgraph
  not just the init info." `emerge.rs`'s `if cli.pretend { return ...; }`
  used to return before `preflight::check` ever ran, so a `-p` preview
  could never reveal a plan that would fail preflight during a real run —
  exactly what made the false "the flags fix it" lead above look plausible
  for a moment.
  - **Fix**: unified the two separate exit-code checks (one per
    pretend/non-pretend branch) into one that runs regardless of
    `--pretend`, then moved `preflight::check` — still skipped under
    `--nodeps`, that guard's condition untouched — before the `--pretend`
    return. `relocate`/`distdir`/`work_base` and the `--ask` confirmation
    stay after it: real-run-only work a preview never needed.
  - **Verified**: full test suite (192, all passing), clippy/fmt clean, and
    live-checked in the `aarch64-20260618T101350Z` sandbox — `crossdev
    --setup -p` still shows the full 6-stage plan correctly (no
    regression), and the `--nodeps`-guarded glibc-headers stage still shows
    no preflight output under `-p`, confirming the `--nodeps` skip survived
    the reorder untouched. The specific real preflight-failure scenario
    this item was found from (the `dev-perl/Digest-HMAC` closure) no longer
    reproduces in this sandbox — that closure was removed entirely by the
    crossdev keyword-acceptance fix earlier this session — so the positive
    "does `-p` now surface a real preflight failure" case rests on
    `preflight::check`'s own logic being completely unchanged (only its
    call site moved), not a fresh live repro of a failing case.
- ✅ **`host_copies::compute`'s forward-reference bug, found by a second
  Fable review — fixed 2026-07-11, `ef0cd00`.** The `5989eb1` splice-at-0
  fix above was itself unsound in two distinct ways, both found by
  independent review rather than live repro. First: copies were spliced at
  plan position 0 unconditionally, wrong whenever a copy depended on a
  seeded `MergeRoot::Host` entry not at position 0 (a forward reference). A
  first fix attempt tracked a per-copy "not before this position" constraint
  (`seeded_position: HashMap<Cpn, usize>` + stable-sort). Second review
  (asked to specifically probe "does this add too much complexity" — user's
  own instinct, confirmed correct) found that design unsound too: the
  constraint doesn't propagate across two separate top-level consumers that
  share a dependency already resolved by an earlier one (concrete
  counterexample: `t1(BDEPEND=liba)`, `t2(BDEPEND=libb)`,
  `libb(BDEPEND=liba)` — `liba`'s position constraint from `t1`'s visit
  isn't visible when `libb` gets resolved during `t2`'s visit).
  - **Fix, landed**: redesigned `compute` to interleave copies directly into
    the output during the walk, instead of computing them separately and
    splicing/sorting afterward — iterate `target_order` in original order,
    and for each Target entry, recursively append its not-yet-emitted host
    dependencies immediately before it, then push the entry itself. A copy
    is always immediately before its first (and only) consumer by
    construction — no position tracking, no sort, no offset math needed.
    Less code than the position-tracking attempt, and provably correct for
    the class of bug that one had.
  - **Genuinely unsolvable case, documented, not "fixed"**: a seeded
    `MergeRoot::Host` entry positioned *after* a Target entry that
    transitively depends on it (via a newly-discovered copy) has no valid
    linear order — `compute` never repositions existing `target_order`
    entries, only inserts new copies around them. This is `preflight::
    check`'s job to catch (fail loudly), not `compute`'s to silently
    "solve" by reordering the solver's own seeded entries. Covered by
    `seeded_host_entry_after_its_dependents_consumer_is_unsolvable_by_design`.
  - **Verified**: 3 new/updated regression tests (Fable's exact
    counterexample, the unsolvable-by-design case, the existing
    no-duplicate-seed test updated for the new interleave shape) plus the
    full suite/clippy/fmt, all clean.
- ✅ **Performance regression, user-flagged ("the speed regression is
  severe") — investigated and fixed in two parts, 2026-07-11.** Routine
  `em -p www-client/firefox` vs `emerge -p` benchmarking (prompted by the
  two fixes above) showed `em` had regressed from its historical ~0.79-1.0s
  (`benchmarks/BENCHMARKS.md`, dated to `8e0e8ca`, 2026-06-14 — a ~4×
  speedup over emerge) to ~2.1s (only ~1.7×). Ruled out the same day's
  preflight-before-`--pretend` reorder first (direct A/B hyperfine of a
  worktree at the pre-reorder commit vs current showed *identical* timing,
  ~2.14s both) before reaching for `git bisect run` over the real historical
  range.
  - **Root cause 1, found by automated `git bisect run`** (~212 relevant
    commits over 27 days, built+timed at each step in a dedicated worktree
    per `[[benchmark-baseline-worktree]]`, `em -p firefox` ×3/commit, min,
    1.5s threshold): `762e6456` ("fix(depgraph): check USE-dep brackets
    against installed VDB packages", 2026-07-05). That fix made
    `bdepend_avail.rs`'s `AvailEntry` eagerly call `InstalledPackage::
    use_flags()`/`iuse()` — two separate file reads — for *every* package
    in the VDB (712 on the dev host) at `Avail` construction time, to
    support checking USE-dep brackets (`[flag]`) against installed
    packages. Only a small fraction of those 712 entries are ever actually
    checked against an atom with USE-dep brackets.
    - **Fix, `ad98af9`**: `AvailEntry` stores `installed: Option<
      InstalledPackage>` instead of an eagerly-extracted `use_info`; the
      `use_flags()`/`iuse()` calls move into `use_deps_satisfied`, reached
      only once `atom_satisfied`'s cpv/slot match already succeeded *and*
      only when the atom actually has USE-dep brackets — the common case
      (no USE-deps on the atom, or an early `.any()` match) touches no
      extra files. `em -p firefox`: ~2.1s → ~1.4s.
  - **Root cause 2, found by isolating the remaining gap**: the same
    "eager USE/IUSE for the whole VDB" pattern existed *independently* in
    `query/depgraph/installed.rs`'s `load_one` (feeding
    `load_target_installed`, the ROOT/RDEPEND installed view) — a separate
    function from `load_host_installed`, which the first suspicion (from
    the prior session) wrongly assumed was the sole remaining
    contributor. Isolated by stubbing each function's USE/IUSE extraction
    independently rather than both at once (the initial combined-stub
    measurement couldn't distinguish them): stubbing `load_host_installed`
    alone gave *no* improvement (~1.4s unchanged); stubbing `load_one`
    alone reproduced the full remaining gap (~1.4s → ~1.05s).
  - **The real, deeper problem, per user**: *"the main problem is that
    read_field shouldn't be implemented like that but be caching."*
    `InstalledPackage::read_field`/`read_field_opt` (`portage-vdb`) re-read
    a flat file from disk on *every* call, so independent VDB scans of the
    same root (there are 3-4 per `em -p` invocation: `preflight::check`'s
    `Avail::initial_depend`/`initial_bdepend`, plus `load_target_installed`/
    `load_host_installed`) each re-read the same ~700 packages' fields from
    scratch, with no sharing between them at all.
    - **Design question, asked and answered**: per-instance cache (safe,
      but doesn't dedupe across independent scans) vs. a global,
      path-keyed cache (bigger win, but risks staleness if anything expects
      a live view mid-run). User: *"a global cache would be correct... but
      ideally the vdb should be locked between resolution and final merge
      to begin with"* — then, on granularity: *"the lock should be at
      entry-granularity though."*
    - **Checked against real portage first** (`/usr/lib/python3.13/
      site-packages/portage/dbapi/vartree.py`, installed on the dev host):
      confirms the entry-granularity call. Real portage's own `vardbapi`
      takes a whole-VDB lock (`lock()`/`unlock()`, `lockdir(self._dbroot)`)
      only briefly for structural directory ops, but the lock actually held
      around each individual merge/unmerge (`dblink`'s `_acquire_slot_locks`
      / `@_slot_locked`) is scoped per `(cp, slot)` — exactly entry
      granularity, not whole-database. Real portage also bumps each
      package directory's mtime after any write (`_bump_mtime`, "so
      consumers can use directory mtimes to validate caches") — a
      pull-based staleness check; this codebase uses push-based
      invalidation instead, which is sufficient here because (verified via
      grep) `Vdb::register`/`unregister` are the *only* two VDB writers in
      the whole codebase — no external-writer case to guard against the
      way portage's mtime approach does.
    - **Fix, landed**: a new `portage-vdb/src/field_cache.rs` module — a
      process-wide cache keyed by absolute file path, deliberately
      decoupled from `std::fs` (`get_or_fetch` takes a closure that
      performs the actual read, so the cache module has zero knowledge of
      *how* a field is fetched — future backends need no changes here).
      `read_field`/`read_field_opt` consult/populate it;
      `Vdb::register`/`unregister` call `invalidate_entry(pkg_dir)`
      afterward, dropping exactly that one entry's cached fields (e.g. a
      same-version rebuild overwriting USE in place) — every other
      package's cache entries are untouched. New regression test
      `register_over_existing_entry_invalidates_cached_fields` pins this:
      registers, reads, re-registers with different USE, asserts *both* the
      original and a fresh handle at the same path see the new value.
      `em -p firefox`: ~1.4s → ~1.15s (3.17× faster than `emerge -p`, up
      from ~1.73× right after fix 1). Parity unaffected across every
      benchmarked target (`benchmarks/bench-em-vs-emerge.sh`).
    - **Not fully back to the ~0.79-1.0s June 14 baseline** — the
      remaining gap is most likely the *first* read of each field still
      being real, unavoidable I/O (the cache only removes *redundant*
      re-reads across scans, not the first one), plus the user's own
      already-flagged, deliberately-deferred follow-up: "a second pass to
      reduce the amount of spurious allocations and missing interning."
      Not started.
    - **Cross-process VDB locking (real OS-level file locks, matching
      portage's `lockdir`/`_slot_lock`) is explicitly out of scope for this
      fix** — this codebase never had that (pre-existing gap, not
      introduced or worsened here) and the user deferred it: "we'll tackle
      global locking later."
    - **Relevant existing plan**: `todo/dedup-availability-walks.md`'s
      "Step 1" already proposed unifying `Avail::initial_bdepend` and
      `load_host_installed` into one shared BROOT-availability loader,
      motivated by this exact duplication. This fix addresses the
      *performance* consequence of that duplication (and extends the
      benefit to `load_one`/`load_target_installed`, which Step 1 didn't
      cover) without doing the structural unification — Step 1's other
      motivation, eliminating "the same bug fixed twice" drift risk,
      stands on its own and is still worth doing separately.
- ✅ **Fixed 2026-07-10: crossdev's host-arch keyword acceptance was too
  eager — a blanket `**` silently preferred a live `9999` ebuild over a
  perfectly good dated snapshot.** User: "we should not auto-accept live
  ebuilds... right now our auto-unmasking is too eager." This turned out to
  be the *actual* root cause visible in the plan (`cross-riscv64-unknown-
  linux-gnu/binutils-9999`/`gcc-17.0.9999` in every earlier capture this
  session) — not `host_copies`, whose own fallback was already fixed
  (`5989eb1`) and correctly picks `dev-vcs/git-2.53.0`. The `**` was written
  unconditionally for every host-arch cross-category package (base
  toolchain + `--ex-pkg` extras) in `crossdev/mod.rs`'s `cross_env_entries`.
  - **Checked against real crossdev (`/usr/bin/crossdev`'s own
    `set_keywords()`), not assumed**: its default (no explicit version
    requested) writes `${TARCH} ~${TARCH}` — the *target's* arch, stable +
    testing — never `**`; `**` only appears in the explicit-version branch,
    paired with an explicit `package.mask >=pkg-9999` unless the version
    requested *is* `9999`. Our own comment claiming "matching how real
    crossdev packages are treated" was wrong.
  - **Checked whether narrowing to `{target_arch} ~{target_arch}` would
    actually work here, not assumed**: yes — `sys-devel/binutils-2.46.0` and
    `sys-devel/gcc-16.1.1_p20260613` both carry a real `~riscv` keyword in
    this repo snapshot, so riscv crossdev wouldn't have been stranded.
  - **But a keyword-only fix would have broken `sys-devel/rust-std`** — our
    own `--ex-pkg` doc example. Every version ships `#KEYWORDS="" #nowarn`:
    permanently unkeyworded by Gentoo convention (a generated, arch-agnostic
    artifact), not "live" in the churning-VCS sense. "Unkeyworded" and
    "live" are different properties and need different handling.
  - **User's design, implemented as given**: for an unversioned atom (the
    only kind `--ex-pkg`/the base toolchain support today), look at what the
    *host* already has installed for the real package and mirror that — pin
    exactly to the installed version if its ebuild still exists in the
    tree, else bound to that version's own release branch
    (`<cat/pkg-{major}.{minor}.9999[:slot]`, which — matching the user's own
    `<sys-devel/gcc-16.2.9999:16` example — includes every dated snapshot on
    the branch, since `X.Y.Z_pDATE` sorts below `X.Y.9999`, but excludes the
    branch's own live/rolling ebuild and any newer branch/slot). Nothing
    installed at all (e.g. `rust-std`, never a host package): bound to the
    newest available version's own branch instead. `**` still used, but
    scoped to one version/branch, not the whole category.
  - **Landed**: `host_installed_versions`/`ebuild_versions`/`branch_bound`/
    `host_arch_keyword_line` in `crossdev/mod.rs`, wired into both the base
    host-arch loop and the `--ex-pkg` extras loop in `cross_env_entries`.
    Six new unit tests (installed+available → pinned; installed+gone →
    branch-bounded; nothing installed+available → newest branch-bounded;
    nothing at all → blanket fallback; `branch_bound`; `ebuild_versions`).
  - **Live-verified, with a striking side effect**: the entire `dev-vcs/
    git`/`app-text/asciidoc`/perl-`Digest-HMAC` closure that drove this
    whole investigation (the original preflight failure, the `host_copies`
    duplicate-entry bug, all of it) **disappears from the plan entirely**
    once `binutils` resolves to the dated `2.46.0` instead of the live
    `9999` — that whole closure was purely an artifact of the live ebuild's
    own `doc` USE default pulling in `asciidoc`; the dated snapshot doesn't
    have it (`USE="... -doc ..."`). Step 1 of the bootstrap plan is now a
    single, clean `binutils` merge. Real (non-`-p`) `--setup` re-verified:
    still passes preflight, proceeds straight into building.
  - Deferred, explicitly out of scope for today: real crossdev's
    explicit-version request syntax (`--ex-pkg pkg:version` pinning exactly,
    matching the user's third design bullet) — `--ex-pkg` only accepts
    unversioned `CATEGORY/PN` atoms today, so there's nothing to wire this
    into yet; the `=cat/pkg-version **` pinned-atom code shape already
    exists in `host_arch_keyword_line` and just needs a caller once
    versioned `--ex-pkg` atoms are supported.
  - **Live-verified the "host offers more" case too, and it's intended, not
    a bug**: switched the sandbox host to `ACCEPT_KEYWORDS="~arm64"` and
    really `em sys-devel/gcc`'d a new SLOT (`gcc-16.1.1_p20260613:16`,
    alongside the existing `gcc:15`) — a subsequent `--init-target`
    correctly re-pinned the alias to `gcc-16.1.1_p20260613:16` with no
    manual step, exactly mirroring the host automatically. Side effect
    caught along the way: `binutils` *also* moved, from our pinned
    `2.46.0` to `2.46.1`, even though nothing about binutils itself
    changed — because `binutils-2.46.1` carries only `~arm64` (no stable
    arm64), and Portage's own per-package `accept_keywords` semantics are
    additive onto the global decision, never exclusive (confirmed by
    reading both ebuilds' `KEYWORDS=`): once the *host's* global policy
    went testing, `2.46.1` became acceptable on its own, independent of our
    pin. User's call once this was reported: **fine as-is** — "if the host
    offers more it is fine to pick it." The mechanism is meant to track
    what the host makes available, not to wall a package off from the
    host's own policy; not a bug to fix.
- ✅ **Fixed 2026-07-10/11: `crossdev --setup` package replaces always hit
  the `environment.bz2` `declare -f` fallback (root cause fixed too) and
  it was silently broken in four different ways, plus one portage-cli-side
  false error.** Follow-on from the same live session above: every
  cross-category replace (binutils,
  linux-headers, glibc, gcc) goes through `try_run_phase_from_env_bz2`
  instead of the real ebuild file, because `unmerge_slot_occupant`'s
  `old_ebuild_path` is built from `old_pkg.category()` — for cross-derived
  (virtual-alias) packages that's never the real on-disk category, so
  `.exists()` is unconditionally false (also fixed today, see below). This
  made every replace exercise `declare -f`'s round-trip fidelity, which
  turned out to be broken:
  1. **portage-cli, `787b399`**: `try_run_phase_from_env_bz2` unconditionally
     invoked `pkg_prerm`/`pkg_postrm` even when the ebuild never defined the
     (optional) phase at all — spurious `command not found`. Guarded with
     `declare -F` first.
  2. **brush `ea00a664`**: heredoc bodies/delimiters got space-reindented on
     `declare -f` re-serialization, breaking `<<-EOF` round-trips outright.
  3. **brush `6645924c`**: a `case` branch's last statement lost its `;`
     before `;;` when it had no redundant trailing separator (idiomatic
     bash); `for var; do` (implicit `$@`) got rewritten as `for var in ; do`
     (explicit empty list, a silent no-op loop). Real constructs from
     `multilib.eclass`/`python-utils-r1.eclass`.
  4. **brush `d96b3a47`**: a multi-line `Word` (e.g. a `$( ... )` command
     substitution spanning lines, itself containing a heredoc) got its
     internal newlines reindented by whatever enclosing `write_indented` it
     sat inside, corrupting the heredoc nested inside it. Required making
     the printer's `SuppressIndent` guard reentrant.
  5. **brush `d1359cfd`**: a compound command's trailing redirect (`{ cmd;
     } 3>&1`, from `sys-libs/glibc`'s `run_locale_gen`) was rendered with no
     separating space after the closer — `}3>&1` lexes as one broken token.
  6. **portage-cli, `185af2e`**: `capture_environment` dumped bash's own
     readonly/dynamic specials (`EUID`, `PIPESTATUS`, ...) unfiltered;
     re-sourcing them later always errored `declare: cannot mutate readonly
     variable` (noisy but non-fatal). Fixed by reusing the already-tested
     `filter_declare_dump` (previously only applied to the Compile→Install
     worker dump) at this call site too.
  - **Methodology note**: bugs 3–5 were found by bisecting the *actual*
    corrupted `environment.old` capture against real Gentoo eclass source,
    not by guessing at synthetic repros — several hand-written minimal
    cases parsed fine both with and without each fix, while the real file
    kept failing, until the real source was used verbatim. See
    `[[empirical-repro-over-synthetic]]` (Claude memory).
  - **Live-verified end-to-end**, four full `crossdev --setup` sandbox runs
    (each rebuilding the whole binutils→headers→glibc→gcc-stage1→glibc→
    gcc-stage2 sequence): zero `syntax error`s once fixes 2–5 landed; zero
    `declare: cannot mutate readonly variable` once fix 6 landed *and* a
    package replaced against a freshly-captured (post-fix) blob — packages
    still replacing a stale pre-fix blob in the same run keep the old noise,
    expected, since stale blobs can't retroactively heal. Final run ended
    with `>>> cross toolchain riscv64-unknown-linux-gnu ready`.
  - brush commits `ea00a664`/`6645924c`/`d96b3a47`/`d1359cfd` pushed to
    `mine/for-portage-repo`; `Cargo.toml`'s pinned rev bumped to `d1359cfd`
    (`23c6fc0`) so non-`[patch]`-override builds pick them up too.
  - **Root cause also fixed, `aff0745`**: `unmerge_slot_occupant`'s
    `old_ebuild_path`-uses-virtual-category bug above — it's *why* every
    cross-category replace hit the fallback at all. `run_merge` already
    copies the ebuild into the VDB entry itself as `<PF>.ebuild`,
    independent of category correctness — source that instead of
    re-deriving a repo-relative path. Caught one regression along the way
    before landing: copying it straight from `old_pkg.path()` and sourcing
    it in place worked for `pkg_prerm` but then failed `pkg_postrm` with an
    I/O error, because `vdb.unregister()` runs *between* the two phases and
    deletes the VDB directory out from under the still-open reference;
    fixed by copying into `old_work_root` (which outlives `unregister`)
    instead. Live-verified: a full `crossdev --setup` run (fresh binutils
    install, two replaces each of glibc and gcc-16) now shows **zero** "old
    ebuild not found" warnings and zero errors of any kind, across every
    single merge — cross-category replaces use the real ebuild file
    directly, same as any other package, exactly as intended.
- 🟡 **Re-derive "stage1 complete" — attempted 2026-07-11, found a real,
  deeper gap; the original `-j1`/`-j40` race question is still unanswered.**
  The riscv64 sysroot this claim was based on was wiped, so re-deriving it
  meant bootstrapping a genuinely empty `--root` from absolute zero for the
  first time ever (every prior "stage1 complete" run had reused a sysroot
  that already had `perl`/`portage`/etc. installed from earlier, unrelated
  work — masking this). That surfaced two real, fixed-or-documented bugs
  before ever reaching the original bzip2/xz-utils/gettext race question:
  1. **Fixed, `portage-cli/src/crossdev/stages.rs`**: native
     `toolchain_plan`'s "libc" step lacked `--nodeps`, unlike cross's
     analogous break-the-cycle step — a real, unbreakable-by-ordering
     `glibc ↔ gcc ↔ libxcrypt` circular DEPEND in a from-scratch native
     bootstrap (cross avoids it because `gcc-stage1` is a real prior step,
     already installed by the time the full "libc" step runs; native's "no
     two-stage split" design never had that safety net). Confirmed twice
     live — once under a wrong invocation, once under the correct one — same
     result both times, so it's real. `native_plan_is_seed_built_single_stage_gcc`
     test updated accordingly.
  2. **Found, not yet fixed — see `docs/root-topology.md`'s "Plain
     unprivileged toolchain" section for the full writeup.** Past that fix,
     `sys-devel/gcc`'s own resolution still explodes into a huge, real
     dependency closure (`perl` → `perl-cleaner`/CPAN → `sys-apps/portage` →
     `gnupg`/`eselect`/`rsync`, plus `util-linux`/`libarchive` wanting
     `acct-group/root`/`e2fsprogs`) — every single one of those packages
     confirmed already installed on the host doing the test. Root cause:
     the self-contained bootstrap checks these build-tool-shaped DEPEND/
     RDEPEND requirements against the still-empty target ROOT instead of
     BROOT (the host actually doing the compiling), unlike BDEPEND, which
     already correctly checks BROOT. Forcing minimal USE (`-*`) does not fix
     it (`perl`'s `minimal` IUSE defaults off, `-*` only disables). Real fix
     needs the existing BDEPEND-satisfied-at-BROOT machinery
     (`provider/solve.rs`) extended to cover this case — not yet
     implemented, no small patch.
  Also found and documented (not a code bug, a usage footgun): adding
  `--config-root` to a self-contained `--root`/`toolchain --setup`
  invocation defeats the self-bootstrap `ensure_self_contained_prefix` does
  — `Roots::config()` already defaults to following `--root`, no
  `--config-root` needed or wanted here.
  A second, broader gap found chasing why `--local` behaves worse than bare
  `--root` (corrected after an initial wrong diagnosis — the `is_local`
  symlink inversion this was first blamed on is **already fixed** in
  `setup.rs`, don't re-diagnose it): `--config-root` resolution is
  inconsistent across three commands. `em --local DIR setup` writes no
  `make.profile` (deliberate — `--local` must also work on a non-Gentoo
  host). The follow-up step, `em select profile set <profile>`, never
  infers a config root from `--local`/`--prefix`/`--root` at all — it needs
  an explicit `--config-root DIR`, and running it as `em --local DIR select
  ...` silently tries to modify the *host's* `/etc/portage/make.profile`
  instead (permission-denied, no hint the flag was wrong). Skip that step
  and `em --local DIR toolchain --setup` doesn't error either — it silently
  falls back to the host's real profile/config, producing a much bigger,
  more chaotic dependency explosion than the BROOT-satisfaction gap above.
  Full writeup in `docs/root-topology.md`'s "Plain unprivileged toolchain"
  and "`--local` and `--prefix` setup" sections. Use bare `--root` for a
  self-contained native toolchain bootstrap; if using `--local`, the
  lifecycle needs an explicit `em --config-root DIR select profile set
  <profile>` step between `setup` and `toolchain --setup` that nothing
  currently prompts for.
  **Original task still open**: once the BROOT-satisfaction gap is fixed,
  redo the clean `-j1`/`-j40` rebuild of the 4 stragglers to actually answer
  the race question this was supposed to settle.
- 🔴 **Re-merge `app-alternatives/gpg-1-r3` — accepted 2026-07-09, next up.**
  With current `em`, expect `IUSE=nls ssl +reference freepg sequoia` in the
  VDB. If so, close #36 as "already fixed; stale entry" — verified via
  `regen_only` that current code produces correct IUSE
  (`iuse-vdb-already-fixed.md`).

## Out of scope (deferred)

- Tier 3 mutable-BROOT bootstrap on a foreign host (`build-environment.md`).
- Zero-config merged sysroot via `fuse-overlayfs`/`overlayfs` (M3).
- `binrepos.conf`, signing/verify, `em maint binpkg` tooling — see `PENDING.md`.
