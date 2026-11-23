# `em crossdev` — cross-compilation targets

`em crossdev` is `em`'s built-in workalike for Gentoo's `crossdev` tool: it
sets up a cross-compilation target (a `CTARGET` tuple like
`riscv64-unknown-linux-gnu`) so you can build a cross-toolchain and then
cross-build ordinary packages against it, all through `em` itself — no
separate `crossdev` binary, no on-disk symlink overlay.

This is the user-facing how-to. For the design rationale (why the category
is derived on the fly, how root/config resolution works across `--target`/
`--prefix`/`--local`, the `PackageArch` host/target split) see
[`root-topology.md`](../design/root-topology.md). For how **bash-crossdev** assigns
stage letters and `package.env` (and where `em` still diverges — especially
LLVM runtimes), see [`bash-crossdev-matrix.md`](../design/bash-crossdev-matrix.md).
For **EPREFIX / multi-root path assumptions** (baselayout, host-tool links,
why probes go wrong), see [`em-prefix-experiment.md`](../design/em-prefix-experiment.md).

## The model, briefly

- **`--target <tuple>` is one flag, two roles.** `em crossdev --target T
  --init-target` *sets up* tuple `T`; `em stages --target T --stage1` (or
  any plain atom build) *uses* the already-set-up `T`, resolving/installing
  into the target sysroot. There's no separate `-t` for crossdev.
- **`cross-<tuple>/<pkg>` packages are derived on the fly**, not symlinked on
  disk. `--init-target` writes a `Location::Alias` `repos.conf` entry that
  maps the `cross-<tuple>` category onto the real `::gentoo` ebuilds
  (`sys-devel/binutils`, `sys-devel/gcc`, …). The eclasses (`toolchain.eclass`
  etc.) do the actual cross-compilation magic, triggered by the category name
  — `em` doesn't reimplement cross-compilation, it just builds the aliased
  ebuild phases with the right env.
- **Two package classes live in the same category (host emerge, not
  cross-emerge).** `binutils`/`gcc`/`clang-crossdev-wrappers` (and any
  `--ex-pkg` extra) are **host-arch** tools: they run on the build machine
  and produce code for the target, so they install onto the host/prefix, not
  the sysroot. Their keyword acceptance mirrors what the *host* would already
  pick for the real package (installed version, or the newest available one
  bounded to its own release branch) — not a blanket accept-anything, which
  used to let a package's live `9999` ebuild win over a perfectly good dated
  snapshot. `linux-headers`/the libc are **target-ABI** in package.env
  (bash-crossdev letters K|L). LLVM runtimes install target code into the
  sysroot via the ebuild but are **host-env** in bash-crossdev (`*` letters
  R/U/A/P) — `em` still marks them target-env today; see the
  [matrix](../design/bash-crossdev-matrix.md).
- **The sysroot** is `<EROOT>/usr/<tuple>` — `/usr/<tuple>` for a bare/
  privileged setup, `<prefix>/usr/<tuple>` under `--prefix`/`--local`.
  `crossdev` does not take `--root`.

## Flags

```
em crossdev [OPTIONS]
```

| flag | what it does |
|---|---|
| `--target T` / `-T T` (global) | the tuple to set up or use, e.g. `riscv64-unknown-linux-gnu`, or a [spec file](#spec-files) |
| `--show-target-cfg` | print the derived config (category, sysroot, package set) and exit — no writes |
| `--print-spec` | print the target's built-in sysroot spec as TOML (toolchains, libc, profile, who builds what) and exit — no writes |
| `--init-target` | lay down the overlay alias + sysroot `make.conf`/`make.profile`, no building |
| `--setup` | bootstrap the full cross toolchain (binutils → headers → gcc-stage1 → libc → gcc-stage2) into the sysroot; implies `--init-target` |
| `-L` / `--llvm` | use the LLVM/Clang model (`cross_llvm-<tuple>`: host clang cross-targets directly, no per-target compiler build) instead of GCC |
| `--ex-pkg CATEGORY/PN` | build an extra package onto the target (repeatable); see [Extra packages](#extra-packages-ex-pkg---ex-gdb) below |
| `--ex-gdb` | shorthand for `--ex-pkg dev-debug/gdb` |

Plus `-p`/`--pretend` (preview, write nothing), `-a`/`--ask` (preview and
confirm before writing), `--prefix DIR`/`--local` (where the target lands —
see `root-topology.md`). `--root` is a parse error on `crossdev`.

### Supported tuples

The tuple's suffix picks the libc/OS model (crossdev's own `parse_target`):

| suffix | libc | kernel headers | example |
|---|---|---|---|
| `...gnu`, `...gnueabi`, `...gnueabihf` | glibc | yes | `riscv64-unknown-linux-gnu` |
| `...musl` | musl | yes | `aarch64-unknown-linux-musl` |
| `...elf`, `...eabi`, `...newlib` | newlib (bare metal) | no | `riscv64-unknown-elf` |

`-L`/`--llvm` rejects glibc targets outright (matching real crossdev — LLVM
can't currently build glibc); use a musl or bare-metal tuple with `-L`.

## Spec files

`--print-spec` prints what a tuple builds as TOML. Edit a copy and pass its path as `--target` (any value
containing `/` or ending in `.toml` is read as a file):

```
$ em crossdev --target riscv64-unknown-linux-gnu --print-spec > k3.toml
$ $EDITOR k3.toml          # e.g. cflags = "-O3 -march=rva23u64 -pipe"
$ em crossdev --target ./k3.toml --init-target
```

With a spec file, `--init-target` gives the sysroot its own profile: an overlay at
`<sysroot>/var/db/repos/cross-sysroot`, listed in the sysroot's `repos.conf`, whose
`profiles/sysroot` inherits the spec's `profile` from `::gentoo` (`gentoo:<profile>`, the
`portage-2` profile format) and holds the spec as `cross-sysroot.toml`. `make.profile` links
there, so later commands given just the tuple read that saved spec. A built-in tuple links
`make.profile` to the `::gentoo` profile directly, as before.

A version or slot on a toolchain package is a pin, e.g. `compiler = "sys-devel/gcc:15"`: the
sysroot profile masks the package and unmasks the pin, so the target's own packages stay inside
it, and the host's `package.mask`/`package.unmask`/`package.accept_keywords` for the cross
category do the same for the cross toolchain, up to the newest keyworded release the pin allows
(never a live ebuild). The clang compiler's slot is the LLVM slot instead.

A spec may define both toolchains: `[system] built-by = "clang"` with `exceptions.gcc = [...]` makes
clang the target's compiler and keeps gcc for the listed packages, and `--setup` builds the gcc
toolchain and the libc first, then the clang wrappers and LLVM's runtimes. `profile` may list several
parents (e.g. the arch profile plus `features/llvm`), `[package-use]` gives packages the flags the
toolchain needs, and `[toolchain.<name>.env]` overrides the variables that select a toolchain.

A spec is refused when read if it asks for other toolchain packages than the tuple's, has gcc and
clang but not gcc building the libc, names an undefined toolchain, or pins with a blocker, USE
dependency, repository or slot operator.

How each part of a spec maps to Portage files, and what is still planned:
[cross-sysroot-spec.md](../design/cross-sysroot-spec.md).

## Examples

### Preview a target before touching anything

```
$ em crossdev --target riscv64-unknown-linux-gnu --show-target-cfg
  Target    riscv64-unknown-linux-gnu
  Model     GCC
  Category  cross-riscv64-unknown-linux-gnu
  ARCH      riscv
  Profile   default/linux/riscv/23.0/rv64/lp64d
  Sysroot   /usr/riscv64-unknown-linux-gnu
  CFLAGS    -O3 -march=rv64gc -pipe
  Packages
    cross-riscv64-unknown-linux-gnu/binutils → sys-devel/binutils
    cross-riscv64-unknown-linux-gnu/linux-headers → sys-kernel/linux-headers
    cross-riscv64-unknown-linux-gnu/gcc → sys-devel/gcc
    cross-riscv64-unknown-linux-gnu/glibc → sys-libs/glibc
```

No writes happen — this is purely informational, and works before
`--init-target` has ever been run.

### Set up a target (privileged, classic crossdev layout)

```
em crossdev --target riscv64-unknown-linux-gnu --init-target
```

Writes the alias `repos.conf` entry and the sysroot's own
`etc/portage/{make.conf,make.profile}` under `/usr/riscv64-unknown-linux-gnu`.
Nothing is built yet.

### Bootstrap the full cross-toolchain

```
em crossdev --target riscv64-unknown-linux-gnu --setup
```

Implies `--init-target`, then runs the staged bootstrap (binutils → headers
→ gcc-stage1 → libc → gcc-stage2) through the normal merge path. Preview
first with `-p`:

```
em crossdev -p --target riscv64-unknown-linux-gnu --setup
```

### Unprivileged, under `--prefix`

Everything above works identically under `--prefix`/`--local` — the sysroot
and config just move under the prefix instead of the real host `/`:

```
em crossdev --prefix /opt/xp --target riscv64-unknown-linux-gnu --init-target
em crossdev --prefix /opt/xp --target riscv64-unknown-linux-gnu --setup
```

Sysroot lands at `/opt/xp/usr/riscv64-unknown-linux-gnu`; the alias/env
config lands in `/opt/xp/etc/portage`.

### Preview and confirm config changes

`--init-target` (and `--setup`'s own config-laydown step) diffs what it's
about to write against what's already there, so it's safe to re-run and
plays along with `-p`/`-a`:

```
$ em crossdev -p --prefix /opt/xp --target riscv64-unknown-linux-gnu --init-target
>>> config changes:
  update /opt/xp/etc/portage/repos.conf/crossdev.conf

$ em crossdev -a --prefix /opt/xp --target riscv64-unknown-linux-gnu --init-target
>>> config changes:
  update /opt/xp/etc/portage/repos.conf/crossdev.conf

>>> Would you like to write these 1 config file(s)? [y/N] y
>>> cross target riscv64-unknown-linux-gnu ready
    ...
```

A re-run with nothing to change prints no config-changes noise at all — just
the normal "ready" summary.

### Build the target's package set

Once the toolchain exists, use the same `--target` flag with the ordinary
`em` commands — no separate entry point:

```
em stages --target riscv64-unknown-linux-gnu --stage1     # packages.build (@system seed)
em --target riscv64-unknown-linux-gnu --emptytree @system # full target-native @system
em --target riscv64-unknown-linux-gnu sys-apps/coreutils  # one target package
```

These resolve and install into the target sysroot (`<EROOT>/usr/<tuple>`),
using the sysroot's own `make.conf` for `CHOST`/`CFLAGS`/keywords.

### After setup: what is (and isn’t) required

A full **`@system` / stage1 is not inherently required** just to *cross-build*
packages into a sysroot. Runtime (RDEPEND) closure for a complete target
userland is a different goal from “this emerge can compile and link.”

What *is* required for ordinary target packages is that **library DEPEND**
(headers / `.so` you build against under `SYSROOT`/`ESYSROOT`) is
**solver-visible** under the **real Cpns** ebuilds name — profile-shaped
libc (`sys-libs/glibc` *or* `sys-libs/musl`, …), headers, zlib, etc.

`crossdev --setup` already puts those **files** on disk, but registers them
as **`cross-<tuple>/…`**. Ordinary ebuilds Depend on **`sys-libs/glibc`** (or
musl), not `cross-T/glibc`. Until Favor/provided/alias bridges that identity,
the plan re-pulls a second libc (and friends) — the libcrypt / ballooned
clang-plan class of failures (2026-08-07). That is a **library-identity**
problem, not “missing full @system.”

| Need | Satisfied by |
|------|----------------|
| Compiler + link against sysroot | Files under sysroot + correct **DEPEND** Favor/provided (libc/headers/libs) |
| Full target runtime / chroot / stage | stage1 / `@system` / stage3 (optional for pure cross-build) |
| Host build tools (BDEPEND) | BROOT / dual-root (host or prefix) |

**Ways to fix library DEPEND** (product choice, not all required together):

1. **Identity bridge** — installed `cross-T/foo` Favor-satisfies `real_cpn(foo)`, and/or `package.provided` for real libc/headers after setup.  
2. **Register toolchain under real Cpns** in the sysroot VDB.  
3. **stage1 / manual baselayout+libc** — works, but heavier than needed if the only gap is Cpn identity for libs already on disk.

### Worked example: bootstrap a toolchain and cross-build a real package, under `--prefix`

End-to-end recipe, unprivileged (`riscv64-unknown-linux-gnu`, GCC model):

```
# 1. Cross toolchain (binutils → headers → gcc-stage1 → libc → gcc-stage2).
#    Files under sysroot; VDB identities are cross-<tuple>/… today.
em crossdev --prefix /opt/xp --target riscv64-unknown-linux-gnu --setup --jobs 8

# 2. Ordinary target package (real target-arch atoms into the sysroot).
#    Needs library DEPEND satisfaction (see above) — not a full @system.
#    Optional: stages --stage1 if you want packages.build / a richer base.
em --prefix /opt/xp --target riscv64-unknown-linux-gnu -b llvm-core/clang --jobs 8
```

Step 2 is different from step 1’s `cross-<tuple>/*` packages (see "Two package
classes"): real Cpns, sysroot `CHOST`/`CFLAGS`. Host vs target search paths:
`setup/mod.rs` `BASHRC_PREFIX`, `shell.rs` `ESYSROOT`.

### Build one cross-category package directly (no `--target` needed)

A `cross-<tuple>/<pkg>` atom fully identifies its own target through its
category name, so you don't need `--target` at all to build one directly —
just name it like any other atom:

```
em --prefix /opt/xp -p cross-riscv64-unknown-linux-gnu/gcc
```

Combining this with `--target` at the same time doesn't do what it looks
like it does: `--target` sets up a *separate* dual-root session for
resolving ordinary (non-cross-category) packages against the target sysroot
— a different concern from naming a cross-category atom directly. Don't mix
them for this use case.

### Extra packages (`--ex-pkg`, `--ex-gdb`)

The base toolchain set (binutils/headers/gcc/libc) is fixed, but you can add
more host-arch tools onto an established target — crossdev's own "Extra
Fun". These are **per-invocation**, not persisted: a later `--init-target`
that omits the flag drops the extra again, exactly like real crossdev.

`--init-target` records the extra and exits; `--setup` builds what is recorded,
after the toolchain. `--setup --ex-pkg X` also works on a target not initialized
yet. On an initialized target it refuses an extra not recorded there: run
`--init-target --ex-pkg X` first. Without a version, an extra resolves to the
host's installed version or the newest keyworded release; a spec's `extra` may
bound it, e.g. `"<sys-devel/clang-crossdev-wrappers-24"`.

```
# Build a cross-gdb (dev-debug/gdb) alongside the toolchain:
em crossdev --target riscv64-unknown-linux-gnu --init-target --ex-gdb
em crossdev --target riscv64-unknown-linux-gnu --setup

# The Rust standard library for this target (sys-devel/rust-std — its own
# ::gentoo DESCRIPTION literally says "standalone (for crossdev)"), needed
# to cross-compile Rust code targeting the tuple:
em crossdev --target riscv64-unknown-linux-gnu --init-target --ex-pkg sys-devel/rust-std
em crossdev --target riscv64-unknown-linux-gnu --setup

# Multiple extras, repeatable:
em crossdev --target riscv64-unknown-linux-gnu --init-target --ex-gdb --ex-pkg sys-devel/rust-std
em crossdev --target riscv64-unknown-linux-gnu --setup
```

Pick a genuine standalone host-arch tool for `--ex-pkg` — something that
isn't already an ordinary transitive dependency of anything in the
toolchain's own build closure (`sys-devel/rust-std`, like `dev-debug/gdb`,
is built purpose-made for this — real crossdev's own `--ex-gdb` precedent).
A widely-depended-on package (e.g. `dev-vcs/git`) is a poor choice: it's
liable to already be pulled in transitively by something else in the
closure (e.g. a doc-build BDEPEND), which only creates confusion about what
`--ex-pkg` actually added.

After that, the extra resolves like any other cross-category package — no
`--target` needed:

```
em --prefix /opt/xp -p cross-riscv64-unknown-linux-gnu/gdb
```

`--ex-pkg` atoms must be `CATEGORY/PN` (validated as a real `Cpn`, and
checked to actually exist in `::gentoo` before the alias is written) — a
malformed or nonexistent atom is rejected up front with a clear error rather
than surfacing later as an opaque resolver failure.

### LLVM/Clang model

```
em crossdev --target aarch64-unknown-linux-musl -L --show-target-cfg
em crossdev --target aarch64-unknown-linux-musl -L --setup
```

No per-target compiler build — clang already cross-targets. The category is
`cross_llvm-<tuple>` instead of `cross-<tuple>`, and the package set is the
clang wrapper + LLVM runtimes (`compiler-rt`/`libunwind`/`libcxxabi`/
`libcxx`) instead of the GCC toolchain stages.

## Gaps vs crossdev

`em crossdev` covers config setup, the full toolchain bootstrap and the LLVM
model. The real tool's wider option surface is not implemented:

**Missing options:**

- Stage selection `-s0..-s4` and `--ex-only`: em has only config-only
  `--init-target` or the full `--setup` sequence.
- `-C/--clean` to uninstall a target's config, overlay, wrappers and sysroot.
- Per-component version selectors (`--b/--g/--k/--l`, LLVM equivalents) with
  depend atoms, and `-S/--stable`. A [spec file](#spec-files) can pin exact
  versions, but there is no CLI range or stable selector.
- Per-component `--[bdgkl]cat/pkg/env` overrides and `--ex-gcc` extra
  frontends.
- `-A/--abis`, `--host-abi`, `-f/--force`, `-P/--portage` and
  `--show-fail-log`. Note that `-f` means `--fetchonly` in em, so real
  crossdev command lines do not carry over.
- Overlay selection flags. em discovers repositories and writes a virtual
  config entry instead; see the
  [fidelity matrix](../design/bash-crossdev-matrix.md).

**Different behaviour:**

- The letters `b`, `g`, `k`, `r`, `u`, `a`, `p` and `t` mean different things
  in em because they are shared with emerge. Real crossdev lines are not
  portable.
- `--show-target-cfg` prints a human table, not shell-evaluable `key=value`.
- Supported [tuples](#supported-tuples) are narrower: `gnu*`, `musl` and
  bare-metal `-elf`/`-eabi`/`-newlib` targets.
- em regenerates config on `--init-target`/`--setup` rather than restoring
  `/etc/portage/crossdev/<tuple>` settings, and installs no per-target
  wrappers. Use `em --target T …` instead.

Reference: [`crossdev`](./cli/crossdev.md).

## Gotchas

- **`--local` needs a native seed before crossdev.** A fresh `--local` prefix
  has an empty VDB; `toolchain --setup` / `crossdev --setup` hit a genuine
  hard cycle unless `package.provided` declares host-supplied tools. Prefer
  `--prefix` on a Gentoo host when you only need an overlay; see
  [`stages-and-testing.md`](./stages-and-testing.md).
- **`--target` alone doesn't imply a config has been set up.** Run
  `--init-target` (or `--setup`, which implies it) at least once before
  using `--target T` for ordinary builds — otherwise the sysroot has no
  `make.conf`/`repos.conf` and nothing resolves.
- **Extras aren't sticky.** If you want `dev-debug/gdb` to keep resolving,
  pass `--ex-gdb` every time you re-run `--init-target`/`--setup` — it's not
  remembered from a previous run, on purpose (matches real crossdev).
- **`cross-<CTARGET>/gcc` and `sys-devel/gcc` are different packages that can
  drift.** The former is the host-side cross-compiler built once by
  `--setup`; the latter is whatever ordinary compiler version
  `stages --stage1`/plain merges install *into* the target sysroot. Upgrading
  one doesn't upgrade the other.
- **Don't hand-edit the generated config.** The sysroot `make.conf`, the
  per-package `env/<category>/<pkg>.conf` files, `package.env`, and
  `package.accept_keywords` are entirely `em`-owned and unconditionally
  regenerated on every `--init-target`/`--setup` re-run — any hand edit is
  silently discarded (this matches real crossdev, which does the same for
  its equivalent files). The `[crossdev]` alias `repos.conf` entry behaves
  the same way once it's recognisable as `em`'s own. The one thing that *is*
  left alone is a **foreign** `[crossdev]` entry — one with no
  `alias-target =` key, e.g. a real crossdev/eselect-managed physical
  overlay — `em` never touches that.
