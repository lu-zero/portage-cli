# Root topology: scenarios, variants, and the satisfaction-root map

This is the design reference for how `em` models the filesystem locations a
Gentoo build touches. It supersedes the scenario narrative in
[`root-model.md`](../user/root-model.md) (which stays as the historical, builder-side
detail reference). Read this first; cross-link into `root-model.md` only for
the `bashrc`/overlay recipe and the per-phase env (`SYSROOT`/`ESYSROOT`/`BROOT`
assignment in `run_phase`). For **EPREFIX leakage / multi-root path
workarounds** (baselayout, host-tool links, wrong probes) see
[`em-prefix-experiment.md`](./em-prefix-experiment.md).

> **Slop warning.** Verify any claim here against the code before relying on
> it. `Roots` (`portage-resolve/src/roots.rs`) carries
> `satisfaction_root(DepClass)` directly. Historical dead ends: `RootSet` /
> `RootTopology` enums (removed), and **`BuildClass`** (landed then dropped —
> dual authority next to package.env; see
> [`bash-crossdev-matrix.md`](./bash-crossdev-matrix.md)). Older "variant enum"
> sections below are historical; the **Override semantics** table and
> `cli.rs` `base_roots` are the live contract.

## The four roles

A Gentoo build touches up to four distinct filesystem locations. PMS fixes the
*meaning* of each; only the *paths* vary per invocation.

| role | PMS / portage var | governs |
|---|---|---|
| **base root** | (planner "installed" view) | what counts as already installed — seeds the plan |
| **target root** | `ROOT` / `EROOT` | install destination, the new VDB |
| **sysroot** | `SYSROOT` / `ESYSROOT` | build-against headers/libs/`.pc` for `DEPEND` |
| **BROOT** | `BROOT` | where `BDEPEND` tools run (host machine, native `${CBUILD}`) |

**Config is orthogonal to the roles** — it is not a fifth role, it is a single
global path: `PORTAGE_CONFIGROOT`, the profile + `make.conf` source, defaulting
to `/`. `--config-root` overrides it; `--root` does **not** (matching portage's
`ROOT=R emerge`, which changes only the install destination, not the profile).
A separate `config_overlay` (`--prefix`) layers per-user
`package.use`/`package.keywords`/`package.license`/`bashrc` on top of the
profile, never replacing it. (`--local` is self-contained — config lives in
the prefix itself, not overlaid on the host's.)

This matters because in a cross build the host's config and the target's config
genuinely differ: the sysroot's `make.conf` pins `CHOST`/`CBUILD` and carries
target USE flags, while BROOT's carries the host's. A Host-BDEPEND package
(jinja2 built for host python) builds against BROOT's config, not the target
sysroot's. The current single-config `Roots` field cannot express this; the
model below can, because cross points `config` at the sysroot (where crossdev
wrote the target profile) while BROOT's own `etc/portage` remains the host's.

## Override semantics

Each user-facing flag maps to a portage knob and overrides exactly what that
knob overrides — no more, no less:

| flag | target (ROOT) | base (planner VDB) | config (profile source) | `package.*` overlay | EPREFIX |
|---|---|---|---|---|---|
| *(none)* | `/` | `/` | `/` | — | — |
| `--root R` | R | R | **`/`** *(unchanged)* | — | — |
| `--config-root C` | `/` | `/` | C | — | — |
| `--root R --config-root C` | R | R | C | — | — |
| `--prefix P` | P | **`/`** *(host seeds plan)* | `/` | P/etc/portage | P |
| `--local` | ~/.gentoo | **~/.gentoo** *(self-contained)* | ~/.gentoo/etc/portage | ~/.gentoo | ~/.gentoo |
| `--target T` (on EROOT) | EROOT/usr/T | EROOT/usr/T | EROOT/usr/T | — | — |

- **`--root` vs `--prefix` differ in two cells: base and EPREFIX.** `--root`
  moves base to R (empty R → full closure → stage build) and leaves EPREFIX
  unset (installed scripts use host-absolute paths); `--prefix` keeps base at
  `/` (host seeds the plan → only the delta lands in P) and sets EPREFIX=P
  (installed tree under P is relocatable — scripts shebang to
  `${EPREFIX}/usr/bin/...`). **Both leave config at the host** unless
  `--config-root` is set — matching portage `ROOT=` / `PORTAGE_CONFIGROOT`
  (`cli.rs` `base_roots`: config does **not** follow `--root`). Pair
  `--root R --config-root R` for a self-contained config tree under R
  (what `em setup --root` writes make.conf for).
- **`--local` is the standalone unprivileged deployment** — a self-contained
  Gentoo-Prefix at `~/.gentoo`: base = target = `~/.gentoo` (full closure, not an
  overlay), EPREFIX = `~/.gentoo`, config from `~/.gentoo/etc/portage` once
  `make.profile` exists (otherwise host config until setup/select lands one).
  Unlike `--prefix`, it does **not** assume the host is Gentoo — the prefix
  carries its own VDB, config, and (after toolchain setup) its own toolchain.
  - **`--prefix` vs `--local`:** `--prefix P` is the overlay (host stays base,
  delta only — fast path on a Gentoo host); `--local` is standalone (full
  closure). That is what the code does today (`TopologySource::Local` vs
  `Prefix`), not a pending refactor.
- **Multi-root path assumptions** (EPREFIX leakage, wrong probes, baselayout
  vs host-tool workarounds): see [`em-prefix-experiment.md`](./em-prefix-experiment.md).
- **`--target` points config at the sysroot** because crossdev physically writes
  the target profile + `make.conf` there; the host's `etc/portage` remains
  BROOT's config.

The dep-class → role map is fixed by PMS table 8.2:

| dep class | runs on / resolved against |
|---|---|
| `BDEPEND` | **BROOT** (always the build host, native `${CBUILD}`) |
| `DEPEND` | **sysroot** (`ESYSROOT`) |
| `RDEPEND` | **target root** (`ROOT`) |
| `IDEPEND` | **BROOT** (same as BDEPEND; PMS table 8.2) |

Getting a build right is, mechanically, getting this map right. Almost every
hard bug in the cross/stage work has been one role silently standing in for
another — CTARGET leaking sysroot-wide, CHOST invisible to subprocesses,
Host/Target root conflation, build-machine pkgconfig searching the target
sysroot.

## The variant enum (target design — superseded)

> **Superseded.** This section is the original enum proposal. It did **not**
> ship as written: the maintainer chose a field-based approach instead (see the
> slop warning above). `Roots` (`portage-resolve/src/roots.rs`) keeps its flat
> fields but gained `satisfaction_root(DepClass)`; `BuildClass` carries the
> host-vs-target-vs-cross discrimination; the `RootSet` enum below was removed.
> The section is retained because its *role analysis* (the four roles, the
> variant collapse, the satisfaction-root table) is what the field-based design
> delivers on — read it for the model, not as a literal type proposal. The
> "Status" section is authoritative for what's in the code.

Today `Roots` is a flat bag of fields (the doc's earlier "five `Option<PathBuf>`"
undercounted: it's seven path `Option<Utf8PathBuf>`s — config/base/target/broot/
eprefix/config_overlay/config_root_explicit — plus three bools — is_cross_arch/
relocate/installed_view_target_only), and every caller historically had to *know*
which field answers which role. That was the structural debt the cross/stage
session exposed — addressed by `satisfaction_root` + `BuildClass`, not the enum.

The proposed shape makes the variant answer `satisfaction_root(dep_class)` as
a pure function, so no caller holds an ambiguous `&Roots`. Config and its
overlay are sibling globals (defaulting to `/` and `None`); the variant is
only about the four filesystem roles:

```rust
struct RootTopology {
    /// `PORTAGE_CONFIGROOT` — profile + make.conf source. Defaults to `/`.
    /// `--config-root` overrides; `--root` does NOT (portage `ROOT=` parity).
    config: PathBuf,
    /// Per-user `package.*`/`bashrc` overlay (`--prefix`). Layered
    /// on top of `config`, never replaces the profile. `None` otherwise.
    config_overlay: Option<PathBuf>,
    /// The four filesystem roles, collapsed by how many coincide.
    roots: RootSet,
    /// Same-arch vs foreign-triple (CHOST/CBUILD/CTARGET). Orthogonal to the
    /// topology: cross is the same root routing with different compiler
    /// prefixes, not a fourth variant.
    cross: CrossArch,
}

enum RootSet {
    /// All four roles collapse to one path.
    /// `em <atom>` as root.
    Single { root: PathBuf },
    /// BROOT (build host) distinct from target. Sysroot == target.
    /// `--root R` (BROOT=/), `--target` (BROOT=outer EROOT).
    Dual { broot: PathBuf, target: PathBuf },
    /// BROOT, base (sysroot source), and target all distinct.
    /// `--prefix P` (base=/, target=P, BROOT=/).
    Overlayed { broot: PathBuf, base: PathBuf, target: PathBuf },
}
```

The `cross: CrossArch` field (`SameArch` / `ForeignArch` with
`CHOST`/`CBUILD`/`CTARGET`) is orthogonal to `RootSet` because **cross is not
a fourth topology** — it's the same root routing with different triples. The
session's `cross_active` + `is_cross_arch` split (`root_aware.rs:66-72`)
already discovered this empirically: routing is identical, only the compiler
prefixes differ.

### What `satisfaction_root` returns, per variant

| dep class | `Single` | `Dual` | `Overlayed` |
|---|---|---|---|
| `BDEPEND` | root | broot | broot |
| `IDEPEND` | root | broot (cross) / target (native) | broot |
| `DEPEND` | root | target | base (sysroot) |
| `RDEPEND` | root | target | target |

`Single` collapses everything; `Dual` splits BROOT from target; `Overlayed`
adds the base/sysroot distinction. Cross vs native only flips the `IDEPEND`
cell (running root) — the one place `satisfaction_root` needs the `cross`
field rather than the `RootSet` alone, so the signature is
`satisfaction_root(&self, class: DepClass) -> &Path` with `self.cross` read
internally for `IDEPEND`.

## The two axes that determine difficulty

The variant captures **axis 1 — how many distinct roots**. But the scenarios
below show a second, mostly-orthogonal axis that determines how *hard* a build
is: **axis 2 — what BROOT is**.

| | BROOT = `/` (rw) | BROOT = `/` (ro, Gentoo host) | BROOT = prefix subset |
|---|---|---|---|
| **native stage1** | trivial | trivial | Tier 3 bootstrap |
| **cross stage1** | crossdev classic | — | layered on Tier 3 |
| **cross stage4** | + big closure | — | + big closure |

Axis 2 is what *privilege* really controls: root buys "BROOT can be the real
`/`"; unprivileged on a Gentoo host buys "BROOT reads `/`"; unprivileged on a
foreign host forces "BROOT must be bootstrapped into writable space."
**BROOT identity should not be a variant field** — it's a property of *what's
installed at BROOT* (is the host VDB present? are the tools there?),
discovered at runtime, not a structural property of the topology. Mixing it in
would conflate "where do roots point" with "is BROOT self-hosting" (the Tier 3
question, which deserves its own modelling in
[`build-environment.md`](./build-environment.md)).

## The five scenarios

Notation: `C` config, `B` base, `T` target, `S` sysroot (ESYSROOT), `BR` BROOT.
"stage1" is overloaded — see "Two meanings of stage1" below.

### 1. Native stage1, privileged (root)

The seed toolchain is the host's own `/usr/bin/gcc`. BROOT is the real `/`,
read+write.

```
C=/  B=T=<offset>  S=T  BR=/        CBUILD==CHOST
```

- `em toolchain --root /var/tmp/stage1 --setup` (builds binutils/glibc/gcc into
  the offset, single-pass since `CHOST==CBUILD`), then
  `em stages --root /var/tmp/stage1 --stage1` (the `packages.build` set).
- BROOT is the real host `/`; every BDEPEND edge is host-satisfied and dropped.
- Topology: **`Dual { broot: /, target: <offset> }`** + `SameArch`. (`Single` is only
  the bare `em <atom>` case where every role is `/`; any offset splits BROOT from
  target.) Config stays at `/` — portage `ROOT=` parity.

### 2. Native stage1, unprivileged

Two genuinely different sub-cases, split by **whether the host `/` already has
the build tools**:

**(2a) Gentoo host, unprivileged user.** `/` is read-only but present and
complete (real portage VDB, real `/usr/bin/cmake`).

```
C=/ (ro)  B=T=<offset>  S=T  BR=/ (ro)    CBUILD==CHOST
```

- Same topology as (1) — **`Dual { broot: /, target: <offset> }`**; the only
  difference is we can't *write* `/`, but we never need to — BDEPEND is satisfied
  by *reading* the host VDB + host binaries. `em stages --root /var/tmp/stage1
  --stage1` works unchanged.
- **This is just (1) minus root.** For a delta-only deployment into `~/.gentoo`
  on a Gentoo host, use `em --prefix ~/.gentoo` (overlay: host stays base).
  For a self-contained deployment, use `em --local` (see 2b).
- A BDEPEND the host lacks (e.g. jinja2 for a python target the host's jinja2
  doesn't cover): under `--root`, it lands on the real host `/` (privileged,
  writable — portage `ROOT=` parity). Under `--prefix`, the host is
  read-only, so it instead lands in the prefix itself (`Cli::host_roots()` routes
  a `MergeRoot::Host` entry to `outer_roots()`, not the host, when
  `is_overlay()`); satisfaction checking then reads host ∪ prefix VDB
  (`Avail::initial_bdepend`/`load_host_installed`), so a tool already built
  into the prefix by an earlier run is recognized without rebuilding.
  Fixed 2026-07-09.

**(2b) `--local`: self-contained deployment (any host).** The prefix at
`~/.gentoo` is standalone — base = target = `~/.gentoo`, carrying its own VDB,
config, and (after bootstrap) its own toolchain. Works on a Gentoo host *and*
on a foreign host (Debian/Arch/Fedora). Bootstrapping a toolchain into an
**empty** prefix needs a host-tool seed via `package.provided` (empty VDB
hard cycle otherwise) — see [`stages-and-testing.md`](../user/stages-and-testing.md).

```
C=~/.gentoo/etc/portage  B=T=~/.gentoo  S=~/.gentoo  EPREFIX=~/.gentoo   CBUILD==CHOST
```

- `em setup` bootstraps the initial layout: places `make.profile` + minimal
  `make.conf` into `~/.gentoo/etc/portage`. On a Gentoo host the profile
  symlinks into `/var/db/repos/gentoo`; on a foreign host the user provides
  one (or `--setup` fetches a minimal tree).
- BROOT starts as `/` (host tools compile the first packages) and converges to
  `~/.gentoo` once the prefix toolchain exists — axis 2 (runtime BROOT
  identity), not a topology field.
- Topology: **`Single { root: ~/.gentoo }`** (all roles collapse to the
  prefix once bootstrapped). This is what makes `--local --target <T>` work on
  a foreign host: BROOT = `~/.gentoo` (writable, real), target =
  `~/.gentoo/usr/<T>`.
- root-model.md's **Tier 3** for the initial bootstrap phase (mutable BROOT,
  hardest case); converges to standalone `Single` once self-hosting.

**`--local` vs `--prefix` at a glance:** `--prefix P` is the overlay (base=`/`,
host seeds plan, delta only — fast path on a Gentoo host, useless on a foreign
one). `--local` is standalone (base=target=`~/.gentoo`, full closure, works
anywhere). They are the `--root`/`--prefix` distinction specialized to the
unprivileged home-directory case: `--local` adds EPREFIX + self-contained config.

### 3. Cross stage1, privileged (root)

Crossdev's classic flow, into `/usr/<CTARGET>`.

```
C=B=T=/usr/<T>  S=/usr/<T>  BR=/     CBUILD≠CHOST
```

- `em crossdev --target <tuple> --setup` → binutils → headers → gcc-stage1 →
  libc → gcc-stage2. Atoms live under the `cross-<tuple>/` alias onto
  `::gentoo`.
- BROOT is the real host `/` (native cmake/perl/python). Every BDEPEND edge
  resolves against the host VDB.
- Topology: **`Dual { broot: /, target: /usr/<T> }`** + `ForeignArch`.
- Result: a cross-toolchain (`<T>-gcc`, `<T>-ld`, …) plus target glibc/headers
  in `/usr/<T>`, ready to compile target code.

### 4. Cross stage1, unprivileged

Can't write `/usr/<T>`. Whole sysroot goes under a writable offset.

```
C=B=T=<offset>/usr/<T>  S=<offset>/usr/<T>  BR=<offset>     CBUILD≠CHOST
```

- BROOT is **not `/`** — it's the offset's own native toolchain, i.e.
  **`em --local` (scenario 2b) ran first** to produce a host stage1 at the
  offset, then cross is layered on top targeting `<offset>/usr/<T>`.
  On a Gentoo host, `--prefix <offset>` (2a overlay) also works — BROOT reads
  `/` directly.
- This is *exactly* the session's `/var/tmp/cross-stage1-riscv64`:
  `base_roots()` = the outer EROOT (host stage1, the BROOT), `--target` targets
  the sysroot subdir. The jinja2/perl/Host-BDEPEND routing bugs were
  all about BDEPEND edges landing in `base_roots()` instead of `/` or the
  sysroot.
- If the host *is* Gentoo and complete, (2a) applies and BR can read `/` — but
  the session deliberately kept it self-contained under the offset to avoid
  depending on the real machine.
- Topology: **`Dual { broot: <offset>, target: <offset>/usr/<T> }`** +
  `ForeignArch`. (BROOT being a prefix subset rather than `/` is axis 2, not a
  topology difference from (3).)

### 5. Cross stage4 (full target system)

A bootable/installable target system — a real `<T>` stage3+ that boots on
`<T>` hardware. Same topology as (3) or (4) (whichever privilege tier); stage4
just means the *closure* is `@system` + a custom set rather than the
toolchain.

**Inputs:**
1. A working **cross-toolchain** (output of 3 or 4): `<T>-gcc`, target glibc +
   headers in the sysroot.
2. The **target sysroot seeded** with libc + a minimal VDB.
3. Build **`@system` (stage3) + custom set (stage4)** as *target-native*
   packages: each has `CHOST==CTARGET==<T>`, `CBUILD==host`, installs into the
   target root, records in the target VDB.

**The two real hazards (both already worked through in the session):**

- **BDEPEND visibility into BROOT.** A target-native package's BDEPEND (e.g.
  `dev-python/jinja2` for `systemd-utils`) runs on BROOT under the *host's*
  python — must be installed for the host python target, not the target one.
  Unsatisfied BDEPEND must schedule a **`MergeRoot::Host` merge** into BROOT,
  not into the target sysroot. All instances of this bug class have been fixed.
- **Genuine bootstrap SCCs.** `gawk → bison → gettext → libxml2 → meson →
  python → gawk` is a real strongly-connected component with no valid linear
  order. Broken by seeding one member (`--nodeps`), exactly as catalyst/portage
  do for `xz-utils ↔ elt-patches`. Not a bug; an inherent property of
  bootstrapping a self-hosting set.

**Not a hazard, despite prior claims:** "some ebuilds just can't
cross-build." Every such case in the session turned out to be a
misdiagnosed env-var bug (build-machine pkgconfig searching the target sysroot,
fixed by `BUILD_PKG_CONFIG_LIBDIR` → outer EROOT in `de87153`; CTARGET leak;
CHOST invisible to subprocesses). Real cross builds a full
target-native stage3 *without ever executing a target binary on the host* —
the build phase runs the host compiler producing target binaries that don't
run until installed on target hardware. `qemu-user` is at most a per-ebuild
escape hatch for upstream bugs that execute helpers at build time (some
`src_test`, broken ebuilds); it is **never** an architectural stage4
dependency, and `crossdev-stages` (separate tool,
`/home/lu_zero/Sources/crossdev-stages`) is the proof — it produces target
stage3 sandboxes with no qemu involvement.

## Lifecycle: setting up each topology

A root rarely starts empty and usable. `em setup` (layout bootstrap) and
`em toolchain --setup` (compiler bootstrap) are the two lifecycle primitives;
cross adds `em crossdev --init-target` (sysroot config). What each does depends
on the topology being bootstrapped.

### `em setup` — layout bootstrap

Creates the directory skeleton, `make.conf`, `bashrc`, and (for self-contained
roots) `repos.conf` + `make.profile`. Implemented in
[`setup.rs`](../../portage-cli/src/setup.rs); never touches `/`.

| target | what `em setup` writes |
|---|---|
| `--prefix P` (overlay) | skeleton + a `make.conf`/`bashrc` **overlay** (host profile + make.conf stay authoritative; `bashrc` injects `-I$P/usr/include` etc. so the compiler sees the delta) + **host-python/host-tool symlinks** into `P/usr/bin` (the installed tree is relocatable under EPREFIX=P, so ebuilds bake `${EPREFIX}/usr/bin/pythonX.Y` into shebangs; since the overlay borrows the host's python rather than building one, the symlink satisfies those shebangs) |
| `--local` (standalone) | skeleton + **self-contained** `make.conf`/`bashrc` under `~/.gentoo/etc/portage`. Builds its **own** python via `toolchain --setup`; during bootstrap the host's python is reached via PATH/BROOT, never via a symlink masquerading as a prefix-owned file |
| `--root R` (self-contained offset) | skeleton + self-contained `make.conf` (with real `MAKEOPTS`/`ACCEPT_KEYWORDS` — this is the *only* make.conf it reads) + `repos.conf` + `make.profile` symlinked to the host's resolved profile |

The `bashrc` distinction is load-bearing
([`setup.rs:131-157`](../../portage-cli/src/setup.rs)): an overlay (`--prefix`,
`--local`-as-overlay) needs CPPFLAGS/LDFLAGS injection so the compiler sees the
delta layered over the host; a self-contained root (`--root`, `--local`-as-
standalone) must **not** get that injection — it actively breaks builds by
shadowing a package's own version-matched headers with the root's libc
(`gcc libiberty/obstack.c` vs the ROOT's `obstack.h`, found 2026-07-03).

### Plain unprivileged toolchain (`em toolchain --setup`)

Builds a native `baselayout → binutils → os-headers → glibc → gcc` into `--root`
(`BootstrapKind::Native`, single-pass since `CHOST==CBUILD`). Under `--prefix`
(`USE=prefix-guest`) the libc step is skipped. The compiler this produces is
what `em stages --stage1` then builds `packages.build` against.

```
em toolchain --root /var/tmp/stage1 --setup
em stages --root /var/tmp/stage1 --stage1
```

`toolchain --setup` calls `ensure_self_contained_prefix` first
([`crossdev/mod.rs`](../../portage-cli/src/crossdev/mod.rs)) — runs `em setup`
if the destination is non-host, writes `repos.conf`/`make.profile` — so it is
self-sufficient: a fresh empty `--root`/`--prefix`/`--local` becomes a
buildable toolchain in one command. A toolchain into `/` is rejected.

**Footgun — do not add `--config-root` to this invocation.** `Roots::config()`
already defaults to `config_root.or(root)`, so a bare `--root DIR` reads
config from `DIR` itself once `ensure_self_contained_prefix` has bootstrapped
it — that's the whole point of the self-contained model. Bolting on an
explicit `--config-root` (e.g. to work around a `-p` dry run on a
not-yet-bootstrapped root failing with `cannot resolve make.profile` — itself
expected, not a bug: `ensure_self_contained_prefix` only runs on a real,
non-pretend invocation) overrides that default and forces every subsequent
command to keep reading the *host's* config instead, silently fighting the
self-bootstrap it just did. Found live 2026-07-11 chasing a dependency-cycle
bug that turned out to be real regardless of this mistake, but the mistake
itself cost significant detour time — just run the plain command twice
(`-p` will fail cleanly the first time on a fresh root; the real, non-`-p`
run is what matters and self-bootstraps correctly).

**DONE (was: "known gap") — a truly from-scratch bootstrap needs build-tool
DEPEND/RDEPEND satisfied at BROOT, not just BDEPEND.** Closed 2026-07-11
(`b9d4fbb`, same commit as the `--root` config-resolution revert above):
`broot_filtered` (`portage-atom-pubgrub/src/provider/solve.rs`) now runs
`append_unsatisfied_broot` for DEPEND/BDEPEND/IDEPEND alike, via
`host_satisfied_on_broot`/`virtual_satisfied_on_broot` for Choice/SlotChoice
edges — closing exactly the gcc→perl→rsync/gnupg explosion described below.
Left for historical context: a clean
`--root` with nothing installed hits a real dependency explosion once the
"libc"/"gcc" steps resolve past the toolchain itself — `sys-libs/glibc`'s
`COMMON_DEPEND` (shared by DEPEND *and* RDEPEND) reaches `sys-devel/gcc`,
and `virtual/os-headers → linux-headers → dev-lang/perl` pulls in perl's
`!minimal?` `PDEPEND` tail (`perl-cleaner`, `virtual/perl-CPAN`, ...), which
in turn reaches `sys-apps/portage` (`rsync-verify` → `gnupg`), `eselect`,
`net-misc/rsync`, and from there `sys-apps/util-linux`/`app-arch/libarchive`
want `acct-group/root`/`sys-fs/e2fsprogs`. None of this is a real bug in the
sense of a wrong edge — every one of these packages was confirmed already
installed on the host doing the test (`/var/db/pkg/...`) — it's that the
self-contained bootstrap checks these build-tool-shaped requirements against
the still-empty target ROOT instead of BROOT (the host actually doing the
compiling), unlike BDEPEND, which already correctly checks BROOT. Forcing
minimal USE (`-*`) does **not** fix this — `perl`'s `minimal` IUSE flag
defaults off and nothing forces it on, `-*` only disables, it doesn't
enable. `--root-deps=rdeps` (forced on for this path, mirroring
`crossdev --setup`) only relaxes the DEPEND-only half of edges like glibc's,
not the RDEPEND half sharing the same `COMMON_DEPEND` — this is the exact
scenario the BROOT-satisfaction fix above now covers. Historical
"stage1 complete" claims never actually hit this because every prior run reused a
sysroot that had already accumulated `perl`/`portage`/etc. from earlier,
unrelated work in the same sysroot across many sessions — nobody had
bootstrapped a genuinely empty `--root` from absolute zero before.

**Known gap — `--config-root` resolution is not uniform across commands, and
the `--local` lifecycle silently depends on this.** Confirmed live
2026-07-11, in order:
- `setup.rs`'s `is_local`/overlay symlink split is *not* the bug
  `root-topology.md` used to claim here — that was fixed already (see
  `setup.rs`'s own "Previously gated on `is_local` — exactly backwards"
  comment). Don't re-diagnose it.
- `em setup --local DIR` writes the layout correctly (own `bashrc`,
  `make.conf` — commentary-only, no `MAKEOPTS`, matching `--prefix`), but
  writes **no `make.profile`** — unlike bare `--root`, which gets one
  auto-symlinked to the host's resolved profile as part of
  `ensure_self_contained_prefix`. This is deliberate for `--local` (it must
  also work on a non-Gentoo host, where auto-symlinking a Gentoo profile
  tree isn't possible) — the documented lifecycle just never says so
  explicitly, nor what to do about it.
- **FIXED (was: "does not accept `--local`/`--prefix`")** — `em select
  profile set <profile>` (and every other `select` module) resolves its
  config root through `config_portage_dir_for()`
  ([`select/mod.rs`](../../portage-cli/src/select/mod.rs)): explicit
  `--config-root` first, else the `--local`/`--prefix` overlay
  (`Roots::config_overlay()`), else the host's `/etc/portage`. Fixed in
  `7a8c5bc` (2026-06-23), predating the "confirmed live 2026-07-11" note this
  section used to carry — that note was already stale when written. Live
  re-verified 2026-08-09: `em select profile show --local DIR` resolves
  `DIR/etc/portage`, not the host's. A bare `--root DIR` still does **not**
  count (only `--config-root`, `--local`, `--prefix` do) — that part matches
  real `eselect`'s `profile.eselect` on purpose, see `select/mod.rs`'s doc
  comment on `config_portage_dir_for`.
- `em toolchain --local DIR --setup` reads the prefix's own profile once
  `select profile set` has pointed `make.profile` at it (via the same
  `--local`-aware resolution above); skip that step and it falls back to the
  host's real `/etc/portage`, which is what produced the much larger, more
  chaotic unresolved-dependency list observed when this was tried without it.

This was three different commands (`setup`, `select`, `toolchain`/`stages`)
resolving `--local`'s config root inconsistently; `select` is now aligned
with `--local`/`--prefix` like the other two. What's left is `setup` still
not writing a `make.profile` for `--local` (deliberate, see below) — so the
`select profile set` step is still a required manual step in the lifecycle,
just no longer one that needs its own `--config-root` override.

### `--local` and `--prefix` setup

These don't run `toolchain --setup` themselves — they assume the host (or, for
`--local` after bootstrap, the prefix) provides a compiler. The lifecycle:

```
# --prefix (overlay on a Gentoo host): host provides everything
em setup --prefix /opt/prefix          # layout + overlay config + host-python symlinks
em --prefix /opt/prefix <pkg>          # host compiler builds into P

# --local (standalone): seed host tools, then bootstrap the prefix toolchain
em setup --local                             # layout + own config, no python symlinks
em select profile set --local DIR <profile>  # required — see below
# Empty VDB ⇒ hard cycle unless package.provided seeds host tools
em toolchain --local --setup                 # build native toolchain INTO ~/.gentoo
em stages --local --stage1                   # packages.build using the prefix's own gcc
em --local <pkg>                             # now self-hosting
```

The `link_host_pythons`/`link_host_base_tools` `is_local` inversion this
section used to describe is **already fixed** in `setup.rs` (see its own
"Previously gated on `is_local` — exactly backwards" comment) — `--local`
correctly gets no host-python symlinks, `--prefix` correctly does. Don't
re-diagnose that; it's done.

What's still real: `em setup --local` writes layout + config but **no
`make.profile`** — deliberate, since `--local` must also work on a
non-Gentoo host where auto-symlinking a Gentoo profile isn't possible. The
`select profile set <profile>` step above is required to give it one; a
plain `em select profile set --local DIR <profile>` now targets the prefix
correctly (see the "Known gap" writeup above — fixed in `7a8c5bc`, no
`--config-root` override needed for the common `--local`/`--prefix` case,
only for targeting a foreign sysroot). Skipping the step entirely still
doesn't error at the `toolchain --setup` step: it silently falls back to
resolving against the host's real `/etc/portage`, producing a much larger,
more chaotic dependency set than intended (confirmed live 2026-07-11 — see
"Plain unprivileged toolchain" above for the full writeup).

### Cross setup (`em crossdev`)

Cross needs three things the native cases don't: a way to see
`cross-<tuple>/<pkg>` as buildable packages, the sysroot's `make.conf`
(pinning `CHOST`/`CBUILD`/`CTARGET`), and the two-stage gcc bootstrap. The
`cross-<tuple>` packages are now **derived on the fly** from `::gentoo` via a
`Location::Alias` repos.conf entry (no on-disk symlink overlay), written by
`write_alias_repo_conf` in
[`crossdev/mod.rs`](../../portage-cli/src/crossdev/mod.rs):

```
# Privileged: classic crossdev into /usr/<T>  (config writes to /etc/portage)
em crossdev --target <tuple> --init-target   # alias repos.conf + sysroot make.conf/profile
em crossdev --target <tuple> --setup         # binutils→headers→gcc1→libc→gcc2 (implies --init-target)
em stages --target <tuple> --stage1          # target packages.build
em --target <tuple> --emptytree @system      # stage3 (target-native @system)

# Unprivileged: same, under --prefix (config writes to <prefix>/etc/portage)
em crossdev --prefix <P> --target <tuple> --init-target
em crossdev --prefix <P> --target <tuple> --setup
em stages --prefix <P> --target <tuple> --stage1
...
```

`--init-target` writes the alias `repos.conf` entry (deriving
`cross-<tuple>/*` from `::gentoo`) and the sysroot
`etc/portage/{make.conf,make.profile}` via `write_sysroot_config` (the
`make.conf` that pins the triples and sets `PKG_CONFIG_*`/
`BUILD_PKG_CONFIG_LIBDIR`). The per-target
CTARGET/ABI-CFLAGS env is written by `write_cross_env` into the config
overlay (`<prefix>/etc/portage` under `--prefix`/`--local`, host
`/etc/portage` otherwise) — unprivileged under `--prefix`. `--setup` runs `BootstrapKind::Cross` (two-stage gcc) and implies
`--init-target`. `--target <tuple>` is a single global flag serving both
roles — setting a target up (`crossdev`) and using an already-set-up one
later (`stages`, plain `em <atom>`) — not two separate flags that could
disagree (crossdev used to have its own local `-t`; retired 2026-07-09).

### Lifecycle × topology map

| command | topology after | BROOT |
|---|---|---|
| `em setup --prefix P` | `Overlayed` | `/` (host) |
| `em setup --local` | `Single { ~/.gentoo }` | `/` → `~/.gentoo` (after toolchain) |
| `em toolchain --root R --setup` | `Dual { broot: /, target: R }` | `/` |
| `em toolchain --local --setup` | `Single { ~/.gentoo }` | `~/.gentoo` |
| `em crossdev --target T --init-target` | `Dual { broot: EROOT, target: EROOT/usr/T }` | EROOT |
| `em … --local --target T` | `Dual { broot: ~/.gentoo, target: ~/.gentoo/usr/T }` | `~/.gentoo` |

The BROOT column shows axis 2 in action: `--local`'s BROOT *moves* from `/`
(host seed) to `~/.gentoo` (self-hosting) over its lifecycle, without the
topology variant changing — confirming BROOT identity is a runtime property,
not a structural one.



The code calls both "stage1" but they compose
([`crossdev/stages.rs`](../../portage-cli/src/crossdev/stages.rs)):

1. **Toolchain stage1** (`toolchain_plan`, `BootstrapKind::Cross`/`Native`) —
   the chicken-and-egg bootstrap of the *compiler itself*: binutils → headers
   → libc-headers (`--nodeps`) → gcc-stage1 → libc → gcc-stage2. Cross needs
   the two-stage split; native (`CHOST==CBUILD`) builds full glibc+gcc in one
   pass because the seed compiler already targets that arch.
2. **`packages.build` stage1** (`stage1_plan`, catalyst's `stage1/chroot.sh`) —
   *assumes* a toolchain already exists in the root, then emerges the minimal
   bootable package *set* (baselayout with `USE=build` `--nodeps`, then
   `packages.build` with `USE="-* build"`).

`stage3` = full `@system` into the root. `stage4` = stage3 + a custom/`@world`
set — "the bootable/installable system."

## Satisfaction-root mapping (current code, not yet the variant)

Today the routing is encoded in *two vocabularies* that must agree:

- **Solver side** (`portage-atom-pubgrub/src/provider/solve.rs`):
  `cross_target_runtime_deps` / `host_native_deps` / `broot_filtered` stamp
  `MergeRoot::Target` / `MergeRoot::Host` per dep class. `host_aliases`
  (`provider/mod.rs:708`) maps a `Host`-flavored package to its `Target` data;
  `package_data()` is the alias-resolving accessor (a raw `packages.get()` is
  the bug behind `208c818`).
- **Post-solve side** (`portage-cli/src/preflight.rs`, `bdepend_avail.rs`):
  `Avail::initial_bdepend(host_roots)` / `initial_depend(roots)`, and
  `preflight::check`'s `match planned.merge_root` arms.

The two sides can't share code directly (one speaks `PortagePackage`/
`VersionData`, the other `Cpv`/VDB) — that's a real boundary, not gratuitous
duplication. The invariant they must both honour is the table above. The
variant refactor's payoff is that both sides ask
`topology.satisfaction_root(class)` instead of re-deriving it from positional
`&Roots` arguments, retiring the `host_roots`-threading smell.
(Commits c421c95/732aefe/0e9b3e0 fixed "wrong root at one site" bugs.)

## Status

- **Done** — `Roots` struct, three-root split, builder env threading
  (`run_phase` sets `PORTAGE_CONFIGROOT`/`ROOT`/`SYSROOT`/`BROOT`), per-class
  VDB checks, `MergeRoot` on solver nodes, Host-BDEPEND scheduling,
  `BUILD_PKG_CONFIG_LIBDIR` for cross. **`satisfaction_root(class)` — landed
  2026-07-09**, scoped down from a full `RootTopology`/`RootSet`-enum replacement to two
  new fields on the existing `Roots` (`broot`, `is_cross_arch`) plus a
  `satisfaction_root(DepClass)` method — same payoff (one `Roots` value
  answers `BDEPEND`/`DEPEND`/`RDEPEND`/`IDEPEND`/`PDEPEND` without a second
  `host_roots` parameter), far less churn (no type rename, no 9-file enum
  migration). Reuses `portage_atom_pubgrub::DepClass`, the solver's own
  existing PMS dependency-class enum, rather than a second one. `base_roots()`
  and `host_roots()` (the Cli method, formerly `broot()`) still exist — `merge/mod.rs`'s `entry_roots`
  genuinely needs a full `Roots` for a Host-routed entry (`config()`/
  `build_sysroot()`/`eprefix()`, to actually merge there), which
  `satisfaction_root`'s bare path can't provide.
  Also landed the same day: `--root`'s BROOT is the host (portage `ROOT=`
  parity, `Cli::host_roots()`'s original motivation), and `--cross`/`crossdev -t`
  unified into one `--target`/`-T` flag (crossdev's local `-t` retired).
  Landed shortly after, same session: `--prefix`'s unsatisfied BDEPEND now
  merges into the prefix (never the read-only host) and its satisfaction
  check weaves host ∪ prefix VDB — `Cli::host_roots()` returns `outer_roots()`
  for the overlay case instead of a host-anchored `Roots`.
  **`--root` config (current):** profile/make.conf stay at host `/` unless
  `--config-root` is set (portage `ROOT=` parity). `em setup --root R` still
  *writes* a make.conf under R for when the user pairs `--config-root R`.
  `em select` resolves via `config_portage_dir_for()`: explicit
  `--config-root`, else the `--local`/`--prefix` overlay, else host `/` — a
  bare `--root` alone still doesn't count (matching real `eselect`). Fixed
  in `7a8c5bc` (2026-06-23); see the "Known gap" writeup above for the
  `--local`/`select` history.
- **`BuildClass` — landed then dropped (2026-08).** Track A stamped a
  `BuildClass` on plan entries; re-review against bash-crossdev concluded it
  was dual authority next to **package.env** and was removed. Host-vs-target
  for cross packages is package.env + `host_codegen` PN specials (see
  [`bash-crossdev-matrix.md`](./bash-crossdev-matrix.md)).
  `bypass_cross_root` was renamed `use_outer_eroot`.
- **Removed (2026-08, was "Not pursued")** — the `RootSet` enum
  (`Single`/`Dual`/`Overlayed`): it was a lossy path-only summary whose
  `Single` collapsed `Local`+`Host` (which `base_roots` must distinguish), so
  it couldn't drive the full `Roots` construction. Its only read was
  `.broot()`, inlined at the two call sites. `satisfaction_root`'s
  `is_cross_arch: bool` covers its one cell that needs the cross distinction;
  a `CrossArch` triples type is still not needed there. The `Cli::broot()`
  method (returned a full `Roots`) was renamed `Cli::host_roots()` to drop the
  name clash with `Roots::broot()` (the path accessor).
- **Privatizing `provider.packages` behind `package_data()` — landed
  2026-07-09.** A different crate, a different invariant (`host_aliases`) from
  the `Roots`-accessor confusion this pass targeted, but the same underlying
  lesson (a raw lookup bypassing an alias-resolving accessor). Found 12 more
  instances of the bug class beyond the already-fixed `dependency_graph` one.
- **Deferred (out of scope here)** — Tier 3 mutable-BROOT bootstrap on a
  foreign host (`build-environment.md`), zero-config merged sysroot via
  `fuse-overlayfs` (M3).
- **`MergeRoot::Base` / `base_copies` — landed 2026-08-27
  (`ee8339c`/`48e0fb3`).** The board-root topology's missing half of the
  DEPEND row above: `satisfaction_root(DepClass::Depend)` already answered
  "base (sysroot)" for a *check*, but nothing scheduled an actual **merge**
  there — a single-rooted solve only ever produces the `RDEPEND`-satisfying
  target-root entry. Real Portage double-plans a `DEPEND` provider into the
  sysroot as a second merge-list entry (PMS table 8.2, confirmed against a
  real `crossdev`/`cross-emerge` control run); `portage_resolve::base_copies`
  is the post-solve closure walk that does the same, sibling to the
  pre-existing `host_copies` (BDEPEND→BROOT) — never solver-produced, always
  a post-solve stamp, for the same "would triple the pubgrub package
  universe" reason `host_copies` itself doesn't run inside the solver. Two
  parts, landed as two commits: plan generation (`base_copies` wired into
  `depgraph()`), then merge **execution** routing (`Cli::sysroot_roots()` +
  `MergeRun.base_roots`, `merge/mod.rs`'s `entry_roots()`) — the plan looked
  correct under `-p` after the first commit alone, but a `Base` entry
  silently routed to the board root and got skipped as "already installed"
  without the second. Live-verified: `sys-libs/readline`'s
  `ld: cannot find -lncursesw` is this bug start to finish.
- **`root_closure` — the two walks above, consolidated.** `base_copies` and
  `host_copies` were one algorithm written twice; they are now
  `portage_resolve::root_closure::base` / `::host` over a single graph
  (`target_order` entries as immovable anchors, closure nodes appended, one
  DFS post-order emission). The historical module names in the two entries
  above are what those commits landed, not what the tree holds today.
