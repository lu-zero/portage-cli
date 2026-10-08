//! The cross-sysroot spec: what `em crossdev` builds for a target, as data
//!
//! A tuple resolves to a built-in spec ([`SysrootSpec::builtin`]); the TOML
//! form is what a user copies and edits. Roles name the real `::gentoo`
//! package as an atom, and each built thing names the toolchain building it.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use portage_atom::{Cpn, Dep};
use serde::{Deserialize, Serialize};

use super::target::CrossTarget;

/// Everything that decides what a cross sysroot is
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SysrootSpec {
    /// The `CTARGET` tuple, e.g. `riscv64-unknown-linux-gnu`
    pub tuple: String,
    /// Repo-relative profile the sysroot's `make.profile` links to
    pub profile: String,
    /// Target `CFLAGS` for the sysroot's `make.conf`
    pub cflags: String,
    /// Extra host-arch packages built onto the target (crossdev's `--ex-pkg`)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra: Vec<Dep>,
    /// The toolchains available to build with
    pub toolchain: Toolchains,
    /// The target C library
    pub libc: LibcSpec,
    /// Kernel headers; absent for a bare-metal target
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel_headers: Option<KernelHeadersSpec>,
    /// What builds the packages of the target system itself
    pub system: SystemSpec,
}

/// The toolchains a spec defines; only those some `built-by` names get built
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Toolchains {
    /// A per-target GCC and binutils
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gcc: Option<GccToolchain>,
    /// The host's clang cross-targeting, with runtimes built for the target
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clang: Option<ClangToolchain>,
}

/// The GCC toolchain's components
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GccToolchain {
    /// The compiler, e.g. `sys-devel/gcc:16`
    pub compiler: Dep,
    /// Assembler and linker
    pub binutils: Dep,
}

/// The LLVM/Clang toolchain's components
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClangToolchain {
    /// The compiler, e.g. `llvm-core/clang:21`
    pub compiler: Dep,
    /// The linker
    pub linker: Dep,
    /// The per-target `<tuple>-clang` wrappers
    pub wrappers: Dep,
    /// Compiler runtime (the libgcc replacement)
    pub rtlib: Dep,
    /// Stack unwinder
    pub unwind: Dep,
    /// C++ ABI library
    pub cxxabi: Dep,
    /// C++ standard library
    pub cxx: Dep,
}

/// Names one of the spec's [`Toolchains`]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolchainId {
    /// [`Toolchains::gcc`]
    Gcc,
    /// [`Toolchains::clang`]
    Clang,
}

/// The target C library and what compiles it
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LibcSpec {
    /// The libc package, e.g. `>=sys-libs/glibc-2.42` or `sys-libs/musl`
    pub atom: Dep,
    /// The toolchain that builds it
    pub built_by: ToolchainId,
}

/// The kernel headers the libc builds against
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KernelHeadersSpec {
    /// The headers package
    pub atom: Dep,
}

/// Which toolchain builds the target system's own packages
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SystemSpec {
    /// The default toolchain
    pub built_by: ToolchainId,
    /// Packages built by another toolchain than the default
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub exceptions: BTreeMap<ToolchainId, Vec<Dep>>,
}

impl SysrootSpec {
    /// The spec the in-code templates produce for `target` plus `extras`
    pub fn builtin(target: &CrossTarget, extras: &[Cpn]) -> Self {
        let atom = |cat: &str, pkg: &str| Dep::new(Cpn::new(cat, pkg));
        let (libc_cat, libc_pkg) = target.libc.package();
        let (toolchain, built_by) = if target.llvm {
            let mut compiler = atom("llvm-core", "clang");
            if let Some(slot) = target.llvm_slot {
                compiler.slot_dep = Some(portage_atom::SlotDep::Slot {
                    slot: Some(portage_atom::Slot::new(slot.to_string())),
                    op: None,
                });
            }
            let clang = ClangToolchain {
                compiler,
                linker: atom("llvm-core", "lld"),
                wrappers: atom("sys-devel", "clang-crossdev-wrappers"),
                rtlib: atom("llvm-runtimes", "compiler-rt"),
                unwind: atom("llvm-runtimes", "libunwind"),
                cxxabi: atom("llvm-runtimes", "libcxxabi"),
                cxx: atom("llvm-runtimes", "libcxx"),
            };
            let toolchains = Toolchains {
                clang: Some(clang),
                ..Toolchains::default()
            };
            (toolchains, ToolchainId::Clang)
        } else {
            let gcc = GccToolchain {
                compiler: atom("sys-devel", "gcc"),
                binutils: atom("sys-devel", "binutils"),
            };
            let toolchains = Toolchains {
                gcc: Some(gcc),
                ..Toolchains::default()
            };
            (toolchains, ToolchainId::Gcc)
        };
        Self {
            tuple: target.tuple.clone(),
            profile: target.profile_path(),
            cflags: target.cflags().to_owned(),
            extra: extras.iter().copied().map(Dep::new).collect(),
            toolchain,
            libc: LibcSpec {
                atom: atom(libc_cat, libc_pkg),
                built_by,
            },
            kernel_headers: target.has_kernel.then(|| KernelHeadersSpec {
                atom: atom("sys-kernel", "linux-headers"),
            }),
            system: SystemSpec {
                built_by,
                exceptions: BTreeMap::new(),
            },
        }
    }

    /// The spec as the TOML a user edits
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string(self).context("serializing the sysroot spec to TOML")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtin(tuple: &str, llvm: bool) -> SysrootSpec {
        let mut target = CrossTarget::parse(tuple, llvm).unwrap();
        if llvm {
            target.llvm_slot = Some(21);
        }
        SysrootSpec::builtin(&target, &[])
    }

    #[test]
    fn builtin_gnu_spec_prints_as_the_documented_toml() {
        let toml = builtin("riscv64-unknown-linux-gnu", false)
            .to_toml()
            .unwrap();
        assert_eq!(
            toml,
            r#"tuple = "riscv64-unknown-linux-gnu"
profile = "default/linux/riscv/23.0/rv64/lp64d"
cflags = "-O3 -march=rv64gc -pipe"

[toolchain.gcc]
compiler = "sys-devel/gcc"
binutils = "sys-devel/binutils"

[libc]
atom = "sys-libs/glibc"
built-by = "gcc"

[kernel-headers]
atom = "sys-kernel/linux-headers"

[system]
built-by = "gcc"
"#
        );
    }

    #[test]
    fn builtin_llvm_spec_has_no_gcc_and_binds_the_slot() {
        let spec = builtin("aarch64-unknown-linux-musl", true);
        assert_eq!(spec.toolchain.gcc, None);
        let clang = spec.toolchain.clang.as_ref().unwrap();
        assert_eq!(clang.compiler.to_string(), "llvm-core/clang:21");
        assert_eq!(spec.libc.atom.to_string(), "sys-libs/musl");
        assert_eq!(spec.libc.built_by, ToolchainId::Clang);
        assert_eq!(spec.system.built_by, ToolchainId::Clang);
    }

    #[test]
    fn builtin_bare_metal_spec_has_no_kernel_headers() {
        let spec = builtin("riscv64-unknown-elf", false);
        assert_eq!(spec.kernel_headers, None);
        assert_eq!(spec.libc.atom.to_string(), "sys-libs/newlib");
    }

    #[test]
    fn builtin_specs_round_trip_through_toml() {
        let extras = [Cpn::new("sys-devel", "rust-std")];
        let target = CrossTarget::parse("aarch64-unknown-linux-gnu", false).unwrap();
        let spec = SysrootSpec::builtin(&target, &extras);
        let back: SysrootSpec = toml::from_str(&spec.to_toml().unwrap()).unwrap();
        assert_eq!(back, spec);
        assert_eq!(back.extra[0].to_string(), "sys-devel/rust-std");
    }

    // The case the built-ins cannot produce yet: clang as the system compiler
    // on glibc, with gcc kept for what clang cannot build.
    #[test]
    fn a_mixed_toolchain_spec_parses() {
        let spec: SysrootSpec = toml::from_str(
            r#"
tuple = "riscv64-unknown-linux-gnu"
profile = "default/linux/riscv/23.0/rv64/lp64d/llvm"
cflags = "-O3 -pipe"

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
exceptions.gcc = ["sys-libs/glibc", "sys-devel/gcc"]
"#,
        )
        .unwrap();
        assert_eq!(spec.libc.built_by, ToolchainId::Gcc);
        assert_eq!(spec.system.built_by, ToolchainId::Clang);
        assert_eq!(spec.system.exceptions[&ToolchainId::Gcc].len(), 2);
        assert_eq!(
            spec.toolchain.gcc.unwrap().compiler.to_string(),
            "sys-devel/gcc:16"
        );
    }

    #[test]
    fn an_unknown_key_is_rejected() {
        let err = toml::from_str::<SysrootSpec>("tuple = \"t\"\ncompiler = \"gcc\"\n").unwrap_err();
        assert!(
            err.to_string().contains("unknown field `compiler`"),
            "{err}"
        );
    }
}
