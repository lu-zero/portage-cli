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

Still reading `CrossTarget` instead of the spec:

- **`toolchain_plan` in `crossdev/stages.rs`.** It goes through
  `BootstrapKind`, shared with the native `em toolchain --setup`, so
  this is the "native twin" question above and wants deciding first.
- **Host-or-target env per package** (`cross_package_arch`): a table
  keyed by package name, not something the spec states. A spec naming a
  package outside the table gets the host environment, as `--ex-pkg`
  does.
- `show_target_cfg`, which only prints.
- Whether a package is LLVM-model (`target.llvm` in `env_mapping`,
  the keyword bound by `llvm_slot`).
