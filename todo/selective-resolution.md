# Selective resolution — installed-satisfies-atom, set provenance, `em -p @world`

STATUS: **investigated 2026-07-26, nothing implemented; plan settled, no open
questions.** From two Fable investigation rounds plus live differential testing
against emerge 3.13 on the arm64 host, including an instrumented `_emerge` shim.
Read the **measurement trap** below before re-measuring anything here.

## Symptom

```
$ em -p @world
error: resolution failed:
__internal__/root 0 depends on app-misc/asciinema
```

`app-misc/asciinema-3.2.0` is installed (VDB SLOT `0`), is in
`/var/lib/portage/world`, and its sole ::gentoo ebuild has
`KEYWORDS="amd64 ~ppc64 ~riscv ~x86"` — no `arm64`. The only available ebuild
is keyword-masked here, so em has no candidate and aborts the whole solve.

## Measured parity matrix (this host, emerge 3.13 vs em)

| invocation | emerge | em |
|---|---|---|
| `-p app-misc/asciinema` | rc=1, `All ebuilds … masked` + `(masked by: missing keyword)` | rc=1, opaque |
| `-pu app-misc/asciinema` | rc=0, **silent** | rc=1 |
| `-p --noreplace app-misc/asciinema` | rc=0, silent | rc=1 (`-n` is a dead flag) |
| `-pu --autounmask=n app-misc/asciinema` | rc=0 — proves autounmask is **not** the mechanism | — |
| `-p --exclude app-containers/incus @world` | rc=0, **202 rows**, mass reinstall incl. set members (`app-misc/tmux-3.6a [ebuild R]`); soft on asciinema | rc=1, prints nothing |
| `-pu --exclude app-containers/incus @world` | rc=0, **182 rows**; tmux absent; soft `!!! Ebuilds … masked or don't exist` | rc=1, prints nothing |
| `-pu sys-libs/zlib` (up to date) | rc=0, **no rows** | 1 `[ebuild R]` row |

The `-p @world` rc=1 seen initially was **not** asciinema — it came from
`app-containers/incus` needing `net-firewall/nftables[json]`, which sets
`_autounmask_backtrack_disabled` so `backtrack_depgraph` (`depgraph.py:12013`)
never gets to break the genuine `dev-lang/rust-1.97.1` buildtime self-cycle.
Confirmed:

```
emerge -p --autounmask=n --exclude app-containers/incus @world   →  rc=0
```

with asciinema still reported in that rc=0 run. **Every all-masked or
gone-from-tree world-set entry is soft (rc=0) in portage, selective or not.**
There is no fatal `@world` case of this class to reproduce — a lower parity bar
than first assumed.

### ⚠ Measurement trap — do not repeat

**Any `emerge -p @world` run that fails on this host displays the circular-
dependency subgraph, not the merge plan**, and it looks exactly like a short,
clean plan. `_show_circular_deps` (`depgraph.py:10198-10212`) pops `--quiet`,
force-sets `--verbose` **and `--tree`**, then calls `self.display(handler.
merge_list)` — `circular_dependency_handler.merge_list` is the shortest cycle
subgraph. The `* Error: circular dependencies:` banner goes to **stderr** via
`portage.writemsg`, so under `2>/dev/null` or a fast scroll nothing gives it away.

This produced two false readings during this investigation: "plain `-p @world`
= 8 rows" and "`-pt @world` shows 0 nomerge rows / no tmux" — the latter was
that display's *own forced* `--tree`. Both were used to (wrongly) conclude that
set members are not reinstalled non-selectively. **Always check rc and stderr
before trusting a `@world` row count**, and use `--exclude app-containers/incus`
(or whatever the current blocker is) to get a run that actually displays a plan.

## Portage's mechanism (verified against the Python source)

Not autounmask. Two independent things:

1. **`selective`** — `create_depgraph_params.py:144-154` sets it for `--update`,
   `--newrepo`, `--newuse`, `--reinstall`, `--noreplace`, `--changed-deps`,
   `--changed-slot`, `--selective` (and unconditionally for `remove`, :124).
   `depgraph._want_installed_pkg()` (`depgraph.py:7113-7135`) then returns `True`
   at :7132 instead of `not arg`, so an installed instance may satisfy an atom.

2. **Installed-visibility exemption** — `depgraph.py:7811`:
   ```python
   elif not pkg.installed or (matched_packages and not avoid_update):
       if not self._pkg_visibility_check(pkg, autounmask_level):
           continue
   ```
   Keyword/mask visibility is enforced on an installed candidate **only when
   another visible candidate already matched**. Tail fallback "all are masked, so
   ignore visibility" at :8320. The masked ebuild is `continue`d at :7823 before
   reaching :7924, so `found_available_arg` stays `False` for an all-masked
   package and the installed instance is returned as a `nomerge` node —
   regardless of `selective`.

**Fatal vs soft is decided in argument processing, not in the visibility
machinery**, and keys on *arg type* (`depgraph._resolve`, `depgraph.py:5384-5478`):

- `:5384-5399` — selected pkg installed + `nomerge` + arg is a `SetArg` named
  `selected`/`world` → `_missing_args` → the soft `!!! Ebuilds … masked or don't
  exist` warning (printed at `:10951-10963`). Never consults `selective`.
- `:5407-5445` — `pkg is None` (gone from tree): non-set arg → fatal
  (`_unsatisfied_deps_for_display` + `return 0`, :5439-5443); `@selected`/`@world`
  SetArg → `_missing_args` + `continue` (:5444).
- `:5459-5478` — pkg installed and not selective → the loud
  `All ebuilds that could satisfy … have been masked` block (via
  `display_problems` → `_show_unsatisfied_dep`, `:11054-11055`), then bail with
  `return 0` **only if** the arg is not a `SetArg` named
  `selected`/`system`/`world` (:5474-5478 — comment: continuing is "more friendly").

### Behaviour table to implement

| condition | portage |
|---|---|
| explicit atom, installed, no visible candidate, non-selective | fatal rc=1, `All ebuilds … masked` + per-candidate reasons |
| explicit atom, installed, no visible candidate, selective | rc=0, silent |
| world/selected atom, installed, no visible candidate, non-selective | rc=0, `_missing_args` warning **and** the masked block, plan printed |
| world/selected atom, installed, no visible candidate, selective | rc=0, `_missing_args` warning only |
| world atom, ebuild gone from tree | rc=0, `_missing_args` + `there are no ebuilds to satisfy` |
| explicit atom, **not** installed, all masked | fatal rc=1 — must stay fatal |

## Root cause in em

1. `portage-resolve/src/repo.rs:1037-1079` — `target_package` filters candidates
   by keywords + mask + license; nothing survives → returns
   `PortagePackage::unslotted(dep.cpn)` (:1076), deliberately, so the solver
   fails (its own comment: "emerge: 'no match'").
2. `portage-cli/src/query/depgraph/mod.rs:405-443` builds `root_deps`. The :430
   `bail!("no ebuilds found …")` does **not** fire (the ebuild exists), so a
   zero-version unslotted package becomes a root dependency.
3. `Adapter::versions_for` (`repo.rs:824`) filters through `version_accepted`
   (:592) → zero versions → **no `packages` entry is created under any identity**.
4. `add_installed` (`portage-atom-pubgrub/src/provider/mod.rs:736-758`) registers
   the VDB instance under `slotted(cpn, "0")` (built at `depgraph/mod.rs:543-546`),
   and its stub-version injection is guarded by
   `if let Some(pkg_data) = self.packages.get_mut(&installed.package)` (:741),
   which fails. **The defect is two-fold** — identity mismatch *and* the guard
   silently skipping stub creation when no entry exists. Fixing identity alone
   would not work.
5. **The structural bug:** `resolve_targets` (`provider/mod.rs:939-`) wires root
   deps into the synthetic Root's `VersionData` **after** the construction-time
   `known`-filter (`provider/mod.rs:577-643`). So an all-masked package as a
   *mid-graph* dep is dropped and reported as an autounmask candidate
   (`DroppedDep`, :617-643, surfaced at `depgraph/mod.rs:710-711`), while the
   identical package as a *root* dep is a hard PubGrub `NoSolution`.
6. Surfaces at `depgraph/mod.rs:640/648` via `format_solve_error`
   (`portage-atom-pubgrub/src/lib.rs:100`) → `anyhow::Err` → one line, no plan.
   `__internal__/root` is the `Self::Root` Display arm (`package.rs:270-295`).

Evidence for the identity split: the error prints `app-misc/asciinema` with no
`:0` — the unslotted Display arm (`package.rs:280-284`) — while VDB SLOT is `0`.

**`--noreplace` is a dead flag**: declared `portage-cli/src/cli/merge_flags.rs:117`,
copied at `maint/resume.rs:412` and `crossdev/mod.rs:133`, never read by the
resolver. (`grep -rn noreplace --include=*.rs .` returns exactly those three.)
Same shape as the `-K` dead flag found 2026-07-18.

## Reinstall discriminator: `selective`, **not** set provenance — settled

Set members and explicit atoms reinstall **identically**. Proven three ways:

- `emerge -p --exclude app-containers/incus @world` → 202 rows with
  `app-misc/tmux-3.6a [ebuild R]`; adding `-u` as the single changed input →
  182 rows, tmux gone. Same flip as `emerge -p app-misc/tmux` (`[R]`) vs
  `emerge -pu app-misc/tmux` (nothing).
- A **user-defined set** (cloned `PORTAGE_CONFIGROOT`, `sets/mytest` = tmux +
  which) behaves the same: `-p @mytest` → both `[ebuild R]`; `-pu @mytest` →
  empty. No set-name special-casing in the reinstall path.
- Instrumented `_emerge` shim: in the non-selective `@world` run, tmux is
  selected as *ebuild scheduled for merge* with `arg type=SetArg`,
  `found_available_arg=True`, `_add_pkg` parent `@selected`; the digraph handed
  to `_serialize_tasks` holds **205 merge nodes including tmux**, and
  `_remove_pkg` never fires for it.

`_is_argument` (`:7096-7100`) does exclude `SetArg`, but its only consumer is
the `with_test_deps` gate at `:4002`; `force_reinstall` is set only on internal
`__auto_*` rebuild sets (`:5317`). Neither touches reinstall.

Consistent with [[reinstall-default]] (`7f43c27`/`bb89327`): the explicit-atom
`[R]` default is preserved verbatim for non-selective invocations — staged
toolchain flows name atoms without `-u`/`-n` and keep rebuilding. `-n` restoring
the skip is exactly the selective gate that file anticipates.

## Set-name special-casing — fatal-vs-soft only

The **unsatisfiable-atom** path *is* name-special-cased, by literal set name:

| site | names | case |
|---|---|---|
| `depgraph.py:5393` | `("selected", "world")` | emits the `_missing_args` warning |
| `depgraph.py:5436` | `("selected", "world")` | pkg gone from tree → soft, else fatal |
| `depgraph.py:5475` | `("selected", "system", "world")` | masked installed → soft, else fatal |

So a **user-defined set takes the explicit-atom fatal path**: `sets/mytest2` =
`app-misc/asciinema` → `emerge -p @mytest2` rc=1 with the "have been masked"
block, `-pu` rc=0 silent. Provenance must therefore carry the **set name** (or a
three-way tag explicit / user-set / world-family), not just a boolean.

Corner from source, not live-testable here: a masked-*installed* `@system`
member is soft (`:5475`) but a *removed-from-tree* `@system` member is fatal
(`:5436` omits `system`), and `@system` never gets the `_missing_args` warning
(`:5393`). Recommend collapsing to one world-family rule and documenting the
deviation rather than reproducing this.

## Plan

### Commit 1 — root-target classification + set provenance + reason display

Fixes the reported bug. Layer: the **depgraph caller**
(`portage-cli/src/query/depgraph/mod.rs:405-443` root_deps loop), with the
per-candidate reason computation factored out of `find_autounmask_candidates`
(`repo.rs:1150-1200`, already computes `FilterReason::Keyword/Masked/License`)
into a shared `filter_reasons_for(data, cpn, &version_set, policy)` in
portage-resolve.

Rejected layers:
- **`target_package`** — no VDB access; its unslotted-on-no-match return is
  load-bearing for genuine no-match; threading an installed map in would change
  every other caller silently.
- **`add_installed`/provider** — would make the solver *select* an unbuildable
  version, which the plan filter's `root_pkgs` clause (`mod.rs:790`) would then
  re-list as `[R]`; and the provider has no arg-provenance knowledge, so it
  cannot choose warning vs fatal. No provider change is needed for this fix.

Mechanics, mirroring `depgraph.py:5384-5478`: for each atom, after
`target_package`, if the returned package has no slot **and** `data.versions`
holds the cpn (candidates exist, none accepted — distinguishable from the :430
bail), or the cpn is absent entirely:

- consult the installed map already built at `mod.rs:395-403`; check whether an
  installed instance satisfies the dep (`dep.matches_cpv`, incl. slot/version
  qualifiers);
- **world-family provenance** (`world`/`selected`, plus `system` for the masked
  case) → collect for one batched `!!! Ebuilds for the following packages are
  either all masked or don't exist:` warning (like `:10951-10963`), plus
  per-candidate reasons when candidates exist; drop the atom from `root_deps`;
  continue, rc **0**. Move the currently-fatal :430 bail behind the same check
  (fixes the gone-from-tree `sys-kernel/gentoo-sources:*` world entries too);
- **explicit atom or user-defined-set member** + installed satisfies + selective
  → drop silently, rc 0;
- **explicit atom or user-set member**, non-selective or not installed → fatal,
  but pre-solve and with the real message, bypassing the opaque PubGrub text.

Only **unsatisfiable** atoms are dropped from `root_deps`. Visible set members
stay — em's current always-`[R]`-for-`@world` behaviour is correct non-selective
parity (202 rows above); the reinstall defect is Commit 2's missing gate, not
this.

**Commit 1a — set provenance.** `expand_sets` (`portage-cli/src/emerge.rs:37-`)
must stop flattening to `Vec<String>`. Minimal shape: return
`Vec<(String, Option<SetName>)>` (or a `TargetAtom { atom, from_set }`), threaded
through `resolve_atoms` (`query/mod.rs:114`) into `DepgraphOpts`. **Provenance is
required for correctness** (fatal vs soft), not just message quality. The
`(dependency required by "@selected" [set])` chain display then comes nearly free.

### Commit 2 — the `selective` concept

Add `noreplace` to `DepgraphOpts` (from `merge_flags.noreplace`, `emerge.rs:~385`)
and compute `selective = update || noreplace || newuse || changed_use` inside
`depgraph()`, mirroring `create_depgraph_params.py:144-154` restricted to the
flags em has (see [[cli-flag-parity]]). Three consumption points:

1. Root-target classification (Commit 1) — selective ⇒ explicit atom satisfied by
   an installed instance is a silent success.
2. Plan filter `depgraph/mod.rs:789-792` — gate the `root_pkgs` `[R]` clause on
   `!selective`. Set provenance is **not** part of this gate (see the settled
   section above). The `use_rebuild` clause (:781-786) must keep `-N`/`-U`
   same-version rebuilds alive.
3. `choose_version` (`portage-atom-pubgrub/src/provider/solve.rs:104-107`) —
   under selective *without* `update`, root targets should keep a satisfying
   installed version (`--noreplace` does not upgrade). Add
   `set_selective_no_update(bool)` = `selective && !update`; the Favor arm becomes
   `!self.prefer_update && (!self.root_targets.contains(package) ||
   self.selective_no_update) && range.contains(installed_ver)`.

Also fixes `em -pu sys-libs/zlib` printing an `[R]` row where emerge prints
nothing.

### Commit 3 — diagnostic polish

- `format_no_solution` (`lib.rs:84-94`): render `Self::Root` as e.g. `requested
  targets` for whatever failure classes remain. Do **not** change
  `PortagePackage::Root`'s Display globally without auditing debug/tracing users.
- `(dependency required by "@selected" [set] / "@world" [argument])` trailers
  using Commit 1a provenance.

### Rejected: solve-retry recovery

Dropping the offending root target and re-solving is technically possible
(`root_deps` is a plain `Vec`), but every print-plan-anyway case is an
*argument-classification* case detectable **pre-solve**. Genuine mid-graph
failures (the incus → `nftables[json]` case) are fatal in portage too, and em
already handles the USE-change flavour via cosolve + `exit_code=1`
(`mod.rs:1306`). Retry loops would mask real conflicts nondeterministically.

## Tests

- `portage-resolve/src/repo.rs` `mod tests` (:1210) — `filter_reasons_for`
  returns `Keyword` for a keyword-masked candidate, `Masked` for package.mask,
  multiple candidates each reported; plus a regression that `target_package` still
  returns unslotted for both "no accepted" and "cpn absent". Existing tests there
  use bare `.unwrap()`; **new** ones must use `.expect(msg)` — [[no-unwrap]].
- Factor the root-target decision into a pure function (taking `RepoData`, dep,
  policy, installed map, provenance, selective) so the 6-row behaviour table above
  is unit-testable without a live VDB.
- `portage-atom-pubgrub/src/provider/tests.rs` — root target + Favor +
  `selective_no_update` keeps the installed version; with it false, picks newest
  (encodes current behaviour, which stays for non-selective).
- No existing test asserts the unslotted-root failure or always-`[R]`;
  `target_package_honours_slot_and_version_qualifiers` (`repo.rs:1474`) only
  checks slots. Commit 2 changes observable `em -pu <target>` output — no CLI
  snapshot tests found, but `portage-cli/tests/comparison.rs` (ignored-by-default
  live-system tests) is the model if a live `em -pu` vs `emerge -pu` row is wanted.
- Live verification per [[live-verify-full-pretend-output]]: `em -p @world` prints
  a plan + the masked warning, exit 0 (modulo the genuine incus USE-change →
  `ConfigChangesNeeded` exit 1, which is correct parity); `em -p app-misc/asciinema`
  rc=1 with reasons; `em -pu app-misc/asciinema` rc=0 silent; `em -pu sys-libs/zlib`
  prints nothing.

## Risks

- **Genuine no-match must stay fatal.** Soften only when the atom has set
  provenance, or an installed instance matches the dep *and* selective. An
  uninstalled all-masked explicit atom keeps rc=1 (with a better message).
  `target_package` stays untouched, so every other caller is unaffected.
- **`--emptytree`**: portage skips installed satisfaction under `empty`
  (`depgraph.py:7691-7692`) but world-set atoms stay soft (:5444). So under
  emptytree skip the installed-satisfies branch, keep the set warning.
  `emptytree_native` already bypasses the `already_installed` filter
  (`mod.rs:793`) — the `[R]` gate must not disturb `-e`. See [[em-emptytree]].
- **Cross / `--root` / `--prefix`**: satisfaction uses `target_installed`
  (`mod.rs:395-403`), the same VDB that feeds `add_installed`; `MergeRoot::Host`
  entries (`host_installed`/`add_host_installed`) are not consulted. Root targets
  are always `MergeRoot::Target`. `selective_no_update` must default off so
  `em crossdev`/`em stages` are unchanged — audit their `DepgraphOpts` sites in
  Commit 2 (`crossdev/mod.rs:133` already copies `noreplace`); bootstrap steps
  likely want non-selective semantics. See [[root-topology-refactor]].
- **rc semantics**: the new warnings must not feed `DepgraphOutcome.exit_code`
  (:1301-1310) or `ConfigChangesNeeded` (`emerge.rs:409`) — emerge exits 0 here.
  Conversely, do not drop the existing exit-1 for autounmask candidates.
- **Plan-filter gate**: interacts with `-N`/`-U` (covered by `use_rebuild`,
  :781-786) and the reinstall-fallback append (:804-818) — verify a `-uN` run
  still lists USE-drift rebuilds. See [[newuse]].
- **`resume.rs:412`**: once `noreplace`/selective affects planning, a resumed
  job's re-solve honours it (desirable — same plan); check the snapshot carries
  `update`/`newuse` consistently.
- **USE_EXPAND is off-limits** (another agent is fixing it). Nothing here touches
  it; the only shared surface is read-only `ResolvePolicy` construction.
