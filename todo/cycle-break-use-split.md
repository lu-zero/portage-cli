# Break a hard dependency cycle by building a package twice: flag off, then flag on

Status: 🔴 designed, not started. Found 2026-11-18 on `em toolchain --local --setup` ([[docbook-catalog-rdepend-order]]).
Written from an Opus plan and a Fable adversarial review; nothing here was built or run. The review's unverified items were checked against the code on 2026-11-18; the findings are inline.

## Problem

Some bootstrap cycles have no valid order and no alternative. The live one:

```
docbook-xsl-stylesheets -R-> build-docbook-catalog -R-> util-linux -B(nls)-> po4a -B-> docbook-xsl-stylesheets
```

Both BDEPEND edges are hard, so `repair_soft_inversions` (`portage-atom-pubgrub/src/graph.rs`) drops an RDEPEND edge and the stylesheets' `pkg_postinst`
runs before `build-docbook-catalog` exists. `build-docbook-catalog`'s `|| ( util-linux app-misc/getopt )` has no usable second branch on Linux:
`app-misc/getopt` is keyworded only `~arm64-macos ~x64-macos ~x64-solaris`, so it is masked here and in prefix Linux profiles.
Choosing the other `||` branch (the separate, smaller track below) cannot fix this case; a split build can.

Dropping the other RDEPEND edge does not help either. Host `emerge -pe app-text/docbook-xsl-stylesheets` does exactly that (`build-docbook-catalog`, then
the stylesheets, `po4a`, `util-linux` last). On a fresh root `build-docbook-catalog`'s `src_prepare` then finds no util-linux
(`has_version sys-apps/util-linux || sed … getopt-long`), so its script calls a `getopt-long` nobody installs and the stylesheets' postinst still dies.

The fix is the Gentoo bootstrap pattern, done by the resolver for any cycle and never per package: build one package with a USE flag off so the loop opens,
then rebuild it later with the flag on. Portage does not do this itself; its circular-dependency message only suggests the USE change
(`lib/_emerge/resolver/circular_dependency.py`). No per-package lists, no `package.use` workaround in the toolchain plan.

## Behaviour

For `em -p app-text/docbook-xsl-stylesheets` on a bootstrapped `--local` root:

```
[ebuild  N    ] sys-apps/util-linux-2.42.3  USE="-nls" (bootstrap, rebuilt below)
[ebuild  N    ] app-text/build-docbook-catalog-2.4
[ebuild  N    ] app-text/docbook-xsl-stylesheets-1.79.1-r4
[ebuild  N    ] app-text/po4a-0.74-r1
[ebuild   R   ] sys-apps/util-linux-2.42.3  USE="nls"
```

Every `--json` edge has `order_ok: true`; `hard_cycle_edges` is empty. Without the feature the plan is unchanged.

## Design

### 1. Detect the cycle and pick the flag (`portage-atom-pubgrub/src/graph.rs`)

Done in `d4f0d78f` as `InstallOrderResult::cycle_splits()`, called on demand rather than computed in every `install_order`:

- A witness is any DEPEND/BDEPEND/RDEPEND edge the final order places backwards, whose cycle (every edge on a path consumer ⇝ dep, plus the
  backwards edge) has a DEPEND/BDEPEND edge. Inverted hard edges count too. Pure-soft cycles (gtk+ ↔ icon-theme) are skipped.
- A candidate is a `(package, version, f)` whose `f`-gated edges, dropped, cut every path that closes the cycle; a flag that leaves another
  path closed is not one. A witness with no candidate is still returned, for the "say so" message.
- Ranking: build-time edges first, then package key (`{pkg}-{ver}`), then flag. "Fewest nodes the split adds" is not knowable in the graph
  layer; if it matters, rank by it after the re-order (§3).
- Witnesses are per backwards edge, so the three docbook consumers of `build-docbook-catalog` give three witnesses with one candidate; dedupe in step 3.
- Mechanism 1's `via_choice` edge field (below) is independent; this piece only needs `via_use_flag`.

### 2. Validate the flag (new `portage-resolve/src/cycle_split.rs`)

Turning `f` off for `P` is allowed only if all of these hold, else try the next candidate; with none left keep today's plan and say so.
`cycle_split::check` (`02150913`) covers the first three; the last two are checked on the re-ordered plan (§3):

- `f` is not pinned by `use.force`/`use.mask` (`effective_use::pinned_flags`). "Not `-f` already" needs no check: an edge gated by `f` exists
  only while `f` is on.
- No REQUIRED_USE group newly fails with `f` off. A group the plan already fails is left out, since it is reported on its own.
- `P` sits at one merge root in the plan (see §5, cross builds).
- No package between the two builds demands `P[f]`.
- The re-ordered plan has fewer `cycle_splits()`.
- `P` has no `!f? ( … )` dependency outside the solution: the flag-off build would need a package nobody solved for.

### 3. Split the node in the install order (`graph.rs`, `round/plan.rs`)

Revised 2026-11-18, as an experiment: no second solve. The two-solve shape (phase A with `=cpv -f` in `package_use`, phase B over a synthetic installed
overlay) needed every assembly step to handle two providers, solutions and policies: `RoundOutcome` and `assemble.rs`'s single `final_policy` assume one.
The normal solve already holds every package the final plan needs (util-linux with `nls`, po4a); only the order is wrong.

- Order again with `P`'s `f`-gated edges removed. `P` stays one node, the flag-off build.
- Insert the flag-on rebuild of `P` right after the last package its `f`-gated edges point to; its other deps already precede the first build.
- Accept only when the re-ordered plan has fewer `cycle_splits()` than before.
- Reject the candidate when the order cannot express the split: `P` has `!f? ( … )` deps outside the solution (util-linux has none for `nls`),
  or a package placed between the two builds has a `P[f]` USE dependency.
- The flag-off entry's USE is `final_policy`'s minus `f`, so `cycle_split::check`'s REQUIRED_USE test is evaluated on the right set.
- Cost: one extra ordering pass when a split happens. Candidates needing packages the normal solve did not pick are rejected, not re-solved;
  if real cases need that, the re-solve comes back.

### 4. Plan identity: the same package appears twice

These key by `PortagePackage` or `(pkg, version)` and must key by plan index or instance (checked):

- `query/depgraph/assemble.rs:521` `index_of: HashMap<PortagePackage, usize>`, without even the version, so a second entry silently overwrites the first
  (feeds `build_blockers` and `hard_cycle_edges`).
- `output.rs:1767` `edge_order_ok` keys by `(&PortagePackage, &Version)`, and the JSON edges do too: add `instance`, `bootstrap_use`, from/to indices;
  `order_ok` per phase.
- `resume_completed: HashSet<(MergeRoot, String)>` (filtered in `depgraph/mod.rs:716`): the rebuild must not be skipped on resume. The `String` is a CPV;
  make it `Cpv` while changing the key.
- `--jobs`: the rebuild waits for everything before it that its `f`-gated deps need, like any entry.
- Display: the `USE="-nls" (bootstrap, rebuilt below)` marker and the totals.
- VDB: a same-version replace, the existing `[R]` path.
- Binpkg: no change needed, provided each `PlannedMerge.use_flags` holds its own phase's USE. Reuse (`find_binpkg_reuse` → `use_compatible` in
  `portage-binpkg/src/index.rs`) requires USE∩IUSE to match, and `--buildpkg` writes a new build-id instance (`reserve_container`).
  The `-f` binpkg is never picked for the `+f` rebuild, and a later bootstrap can reuse it for its own flag-off build.
- `preflight.rs` already evaluates `use_flags` per entry, so duplicates work there.
- `DepEdge` is public in a publishable crate, and `output.rs` builds it as a struct literal, so any new field changes both.

### 5. Risks

- Cross builds: configs are per root, but the provider evaluates one `package_use` per solve, and `set_cross_active` points each Host node at its
  Target node's `VersionData` (`host_aliases`). So `=cpv -f` reaches both instances. Accept a candidate only when `P` sits at one merge root in the plan.
  Code reading confirms Host nodes take all their plan-time USE from the target's config: `host_native_deps` (`provider/solve.rs` ~556) only re-stamps
  the Target `VersionData`, and the plan's `use_flags` come from the one `final_policy` (`assemble.rs` ~454). That contradicts `root-topology.md`
  ("builds against BROOT's config"). It is a separate gap, not fixed here; what USE the Host build actually gets at merge time was not traced.
- A sub-slot change between the two builds triggers slot-operator rebuilds after the flag-on rebuild.
- `--root-deps=rdeps` drops the DEPEND edges, so the feature stays idle there: correct.
- `package.provided` nodes have no edges, so they never appear in a witness.
- The toolchain staged plan needs no change in `crossdev/`. Each step goes through `crate::emerge_atoms` (`crossdev/mod.rs` ~486) and the normal depgraph,
  so the split applies per step. A step's `use_override` is a make.conf-layer USE, and the split's `package.use` entry overrides it.
  `--nodeps` steps never build a graph, so they never split.

## First live run (2026-11-18, `4943646a`)

`em -p -v --cycle-split --local /var/tmp/local-test app-text/docbook-xsl-stylesheets`: profile set, empty VDB, no `package.provided` seed,
so it is not the bootstrapped root the docbook failure came from. 225 packages; eight genuine hard cycles remain (glibc ↔ gcc, xz ↔ elt-patches, …),
which the toolchain steps and `package.provided` handle, not this feature.

- The split fired, on `dev-libs/libxslt` with `-crypt`, not on the docbook cycle: only the first accepted candidate is applied per plan, and the
  witnesses come in edge order. Several cycles need several splits, so `MAX_CYCLE_SPLITS` is needed after all.
- Rejections looked right: `elibc_glibc` on `virtual/libcrypt` and `libgcrypt` is `Pinned`; `python_targets_python3_14` on
  `meson-format-array` fails `REQUIRED_USE`.
- Both libxslt rows display `USE="crypt"`: the display path computes USE on its own (step 5).

## Sandbox run on a bootstrapped `--local` root (2026-11-18, `924098f2`)

Fresh crossdev-stages sandbox `em-cycle-split` (`setup` + `prepare --bare`), `em setup --local /var/tmp/lt`, then
`em -p -v [--json] [--cycle-split] --local /var/tmp/lt app-text/docbook-xsl-stylesheets` (103 packages).

- Without the flag, three docbook RDEPEND edges point backwards (`docbook-xml-dtd`, `docbook-xsl-stylesheets`, `docbook-xsl-ns-stylesheets`
  → `build-docbook-catalog`), the live failure.
- With it: `util-linux` (-nls), `build-docbook-catalog`, …, the stylesheets, `po4a`, then the `util-linux` rebuild. Two of the three are fixed.
- `docbook-xsl-ns-stylesheets` closes a second cycle: ns -R-> `build-docbook-catalog` -R-> `util-linux` -D(pam)-> `sys-libs/pam`
  -B-> ns (pam's docs, unconditional). The only candidate, `util-linux` `pam`, is rightly rejected: `REQUIRED_USE="su? ( pam )"`.
  Its postinst runs `build-docbook-catalog || die` too, so a real build still fails, at ns instead.
  Opening it needs a flag *set* per candidate (`-pam -su`): when REQUIRED_USE fails, also turn off the flags whose groups force `f` on.
- The `--json` edge `build-docbook-catalog` → `util-linux` shows `order_ok: false` although the flag-off build precedes it: `output.rs`
  `edge_order_ok` keys by `(pkg, version)`, so the rebuild wins (step 5).
- The PDEPEND `setuptools` → `trove-classifiers` `order_ok: false` appears without the flag too; unrelated.
- Unrelated gap found on the way: with no `GENTOO_MIRRORS` anywhere, portage falls back to `make.globals`' `http://distfiles.gentoo.org` and
  tries it before SRC_URI; em has no such default (`gentoo_mirrors_list`, `ebuild/mod.rs`), so `em setup --local` failed on a gitweb 502 for
  baselayout. Worked around with `env GENTOO_MIRRORS=http://distfiles.gentoo.org`.

## Flag sets (2026-11-18, `003325fe`)

`cycle_split::check` now turns off `x` too when turning a flag off breaks a `x? ( … )` REQUIRED_USE group (unless `x` is pinned), and a
second cycle through an already split package adds its flags to that build. Sandbox re-run, same command:
`util-linux` (`-nls -pam -su`), `build-docbook-catalog`, both stylesheets, `pam`, …, `docbook-xml-dtd`, `po4a`, `util-linux` rebuild.
No cycle left unsplit; the only `order_ok: false` edges are the `output.rs` keying artifact and the unrelated PDEPEND.
The build takes `PlannedMerge.use_flags` (`merge/mod.rs`), so a real merge would build the first copy flag-off. Not yet run for real.

## First real merge (2026-11-18, sandbox `em-cycle-split`)

`env GENTOO_MIRRORS=http://distfiles.gentoo.org MAKEOPTS=-j8 em --local /var/tmp/lt --cycle-split --jobs 4 app-text/docbook-xsl-stylesheets`:
76 of 103 merged, then `app-text/opensp-1.5.2-r10` failed (entry 70), before any split package. Not a split problem: the ebuild's
`export CONFIG_SHELL=${BASH}` gets an empty value, because brush sets `$BASH` from its shell name (`wellknownvars.rs`), not a bash path.
autoconf 2.59's `SHELL=${CONFIG_SHELL-/bin/sh}` then substitutes `SHELL=''` (config.log), and make runs `-c`. Blocks the run until fixed.

## Second real merge (2026-11-18, after `31b6d107`)

opensp now builds. The split ran as planned: the first `util-linux` was recorded with `nls`, `pam` and `su` off (VDB `USE`), and
`build-docbook-catalog` merged after it. Then `build-docbook-catalog`'s own postinst failed, for two reasons outside this feature:

- `MAKEOPTS=-j8` in the process env: `init_build_env` copies env `MAKEOPTS` into the shell at the start of every phase, overwriting the
  ebuild's `src_configure` `export MAKEOPTS+=" EPREFIX=${EPREFIX}"` (bug #490754 pattern), so `src_install` installed to `${D}/usr/sbin`, not
  `${ED}`. Portage keeps a phase's changes for the next phase. Only env `MAKEOPTS` is affected; one from make.conf is not re-applied.
  Not fixed; the runs since go without env `MAKEOPTS`.
- The prefix had no `bin/bash` (my shortcut: `em setup --local` straight to docbook, skipping the toolchain steps), and `src_prepare`
  rewrites the shebang to `#!${EPREFIX}/bin/bash`. Installed `app-shells/bash` into the prefix first.

## Tests

- Unit, `portage-atom-pubgrub/src/provider/tests.rs` (`InMemoryRepository`): dxsl R bdc; bdc R `|| ( util-linux getopt )` with getopt absent (genuinely absent,
  not merely unlisted); util-linux IUSE `nls` on, BDEPEND `nls? ( po4a )`; po4a B dxsl. Expect `util-linux(-nls) … po4a, util-linux(+nls)`.
- Variants that must block the split: flag forced, REQUIRED_USE conflict, a `[nls]` demand from an earlier package, a flag-off build that closes another cycle.
- A pure-soft cycle is unchanged; the same plan comes out for shuffled insertion order.
- `portage-cli/src/query/depgraph/tests.rs::fixture`: every JSON `order_ok` is true, `build_blockers` valid, resume does not skip the rebuild.
- Live: `em -p --json --local DIR app-text/docbook-xsl-stylesheets` with `jq '.edges[]|select(.order_ok==false)'` empty; then a real
  `em toolchain --local --setup` passes the docbook packages; then a `portage-bench` run.

## Order of work (one commit each, tests with the code)

1. refactor(cli): resume progress keyed by `Cpv` (done, `82b9cb32`). The phase-slice lookup for `index_of`/`edge_order_ok` moves to step 4: with
   one phase it would be an unused parameter.
2. feat(pubgrub): cycle witness with candidate `(package, version, flag)` edges (done, `d4f0d78f`).
3. feat(resolve): `cycle_split.rs` validation (done, `02150913`).
4. feat: the order split (`graph.rs`) and its use in `shape_plan`, behind a hidden flag at first, plus edge lookup that tells the two entries apart\n   (done: `081b72c5`, `09120502`, `4943646a`, `924098f2`; up to eight splits per plan).
5. feat(cli): display, JSON, resume, jobs and binpkg handling.
6. perf: order cache reuse in `shape_plan`, if the bench shows a cost.
7. docs: `docs/design/architecture.md` (resolution stages, divergences), the user docs for the new row marker, and this file moved to `todo/done/`.

## Separate smaller track: cycle-aware `||` choice

Portage's `dep_zapdeps` demotes an any-of branch that recreates a known circular dependency unless an installed version satisfies it. It does not fix the live
case (getopt is masked on Linux) but is general infrastructure. Constraints from review, if it is built:

- Never demote toward an empty or dropped branch: `provider/mod.rs` ~774-797 strips an unknown dependency out of a `Choice`, so a masked branch becomes empty and
  always satisfied, and the loop would report a clean order for a plan with neither `util-linux` nor `getopt`. Add a test where getopt is genuinely absent.
- Choice nodes are created in `portage-atom-pubgrub/src/convert.rs` (`convert_choice_group`, ~551), which already demotes circular build-time `||` branches at
  conversion (~557-570); keep the two consistent.
- Carry the `Choice` on the edge (`via_choice: Option<PortagePackage>`, set only when every path to the target goes through a `Choice`) instead of a
  `via_any_of` flag plus an owner map. Clear the avoid set at the start of each `resolve_with_repairs`.
- The demotion in `choose_version` (`provider/solve.rs`) is a preference placed after the installed-branch heuristics and must never return `None`.
