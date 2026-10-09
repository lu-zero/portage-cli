# `cross-sysroot.toml` — a target spec file for cross sysroots

Status: 🟡 design agreed 2026-10-07 (Luca); step 1 done 2026-10-08 (the
built-in spec and `--print-spec`), step 2 mostly done 2026-10-09 (see
"Progress"), steps 3–5 open. Supersedes
the flag-based approach in [[crossdev-gcc-version-flag]], whose first
implementation (`--gcc`/`--libc`/… writing exact pins) was dropped as the
wrong shape.

## Idea

What `em crossdev` builds for a tuple is today a set of templates in code,
ported from bash crossdev: `CrossTarget::parse`, `profile_path`, `cflags`,
`packages`, the package-arch table, `make_conf_body`, the clang config
writer, `toolchain_plan`. Make that description data instead — a TOML spec,
the way rustc has JSON target specs:

- a tuple resolves to a **built-in spec**, produced by today's templates;
- a flag **prints** the built-in spec as TOML, to copy and edit;
- `em crossdev` accepts a **spec file** in place of a bare tuple;
- the spec in effect is **saved as `cross-sysroot.toml` in the sysroot**,
  and later operations (`em stages`, merges with `--target`) read it
  instead of re-deriving it.

One typed struct; the file is its serialisation. A caller such as
crossdev-stages builds the struct (from `board.conf`), a person edits the
TOML. This is the "toolchain spec" input of [[crossdev-stages-backend-api]]
R0/R2.

## Shape

Roles take **atoms** naming the real `::gentoo` package; `em` maps them to
the cross category. No version = no preference; a slot, range or glob is
the preference (`sys-devel/gcc:16`, `>=sys-libs/glibc-2.42`,
`=sys-devel/gcc-16.2*`). Where one atom cannot say it, a list, all of which
must hold.

Toolchains are named, and each thing says **which toolchain builds it**:

```toml
tuple = "riscv64-unknown-linux-gnu"
profile = "default/linux/riscv/23.0/rv64/lp64d"
cflags = "-O3 -march=rva23u64 -pipe"
extra = ["sys-devel/rust-std"]

[toolchain.gcc]
compiler = "sys-devel/gcc:16"
binutils = "sys-devel/binutils"

[toolchain.clang]
compiler = "llvm-core/clang:21"
linker = "llvm-core/lld"
wrappers = "sys-devel/clang-crossdev-wrappers"
rtlib = "llvm-runtimes/compiler-rt"
unwind = "llvm-runtimes/libunwind"
cxxabi = "llvm-runtimes/libcxxabi"
cxx = "llvm-runtimes/libcxx"

[libc]
atom = ">=sys-libs/glibc-2.42"
built-by = "gcc"

[kernel-headers]
atom = "sys-kernel/linux-headers"

[system]
built-by = "clang"
exceptions.gcc = ["sys-libs/glibc", "sys-devel/gcc", "dev-lang/perl"]
```

The three cases this has to cover:

| Case | Spec |
|------|------|
| GCC + glibc (today's default) | only `[toolchain.gcc]`; `libc` and `system` built by `gcc` |
| clang as main compiler on glibc | both toolchains; `libc.built-by = "gcc"`, `system.built-by = "clang"` |
| clang on musl | only `[toolchain.clang]`; `libc.atom = "sys-libs/musl"`; no gcc built |

Another libc is a different `libc.atom`. `kernel-headers` is absent for a
bare-metal target.

## What is derived, not written

- **Which toolchains to bootstrap:** every toolchain some `built-by` names.
  Replaces `-L/--llvm`.
- **The step order and per-step USE:** from who builds what (a libc built
  by gcc needs gcc-stage1 first; clang runtimes need libc headers first).
  Stays in code (`stages.rs`); rustc specs are declarative too.
- **Per-package compiler in the target:** `system.built-by` plus
  `exceptions` become `package.env` entries pointing at one env file per
  toolchain — the mechanism Gentoo users already use for this.
- **Versions:** each atom becomes ordinary `package.accept_keywords` /
  `package.mask` config, regenerated from the spec. A requirement `em`
  keeps satisfied, not an exact lock.
- **Multilib/ABI tables:** still queried from `multilib.eclass`.

Assumed unless decided otherwise: an atom applies to both the cross
package on the host and the same real package inside the target (the
target's own gcc matches the cross gcc); a spec file may describe a
variant of a built-in tuple.

## Open

- **Crate home.** crossdev-stages must construct the struct, so it wants a
  published library crate. `gentoo-core` holds arch/variant types but does
  not depend on `portage-atom`, and the spec's fields are `Dep`s. Starts in
  `portage-cli` (`crossdev/spec.rs`), written to depend only on
  `portage-atom` + serde so it can be lifted out. Not decided.
- **Spec file flag name**, and whether `--target` may take a path.
- **Where exactly the file is saved** under board-root topology
  (`--target T --root R`).
- **Native twin.** `em toolchain --setup` shares the staged bootstrap;
  whether it takes the same spec (minus the tuple) is unexplored.

## Steps

1. **Type + built-in + print** (behaviour-neutral). `SysrootSpec` with
   serde; `Dep` gains serde impls in `portage-atom` (as `Cpv`/`Cpn` have);
   the built-in spec for a tuple is derived from `CrossTarget`; a flag
   prints it. Tests: round-trip, and the built-in for gnu / musl / `-L`
   musl / bare metal.
2. **Consume the spec.** `init_target`, `cross_env_entries`,
   `sysroot_config_entries`, `toolchain_plan` read the spec instead of
   `CrossTarget` methods; generated config compared before/after.
3. **Load and save.** Accept a spec file; write `cross-sysroot.toml`; read
   it back in `em stages` and `--target` merges. `extra` becomes
   persistent (today `--ex-pkg` is per invocation).
4. **New knobs take effect.** Version atoms → keyword/mask config;
   `system.built-by` and `exceptions` → `package.env`; `profile` override;
   mixed gcc+clang bootstrap ordering.
5. Regression-test against a real board build in crossdev-stages.

## Progress (2026-10-09): step 2

`em crossdev` builds the built-in spec once in `run` and the config
generators read it: `init_target`, `setup`, `alias_repo_conf_entry`,
`alias_repo_entry`, `alias_packages_line`, `sysroot_repos_conf_entries`,
`sysroot_config_entries`, `make_conf_body`, `cross_env_entries`. They
take the toolchain packages from `SysrootSpec::packages()`, the extras
from `spec.extra`, and the profile and CFLAGS from the spec.

Behaviour-neutral, checked two ways:

- a test asserts the built-in spec lists the same packages in the same
  order as `CrossTarget::packages()` for gnu, musl, `-L` musl and bare
  metal;
- `--init-target --ex-pkg dev-debug/gdb` into a scratch prefix, binary
  before and after: every file written is byte-identical for
  `riscv64-unknown-linux-gnu`, `x86_64-unknown-linux-musl -L` and
  `arm-none-eabi`.

`toolchain_plan` reads the spec too (2026-10-09, after Luca's yes to
the native twin): which model, whether there are kernel headers, the
libc, the clang wrappers and runtimes and the LLVM slot come from it.
`em toolchain --setup` passes `SysrootSpec::native(CHOST)`, the GCC,
glibc and headers the native plan had hardcoded; its `profile` and
`cflags` are empty because the host's configuration applies. The plan
tests are unchanged and pass, and `--setup -p` prints the same steps
before and after for the three targets above and for the native
bootstrap.

Still reading `CrossTarget` instead of the spec:

- The GCC-model package names in `toolchain_plan` (`binutils`, `gcc`)
  are still literals; only the LLVM-model ones come from the spec.
- **Host-or-target env per package** (`cross_package_arch`): a table
  keyed by package name, not something the spec states. A spec naming a
  package outside the table gets the host environment, as `--ex-pkg`
  does.
- `show_target_cfg`, which only prints.
- Whether a package is LLVM-model (`target.llvm` in `env_mapping`,
  the keyword bound by `llvm_slot`).

## What rustc and Meson implement (studied 2026-10-09)

Sources: the rustc book's "Custom Targets" page; Meson's "Machine
files" and "Cross compilation" pages.

| | rustc | Meson |
|---|---|---|
| Flag | the existing `--target` | dedicated `--cross-file` / `--native-file` |
| Value | built-in name, else a path to a JSON file, else `NAME.json` found on `RUST_TARGET_PATH` | a path, or a bare name found in `./`, then `$XDG_DATA_HOME/meson/cross`, then `$XDG_DATA_DIRS/meson/cross` |
| Several files | no: one file is the whole target | yes: the flag repeats, a later file overrides an earlier one |
| Partial file | no | yes, that is what layering is for |
| Print the built-in | `--print target-spec-json`, nightly only | none; distributions ship files under `/usr/share/meson/cross` |
| Native twin | none: a target is a target | `--native-file`, "nearly identical" format |
| When read | every invocation | at the first setup only; later edits are ignored and the documented fix is to wipe the build tree |
| Stability | format unstable, pin the compiler | stable, documented |

What that suggests for `em`, as a proposal, not decided:

- **rustc's flag shape.** `--target` already exists on every command
  and already names the thing. `--target riscv64-unknown-linux-gnu`
  stays the built-in; `--target ./k3.toml` (anything with a `/` or
  ending in `.toml`) reads a file, whose `tuple` field says which
  target it is. No second flag to keep in step with the first.
- **Meson's partial files, without its layering.** The note already
  assumes a file may describe a variant of a built-in tuple. So a file
  needs `tuple` and whatever differs; the rest comes from the built-in
  for that tuple. One file, not a stack: nothing here needs a stack yet.
- **Neither tool's persistence.** rustc re-reads the file every time,
  so the file must stay put; Meson reads it once and then ignores it,
  which is its documented wart. `em` has a place neither has: the
  sysroot. `--init-target` writes the spec in effect to
  `<sysroot>/etc/portage/cross-sysroot.toml` (overwriting, as it does
  its other config); `--setup` and later `--target T` invocations read
  that copy; a bare tuple with no saved copy means the built-in. Under
  `--target T --root R` the copy stays in the cross sysroot the
  toolchain was built from; `R` records nothing.
- **A search path** (`RUST_TARGET_PATH`, XDG) is what lets a
  distribution or a tool like crossdev-stages ship named specs
  (`--target k3`). Useful, and separable: add it when there is a
  second spec to ship.
- **Native twin**: Meson's precedent, same format with a few fields
  that do not apply. Done here in the type (`SysrootSpec::native`);
  what a native spec *file* leaves out is open.

## Crate home, sized (2026-10-09)

Luca: maybe a `crossdev-core`, depending on how big the API is. Today:

- `crossdev/target.rs` (486 lines) and `crossdev/spec.rs` (about 370):
  23 public items between them. `CrossTarget` (parse a tuple, category,
  arch, profile path, CFLAGS, package table), `PackageArch`, and
  `SysrootSpec` with its seven part types, `builtin`, `native`,
  `packages`, `extras`, `llvm_slot`, `to_toml`.
- Dependencies: `gentoo-core` (`Arch`), `portage-atom` (`Dep`, `Cpn`),
  `serde`, `toml`, and `anyhow`, which a library crate would trade for
  an error type of its own.
- Nothing in either file touches `Cli`, the filesystem or a shell.

So the split is cheap and clean. The one judgement call is
`crossdev/stages.rs` (`toolchain_plan`, about 900 lines with tests): it
is also pure, and a backend that wants "the ordered steps for this
spec" wants it too, but it roughly triples the crate. Suggest: start
`crossdev-core` with target + spec, move the plan when crossdev-stages
actually asks for it.

## Decided 2026-10-09 (Luca)

- **Flag: the rustc way.** `--target` takes the tuple or a path to a
  spec file; no dedicated flag.
- **Spec versus profile is open, and to be thought through later.** The
  Gentoo way to describe a system is a profile, so a spec file overlaps
  with one; catalyst has the same pair (a spec file next to the profile
  it names). What belongs in which is not settled, and the persistence
  proposal above waits on it.
- **Crate home: not yet.** First plan what the shared surface has to
  hold, then pick the crate. Until then target and spec stay where they
  are, in `portage-cli`'s crossdev module.
