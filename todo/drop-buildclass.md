# Drop `BuildClass` (cross-tool stamp) — plan and post-mortem

Status: 🟡 landed **2026-08-07** — package.env letter-faithful (llvm host); HostCodegen PN allowlist; BuildClass/CrossRole/EM_BUILD_CLASS removed. Live verify (Step 5) still open  
Companion reference: [`docs/bash-crossdev-matrix.md`](../docs/design/bash-crossdev-matrix.md)  
Related: Track A in [[root-topology-refactor]] (landed the stamp we now want to unwind)

---

## Why this exists

Track A introduced a planner-stamped `BuildClass` so the build shell would stop
re-deriving “host-class vs target-class” from name allowlists and
`CTARGET`/`TARGET_ABI` sniffs. The intent was sound (one answer, no shadow
guards). The **object of classification** was wrong for the packages that hurt
most: `cross-*` / `cross_llvm-*` toolchain atoms.

Those packages are not a second emerge mode. In **bash-crossdev** they are:

1. selected by stage letters (B/G/K/L/… or R/C/A/P/U for LLVM),
2. given a durable build identity via **host-side `package.env`**,
3. built/updated by the **host** emerge (`emerge cross-<T>/gcc`), not
   `cross-emerge`.

`em` already follows that pattern: virtual alias overlay instead of on-disk
symlinks, but still writes `package.env` + `env/<category>/<pkg>.conf` on
`--init-target`/`--setup`, routes toolchain merges through outer roots
(`use_outer_eroot`), and sources those env files on every build. The durable
contract is the files. `BuildClass` became a **second authority** applied
*after* package.env and able to override the package.env sniff for tool
selection — pointless when it agrees, dangerous when it does not.

---

## Why `BuildClass` was not a sound idea (for cross-*)

### Premise that failed

> The planner “knows” host vs target and throws it away; stamp it so the shell
> never re-derives.

What the planner actually needs to know for **cross-category** packages is
already materialised by bash-crossdev’s design:

| Concern | Durable home |
|---------|----------------|
| Multilib / ABI / `TARGET_*` markers | **package.env** (K\|L vs `*`) |
| `CTARGET` from category | **eclass** (`crossdev.eclass` / `toolchain.eclass`) |
| Who to build / stage order | **packages() / stages** (selection) |
| PATH / EPREFIX / ESYSROOT host-codegen hacks | **em-only** — never in package.env |

Track A folded (1) and (4) into one enum and re-derived (1) at plan time from
a name/`PackageArch` table that can disagree with what we write to package.env
and with bash-crossdev’s letter codes.

### Failure modes we already hit or nearly hit

1. **Name-set inversion (Fable, 2026-08-06).** Early `classify` treated only
   `{linux-headers, glibc, musl}` as target under `cross-*`; everything else
   (including **newlib**) became host-codegen. Silent wrong CC/ESYSROOT —
   the class of bug package.env was invented to prevent.
2. **Dual source of truth.** package.env sourced first; then
   `set_build_class` can force tool_tuple and HostCodegen specials that
   ignore `TARGET_ABI` / `CTARGET` already in the shell.
3. **Wrong grain for HostCodegen.** PATH/ESYSROOT/`-idirafter` specials apply
   only to a **small codegen subset** (binutils/gcc/clang-wrappers/gdb-ish).
   bash-crossdev’s **host-env** letter set is larger (includes llvm-runtimes
   R/U/A/P). One `CrossToolHost` bit cannot mean both “host package.env” and
   “needs em ESYSROOT flip.”
4. **LLVM category hole.** `classify` / `unstamped` only match `cross-`, not
   `cross_llvm-`. CrossRole from the table is ignored for llvm packages;
   tests that only assert “not CrossToolHost” false-green on `NativeTarget` /
   `CrossTarget`. Fixing the prefix by mapping table Target →
   `CrossToolTarget` would push runtimes toward CTARGET-gcc tool selection
   while bash writes **host** env and ebuilds use host clang —
   **worsening** dual-channel drift.
5. **Third classifier.** `BuildClass::unstamped` reintroduces a PN allowlist
   for binpkg/`em ebuild` paths. Three answers: package.env, planner stamp,
   unstamped fallback.
6. **Commit/docs overclaim.** “One table fixes llvm-runtimes” assumed
   `PackageArch::Target` ≡ correct BuildClass; bash-crossdev never classifies
   those letters as K|L target env.

### What is still worth keeping (narrower)

em **does** need something bash-crossdev never encoded in package.env:

- PATH prepend of `<root>/usr/bin` for host codegen tools under offset roots  
- EPREFIX flip so toolchain.eclass `--prefix` matches image layout  
- ESYSROOT=`${EROOT}usr/<triple>/` for those tools  
- optional `-idirafter /usr/include` under prefix  

That is a **HostCodegen** (or equivalent) flag / tiny allowlist — not a full
native/cross-tool taxonomy. NativeHost / NativeTarget / plain CrossTarget may
still earn keep for **non–cross-category** topology (`MergeRoot`, ordinary
`--target` packages); that is orthogonal to dropping CrossTool* stamps.

### Bottom line

For `cross-*` / `cross_llvm-*` packages:

> A full `BuildClass` that only re-states the host/target split package.env
> already persists is between **pointless** and **dangerous**.

Prefer package.env as the contract (bash model); keep at most em-only
codegen annotations that env cannot carry.

---

## Target end-state

```text
package selection     →  packages() / stages / letter-aligned policy
package.env write     →  bash-faithful K|L vs * (see matrix doc)
host emerge / em      →  source package.env; trust TARGET_ABI / CTARGET sniff
shell specials        →  optional HostCodegen only (narrow)
BuildClass            →  remove CrossTool{Host,Target} stamps for cross-*;
                         ideally remove the type if nothing else needs it
```

Virtual overlay stays: delivery of ebuilds, not a second classification story.

---

## Implementation plan (ordered)

Do not reorder casually: each step should leave the tree green and live
crossdev no worse than today.

### Step 0 — Document and freeze direction ✅ (this note + matrix doc)

- [x] Post-mortem + plan (`todo/drop-buildclass.md`)
- [x] bash-crossdev matrix (`docs/bash-crossdev-matrix.md`)
- [x] Point Track A in [[root-topology-refactor]] at this reversal
- [x] PENDING.md queue entry

### Step 1 — Align package.env with bash-crossdev letters ✅

**Goal:** durable files match `/usr/bin/crossdev` `set_env`, independent of
BuildClass.

1. Map every planned PN to a letter (B/G/K/L/C/R/U/A/P/X) — matrix is the
   reference.
2. `env_block(..., target_package)` uses **letter ∈ {K,L} only**, not
   “llvm-runtimes are Target.”
3. Fix current divergence: **llvm-runtimes → host env** (with `TARGET_*`),
   matching bash R/U/A/P.
4. Keep host-mirrored `**` keywords on host-env tools + extras (policy can
   stay; do not couple it to HostCodegen specials).
5. Tests: golden or structural asserts on generated `env/<cat>/<pkg>.conf`
   for at least one GCC and one LLVM target (TARGET_ABI present/absent).

**Exit:** `em -p --target … crossdev --init-target` (GCC + `-L`) writes env
that matches the matrix; no BuildClass change required yet.

### Step 2 — Stop stamping CrossTool* from the depgraph ✅

**Goal:** in-process builds use package.env sniff for tool_tuple; stamps no
longer override.

1. Depgraph: stop setting `PlannedMerge.build_class` to CrossToolHost/Target
   (or stop setting `build_class` for cross-category CPNs entirely).
2. Shell: when `build_class` is None / non-CrossTool, keep existing
   CTARGET/`TARGET_ABI` sniff (already documented as bash marker).
3. Confirm HostCodegen specials still fire via a **temporary** path:
   either leave CrossToolHost only for an explicit HostCodegen allowlist
   computed at the shell from PN (pre-series allowlist), or thread a bool
   `host_codegen` without the full enum.
4. Remove `CrossRole` plumbing if nothing reads it.

**Exit:** `cross-*/newlib` and `cross-*/glibc` tool selection matches
package.env; unit tests that required CrossRole reworked to env-based or
deleted.

### Step 3 — Isolate HostCodegen ✅

**Goal:** em-only specials without a second host/target enum.

1. Replace `matches!(build_class, CrossToolHost { .. })` sites in
   `shell.rs` with `host_codegen: bool` (or a tiny enum with one variant).
2. Source of truth for the flag: PN allowlist matching pre-series
   (`binutils|gcc|gdb|clang-crossdev-wrappers`) **or** an explicit column
   on the letter table — **not** “package.env is host.”
3. bashrc: prefer package.env / CBUILD/CTARGET sniff; drop or narrow
   `EM_BUILD_CLASS` cross-tool-* branches once HostCodegen no longer
   needs them (native / plain `cross-target` may remain if useful).
4. `__worker --build-class=` : pass only what remains, or drop the flag.

**Exit:** HostCodegen sites greppable; no CrossToolHost/Target variants.

### Step 4 — Delete BuildClass (or gut it) ✅

1. Remove `BuildClass` / `CrossRole` from `portage-solver` if unused.
2. Remove from `PlannedMerge`, `RunInner`, privilege worker, Display/FromStr
   round-trips, tests.
3. Remove `BuildClass::unstamped`.
4. Doc pass: `docs/root-topology.md`, Track A bullets in
   [[root-topology-refactor]], `docs/crossdev.md` PackageArch wording
   (llvm runtimes are **not** bash target-env).

**Exit:** no `BuildClass` in the workspace (or only historical mentions in
todo/done).

### Step 5 — Live verify (deferred until builds are free)

Highest value; unit tests are weak for this class of bug.

| Scenario | Expect |
|----------|--------|
| GCC `crossdev --setup` riscv64-linux-gnu | stages; glibc/headers target env; gcc/binutils host env + HostCodegen under prefix/root |
| GCC bare-metal elf/newlib | newlib target env; no host-codegen specials; builds with CTARGET tools |
| LLVM `-L` musl | wrappers host env + HostCodegen as needed; runtimes **host** env per bash; ebuilds/`is_crosspkg` install into sysroot |
| Re-emerge / `em -u` one cross atom | package.env still applied; no stamp required |

Record results under the matrix doc or a dated note.

### Step 6 (later) — Clang support reasoning

Use [`docs/bash-crossdev-matrix.md`](../docs/design/bash-crossdev-matrix.md) when
revisiting llvm/clang crossdev: env letter fidelity first, then whether
HostCodegen for `clang-crossdev-wrappers` is enough under `--prefix` /
`--local`. Do not reintroduce a full BuildClass to “fix” clang.

---

## Non-goals

- Replacing virtual alias with a physical overlay  
- Rewriting cross-emerge / ordinary `--target` package path in the same change  
- Full RootTopology enum migration (still low payoff; see root-topology todo)  
- Live llvm verification as a gate on Step 1–2 unit work  

---

## Suggested commit series (when implementing)

```text
docs: bash-crossdev matrix + drop-BuildClass plan
fix(crossdev): package.env K|L-only (llvm-runtimes host env)
refactor(shell): stop CrossTool stamps; trust package.env sniff
refactor(shell): HostCodegen flag replaces CrossToolHost specials
refactor: remove BuildClass / CrossRole
```

---

## Open decisions (resolve during Step 1–3)

1. **Keywords for llvm-runtimes** after they become host-env: keep host-mirror
   `**` like bash host tools, or target-arch keywords? Default: match host-env
   tools (host-mirror) unless live keyword failures say otherwise.
2. **NativeHost / NativeTarget / CrossTarget:** delete with CrossTool* or keep
   for non-cross topology? Prefer keep until a separate pass proves unused.
3. **EM_BUILD_CLASS:** delete entirely vs keep for plain `cross-target` bashrc
   only. Prefer delete for cross-tool tokens once HostCodegen is explicit.

---

## References

- `/usr/bin/crossdev` — `set_env`, `set_portage`, letter pkglist, `doemerge`  
- `/var/db/repos/gentoo/eclass/crossdev.eclass` — `cross-*` and `cross_llvm-*`  
- `portage-cli/src/crossdev/mod.rs` — `cross_env_entries`, package.env writes  
- `portage-cli/src/ebuild.rs` — package.env source then optional build_class  
- `portage-repo/src/build/shell.rs` — tool_tuple stamp vs sniff; HostCodegen sites  
- Fable-era fix `1971b7c` (table + CrossRole) — correct for newlib under
  `cross-*`, incomplete/overclaimed for llvm  
