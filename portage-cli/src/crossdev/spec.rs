//! The cross-sysroot spec: what `em crossdev` builds for a target, as data
//!
//! A tuple resolves to a built-in spec ([`SysrootSpec::builtin`]); the TOML
//! form is what a user copies and edits. Roles name the real `::gentoo`
//! package as an atom, and each built thing names the toolchain building it.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail, ensure};
use camino::Utf8Path;
use portage_atom::{Cpn, Dep};
use serde::{Deserialize, Serialize};

use super::target::CrossTarget;

/// Everything that decides what a cross sysroot is
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SysrootSpec {
    /// The `CTARGET` tuple, e.g. `riscv64-unknown-linux-gnu`
    pub tuple: String,
    /// Repo-relative `::gentoo` profiles the sysroot's profile inherits, in order
    ///
    /// One is written as a string, several as a list (e.g. the arch profile plus `features/llvm`).
    #[serde(with = "one_or_many")]
    pub profile: Vec<String>,
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
    /// USE flags per package atom, for what the toolchains need (e.g. `static-libs` on a runtime)
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        with = "package_use_table"
    )]
    pub package_use: Vec<(Dep, Vec<String>)>,
    /// Further `make.conf` variables for the sysroot (`FEATURES`, `ACCEPT_KEYWORDS`, …)
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub make_conf: BTreeMap<String, String>,
}

/// `make.conf` variables a spec sets through its own fields or the tuple
const RESERVED_MAKE_CONF: &[&str] = &["ARCH", "CBUILD", "CFLAGS", "CHOST", "ROOT"];

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
    /// The variables that select this toolchain for a package (`CC`, `CXX`, `LD`, …)
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
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
    /// The variables that select this toolchain for a package (`CC`, `CXX`, `LD`, …)
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
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
            let toolchains = Toolchains {
                clang: Some(ClangToolchain::builtin(&target.tuple)),
                ..Toolchains::default()
            };
            (toolchains, ToolchainId::Clang)
        } else {
            let toolchains = Toolchains {
                gcc: Some(GccToolchain::builtin(&target.tuple)),
                ..Toolchains::default()
            };
            (toolchains, ToolchainId::Gcc)
        };
        let mut spec = Self {
            tuple: target.tuple.clone(),
            profile: vec![target.profile_path()],
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
            package_use: Vec::new(),
            make_conf: BTreeMap::new(),
        };
        if let Some(slot) = target.llvm_slot {
            spec.bind_llvm_slot(slot);
        }
        spec
    }

    /// Bind the clang compiler to LLVM `slot`, unless the spec already names one
    pub fn bind_llvm_slot(&mut self, slot: u64) {
        if let Some(clang) = &mut self.toolchain.clang
            && clang.compiler.slot_dep.is_none()
        {
            clang.compiler.slot_dep = Some(portage_atom::SlotDep::Slot {
                slot: Some(portage_atom::Slot::new(slot.to_string())),
                op: None,
            });
        }
    }

    /// The spec of the native toolchain bootstrap for `chost`: GCC, glibc and
    /// kernel headers
    ///
    /// `profile` and `cflags` stay empty: the host's own configuration applies.
    pub fn native(chost: &str) -> Self {
        let atom = |cat: &str, pkg: &str| Dep::new(Cpn::new(cat, pkg));
        Self {
            tuple: chost.to_owned(),
            profile: Vec::new(),
            cflags: String::new(),
            extra: Vec::new(),
            toolchain: Toolchains {
                gcc: Some(GccToolchain {
                    compiler: atom("sys-devel", "gcc"),
                    binutils: atom("sys-devel", "binutils"),
                    env: BTreeMap::new(),
                }),
                ..Toolchains::default()
            },
            libc: LibcSpec {
                atom: atom("sys-libs", "glibc"),
                built_by: ToolchainId::Gcc,
            },
            kernel_headers: Some(KernelHeadersSpec {
                atom: atom("sys-kernel", "linux-headers"),
            }),
            system: SystemSpec {
                built_by: ToolchainId::Gcc,
                exceptions: BTreeMap::new(),
            },
            package_use: Vec::new(),
            make_conf: BTreeMap::new(),
        }
    }

    /// The LLVM slot the clang toolchain is bound to, if the spec names one
    ///
    /// LLVM slots are major versions; [`Self::load`] refuses a spec naming another kind.
    pub fn llvm_slot(&self) -> Option<u64> {
        self.llvm_slot_name()?.parse().ok()
    }

    fn llvm_slot_name(&self) -> Option<&str> {
        match self.toolchain.clang.as_ref()?.compiler.slot_dep.as_ref()? {
            portage_atom::SlotDep::Slot {
                slot: Some(slot), ..
            } => Some(slot.slot.as_str()),
            _ => None,
        }
    }

    /// The cross toolchain's own packages in bootstrap order, extras left out
    pub fn packages(&self) -> Vec<Cpn> {
        let Toolchains { gcc, clang } = &self.toolchain;
        let mut pkgs = Vec::new();
        pkgs.extend(gcc.as_ref().map(|t| t.binutils.cpn));
        pkgs.extend(clang.as_ref().map(|t| t.wrappers.cpn));
        pkgs.extend(self.kernel_headers.as_ref().map(|k| k.atom.cpn));
        pkgs.extend(gcc.as_ref().map(|t| t.compiler.cpn));
        pkgs.push(self.libc.atom.cpn);
        if let Some(t) = clang {
            pkgs.extend([t.rtlib.cpn, t.unwind.cpn, t.cxxabi.cpn, t.cxx.cpn]);
        }
        pkgs
    }

    /// The clang toolchain's packages, empty without one
    pub fn clang_roles(&self) -> Vec<Cpn> {
        self.toolchain
            .clang
            .as_ref()
            .map(ClangToolchain::cpns)
            .unwrap_or_default()
    }

    /// The packages built for the target besides its toolchain (`--ex-pkg`)
    pub fn extras(&self) -> Vec<Cpn> {
        self.extra.iter().map(|dep| dep.cpn).collect()
    }

    /// The spec as the TOML a user edits
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string(self).context("serializing the sysroot spec to TOML")
    }

    /// Read a spec file, refusing what `em crossdev` cannot build from it yet
    pub fn load(path: &Utf8Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
        let mut spec: Self = toml::from_str(&text).with_context(|| format!("parsing {path}"))?;
        // A toolchain written without `env` uses the built-in tools for the tuple.
        if let Some(t) = spec.toolchain.gcc.as_mut().filter(|t| t.env.is_empty()) {
            t.env = GccToolchain::builtin(&spec.tuple).env;
        }
        if let Some(t) = spec.toolchain.clang.as_mut().filter(|t| t.env.is_empty()) {
            t.env = ClangToolchain::builtin(&spec.tuple).env;
        }
        spec.check_buildable().with_context(|| format!("{path}"))?;
        Ok(spec)
    }

    /// The tuple's [`CrossTarget`], LLVM-model when clang is the only toolchain
    pub fn cross_target(&self) -> Result<CrossTarget> {
        CrossTarget::parse(&self.tuple, self.toolchain.gcc.is_none())
    }

    /// The toolchain `id` names, if the spec defines it
    pub fn env_of(&self, id: ToolchainId) -> Option<&BTreeMap<String, String>> {
        match id {
            ToolchainId::Gcc => self.toolchain.gcc.as_ref().map(|t| &t.env),
            ToolchainId::Clang => self.toolchain.clang.as_ref().map(|t| &t.env),
        }
    }

    /// The toolchain and extra packages the spec constrains to some versions; the clang compiler's slot
    /// is the LLVM binding instead (see [`Self::llvm_slot`])
    pub fn pins(&self) -> Vec<&Dep> {
        let Toolchains { gcc, clang } = &self.toolchain;
        let mut roles: Vec<&Dep> = vec![&self.libc.atom];
        roles.extend(self.kernel_headers.as_ref().map(|k| &k.atom));
        if let Some(t) = gcc {
            roles.extend([&t.compiler, &t.binutils]);
        }
        if let Some(t) = clang {
            roles.extend([
                &t.linker,
                &t.wrappers,
                &t.rtlib,
                &t.unwind,
                &t.cxxabi,
                &t.cxx,
            ]);
        }
        roles.extend(&self.extra);
        roles.retain(|dep| **dep != Dep::new(dep.cpn));
        roles
    }

    /// A spec asks for nothing the bootstrap does not honour: the toolchain packages the tuple
    /// implies, pinned only by version or slot, toolchains it defines, and gcc for the libc
    /// when it has both
    fn check_buildable(&self) -> Result<()> {
        let Toolchains { gcc, clang } = &self.toolchain;
        ensure!(gcc.is_some() || clang.is_some(), "a spec needs a toolchain");
        let named = std::iter::once(self.libc.built_by)
            .chain(std::iter::once(self.system.built_by))
            .chain(self.system.exceptions.keys().copied());
        for id in named {
            ensure!(
                self.env_of(id).is_some(),
                "{id:?} builds something but the spec defines no such toolchain"
            );
        }
        ensure!(
            gcc.is_none() || clang.is_none() || self.libc.built_by == ToolchainId::Gcc,
            "with both toolchains, gcc builds the libc"
        );
        let target = self.cross_target()?;
        let (libc_cat, libc_pkg) = target.libc.package();
        let headers = Cpn::new("sys-kernel", "linux-headers");
        ensure!(
            self.libc.atom.cpn == Cpn::new(libc_cat, libc_pkg),
            "{}: {} uses {libc_cat}/{libc_pkg}",
            self.libc.atom,
            self.tuple
        );
        ensure!(
            self.kernel_headers.as_ref().map(|k| k.atom.cpn)
                == target.has_kernel.then_some(headers),
            "{}: kernel headers must be {}",
            self.tuple,
            if target.has_kernel {
                "sys-kernel/linux-headers"
            } else {
                "absent"
            }
        );
        if let Some(t) = gcc {
            let expected = GccToolchain::builtin(&self.tuple);
            ensure!(
                t.cpns() == expected.cpns(),
                "the gcc toolchain packages must be {}",
                join(&expected.cpns())
            );
        }
        if let Some(t) = clang {
            let expected = ClangToolchain::builtin(&self.tuple);
            ensure!(
                t.cpns() == expected.cpns(),
                "the clang toolchain packages must be {}",
                join(&expected.cpns())
            );
            ensure!(
                Dep {
                    slot_dep: None,
                    ..t.compiler.clone()
                } == Dep::new(t.compiler.cpn),
                "{}: only a slot can constrain the clang compiler",
                t.compiler
            );
        }
        if let Some(dep) = self.pins().into_iter().find(|dep| !is_pin(dep)) {
            bail!("{dep}: a toolchain package takes only a version or a slot");
        }
        if let Some(slot) = self.llvm_slot_name() {
            ensure!(
                slot.parse::<u64>().is_ok(),
                "LLVM slot {slot} is not a major version"
            );
        }
        for (key, value) in &self.make_conf {
            let valid_name = key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            ensure!(valid_name, "make-conf: {key:?} is not a variable name");
            ensure!(
                !RESERVED_MAKE_CONF.contains(&key.as_str()),
                "make-conf: {key} is set by the spec's tuple or cflags"
            );
            ensure!(
                !value.contains(['"', '\n']),
                "make-conf: the value of {key} has a quote or a newline"
            );
        }
        Ok(())
    }
}

impl GccToolchain {
    /// GCC and binutils for `tuple`, selected by the tuple-prefixed tools
    pub fn builtin(tuple: &str) -> Self {
        // Every tool `features/llvm` sets, so a gcc-built package never inherits an LLVM one.
        let tools = [
            "CC=gcc",
            "CXX=g++",
            "CPP=cpp",
            "AS=as",
            "LD=ld",
            "AR=ar",
            "NM=nm",
            "RANLIB=ranlib",
            "STRIP=strip",
            "OBJCOPY=objcopy",
            "OBJDUMP=objdump",
            "READELF=readelf",
            "STRINGS=strings",
            "ADDR2LINE=addr2line",
        ];
        let env = tools
            .into_iter()
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, tool)| (k.to_owned(), format!("{tuple}-{tool}")))
            .chain([("LLVM".to_owned(), String::new())])
            .collect();
        Self {
            compiler: Dep::new(Cpn::new("sys-devel", "gcc")),
            binutils: Dep::new(Cpn::new("sys-devel", "binutils")),
            env,
        }
    }

    fn cpns(&self) -> Vec<Cpn> {
        vec![self.compiler.cpn, self.binutils.cpn]
    }
}

impl ClangToolchain {
    /// The host clang cross-targeting `tuple` through its wrappers, with LLVM's own tools
    pub fn builtin(tuple: &str) -> Self {
        let atom = |cat: &str, pkg: &str| Dep::new(Cpn::new(cat, pkg));
        let env = [
            ("CC", format!("{tuple}-clang")),
            ("CXX", format!("{tuple}-clang++")),
            ("CPP", format!("{tuple}-clang-cpp")),
            ("AS", format!("{tuple}-clang -c")),
            ("CROSS_COMPILE", format!("{tuple}-")),
        ]
        .into_iter()
        .chain(
            [
                ("LD", "ld.lld"),
                ("AR", "llvm-ar"),
                ("NM", "llvm-nm"),
                ("RANLIB", "llvm-ranlib"),
                ("STRIP", "llvm-strip"),
                ("OBJCOPY", "llvm-objcopy"),
                ("OBJDUMP", "llvm-objdump"),
                ("STRINGS", "llvm-strings"),
                ("ADDR2LINE", "llvm-addr2line"),
                ("READELF", "llvm-readelf"),
                ("DLLTOOL", "llvm-dlltool"),
                ("HOSTCC", "clang"),
                ("HOSTCXX", "clang++"),
                ("LLVM", "1"),
            ]
            .map(|(k, v)| (k, v.to_owned())),
        )
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
        Self {
            compiler: atom("llvm-core", "clang"),
            linker: atom("llvm-core", "lld"),
            wrappers: atom("sys-devel", "clang-crossdev-wrappers"),
            rtlib: atom("llvm-runtimes", "compiler-rt"),
            unwind: atom("llvm-runtimes", "libunwind"),
            cxxabi: atom("llvm-runtimes", "libcxxabi"),
            cxx: atom("llvm-runtimes", "libcxx"),
            env,
        }
    }

    fn cpns(&self) -> Vec<Cpn> {
        [
            &self.compiler,
            &self.linker,
            &self.wrappers,
            &self.rtlib,
            &self.unwind,
            &self.cxxabi,
            &self.cxx,
        ]
        .map(|dep| dep.cpn)
        .to_vec()
    }
}

fn join(cpns: &[Cpn]) -> String {
    cpns.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

/// A field that is one string or a list of them, written back as a string when it holds one
/// `package_use` as a TOML table keyed by atom, in the order written
mod package_use_table {
    use portage_atom::Dep;
    use serde::de::{MapAccess, Visitor};
    use serde::ser::SerializeMap;
    use serde::{Deserializer, Serializer};

    type Entries = Vec<(Dep, Vec<String>)>;

    pub fn serialize<S: Serializer>(entries: &Entries, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(entries.len()))?;
        for (dep, flags) in entries {
            map.serialize_entry(dep, flags)?;
        }
        map.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Entries, D::Error> {
        struct Table;
        impl<'de> Visitor<'de> for Table {
            type Value = Entries;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a table of atoms to USE flags")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Entries, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry()? {
                    entries.push(entry);
                }
                Ok(entries)
            }
        }
        d.deserialize_map(Table)
    }
}

mod one_or_many {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }

    pub fn serialize<S: Serializer>(values: &[String], s: S) -> Result<S::Ok, S::Error> {
        match values {
            [one] => one.serialize(s),
            many => many.serialize(s),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
        Ok(match OneOrMany::deserialize(d)? {
            OneOrMany::One(one) => vec![one],
            OneOrMany::Many(many) => many,
        })
    }
}

/// A version or slot constraint that a mask and an unmask can express: no blocker, USE
/// dependency, repository or slot operator
fn is_pin(dep: &Dep) -> bool {
    dep.blocker.is_none()
        && dep.use_deps.is_none()
        && dep.repo.is_none()
        && matches!(
            dep.slot_dep,
            None | Some(portage_atom::SlotDep::Slot {
                slot: Some(_),
                op: None
            })
        )
}

#[cfg(test)]
/// clang as the system compiler on glibc, gcc kept for the libc and what clang cannot build
pub(crate) fn clang_main_spec() -> SysrootSpec {
    let target = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
    let mut spec = SysrootSpec::builtin(&target, &[]);
    let mut clang = ClangToolchain::builtin(&spec.tuple);
    clang.compiler = Dep::parse("llvm-core/clang:23").unwrap();
    spec.toolchain.clang = Some(clang);
    spec.profile.push("features/llvm".into());
    spec.system.built_by = ToolchainId::Clang;
    spec.system.exceptions.insert(
        ToolchainId::Gcc,
        vec![Dep::parse("sys-libs/glibc").unwrap()],
    );
    spec.package_use.push((
        Dep::parse("llvm-runtimes/libunwind").unwrap(),
        vec!["static-libs".into()],
    ));
    spec
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

[toolchain.gcc.env]
ADDR2LINE = "riscv64-unknown-linux-gnu-addr2line"
AR = "riscv64-unknown-linux-gnu-ar"
AS = "riscv64-unknown-linux-gnu-as"
CC = "riscv64-unknown-linux-gnu-gcc"
CPP = "riscv64-unknown-linux-gnu-cpp"
CXX = "riscv64-unknown-linux-gnu-g++"
LD = "riscv64-unknown-linux-gnu-ld"
LLVM = ""
NM = "riscv64-unknown-linux-gnu-nm"
OBJCOPY = "riscv64-unknown-linux-gnu-objcopy"
OBJDUMP = "riscv64-unknown-linux-gnu-objdump"
RANLIB = "riscv64-unknown-linux-gnu-ranlib"
READELF = "riscv64-unknown-linux-gnu-readelf"
STRINGS = "riscv64-unknown-linux-gnu-strings"
STRIP = "riscv64-unknown-linux-gnu-strip"

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

    // The config generators read the spec; the built-in one must name what
    // the in-code table does, in its order.
    #[test]
    fn builtin_spec_lists_the_packages_of_its_target() {
        for (tuple, llvm) in [
            ("riscv64-unknown-linux-gnu", false),
            ("aarch64-unknown-linux-musl", false),
            ("aarch64-unknown-linux-musl", true),
            ("arm-none-eabi", false),
        ] {
            let target = CrossTarget::parse(tuple, llvm).unwrap();
            let table: Vec<Cpn> = target
                .packages()
                .into_iter()
                .map(|(cat, pkg, _)| Cpn::new(cat, pkg))
                .collect();
            assert_eq!(
                SysrootSpec::builtin(&target, &[]).packages(),
                table,
                "{tuple}"
            );
        }
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

    fn load_text(text: &str) -> Result<SysrootSpec> {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8Path::from_path(dir.path()).unwrap().join("spec.toml");
        std::fs::write(&path, text).unwrap();
        SysrootSpec::load(&path)
    }

    #[test]
    fn a_printed_builtin_spec_loads_back_unchanged() {
        for (tuple, llvm) in [
            ("riscv64-unknown-linux-gnu", false),
            ("x86_64-unknown-linux-musl", true),
            ("arm-none-eabi", false),
        ] {
            let spec = builtin(tuple, llvm);
            assert_eq!(
                load_text(&spec.to_toml().unwrap()).unwrap(),
                spec,
                "{tuple}"
            );
        }
    }

    #[test]
    fn a_spec_asking_for_what_the_bootstrap_ignores_is_refused() {
        let gnu = builtin("riscv64-unknown-linux-gnu", false)
            .to_toml()
            .unwrap();
        for (from, to, why) in [
            (
                "\"sys-devel/gcc\"",
                "\"sys-devel/gcc[cxx]\"",
                "takes only a version or a slot",
            ),
            (
                "\"sys-libs/glibc\"",
                "\"sys-libs/musl\"",
                "riscv64-unknown-linux-gnu uses sys-libs/glibc",
            ),
            (
                "[system]\n",
                "[system]\nexceptions.clang = [\"dev-lang/perl\"]\n",
                "defines no such toolchain",
            ),
            (
                "binutils = \"sys-devel/binutils\"",
                "binutils = \"sys-devel/elftoolchain\"",
                "the gcc toolchain packages must be",
            ),
        ] {
            assert!(gnu.contains(from), "{from}");
            let err = load_text(&gnu.replacen(from, to, 1)).unwrap_err();
            assert!(format!("{err:#}").contains(why), "{to}: {err:#}");
        }
    }

    #[test]
    fn a_clang_main_spec_loads_and_stays_in_the_gcc_category() {
        let spec = super::clang_main_spec();
        let text = spec.to_toml().unwrap();
        assert!(
            text.contains("profile = [\"default/linux/riscv/23.0/rv64/lp64d\", \"features/llvm\"]"),
            "{text}"
        );
        assert_eq!(load_text(&text).unwrap(), spec);
        let target = spec.cross_target().unwrap();
        assert!(!target.llvm);
        assert_eq!(target.category(), "cross-riscv64-unknown-linux-gnu");
        assert_eq!(spec.llvm_slot(), Some(23));

        let mut clang_libc = spec.clone();
        clang_libc.libc.built_by = ToolchainId::Clang;
        let err = load_text(&clang_libc.to_toml().unwrap()).unwrap_err();
        assert!(
            format!("{err:#}").contains("gcc builds the libc"),
            "{err:#}"
        );
    }

    #[test]
    fn a_toolchain_written_without_env_gets_the_builtin_tools() {
        let spec = builtin("riscv64-unknown-linux-gnu", false);
        let text = spec.to_toml().unwrap();
        let start = text.find("[toolchain.gcc.env]").unwrap();
        let end = text.find("[libc]").unwrap();
        let bare = format!("{}{}", &text[..start], &text[end..]);
        assert_eq!(load_text(&bare).unwrap(), spec);
    }

    #[test]
    fn each_builtin_toolchain_sets_every_tool_features_llvm_sets() {
        let gcc = GccToolchain::builtin("riscv64-unknown-linux-gnu").env;
        let clang = ClangToolchain::builtin("riscv64-unknown-linux-gnu").env;
        for var in [
            "CC",
            "CXX",
            "CPP",
            "AS",
            "LD",
            "AR",
            "NM",
            "RANLIB",
            "STRIP",
            "OBJCOPY",
            "OBJDUMP",
            "READELF",
            "STRINGS",
            "ADDR2LINE",
        ] {
            assert!(gcc.contains_key(var) && clang.contains_key(var), "{var}");
        }
    }

    #[test]
    fn a_version_or_slot_on_a_toolchain_package_is_a_pin() {
        let gnu = builtin("riscv64-unknown-linux-gnu", false)
            .to_toml()
            .unwrap();
        let pinned = gnu
            .replacen("\"sys-devel/gcc\"", "\"sys-devel/gcc:16\"", 1)
            .replacen("\"sys-libs/glibc\"", "\">=sys-libs/glibc-2.42\"", 1);
        let spec = load_text(&pinned).unwrap();
        let pins: Vec<String> = spec.pins().iter().map(ToString::to_string).collect();
        assert_eq!(pins, [">=sys-libs/glibc-2.42", "sys-devel/gcc:16"]);
        assert!(
            builtin("riscv64-unknown-linux-gnu", false)
                .pins()
                .is_empty()
        );
    }

    #[test]
    fn make_conf_round_trips_and_refuses_what_the_spec_owns() {
        let gnu = builtin("riscv64-unknown-linux-gnu", false)
            .to_toml()
            .unwrap();
        let with = |table: &str| format!("{gnu}\n[make-conf]\n{table}\n");
        let spec = load_text(&with("FEATURES = \"parallel-install\"")).unwrap();
        assert_eq!(spec.make_conf["FEATURES"], "parallel-install");
        assert_eq!(load_text(&spec.to_toml().unwrap()).unwrap(), spec);
        for (table, why) in [
            ("CFLAGS = \"-O2\"", "set by the spec"),
            ("\"A-B\" = \"x\"", "not a variable name"),
            ("X = 'a\"b'", "quote or a newline"),
        ] {
            let err = load_text(&with(table)).unwrap_err();
            assert!(format!("{err:#}").contains(why), "{table}: {err:#}");
        }
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
