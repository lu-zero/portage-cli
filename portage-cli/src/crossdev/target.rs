//! Parse a crossdev target tuple (`ARCH-VENDOR-OS-LIBC`) and derive everything
//! the no-build setup needs: the overlay category, the package set to symlink,
//! the Gentoo `ARCH`/keyword, the profile path, and the target `CFLAGS`.
//!
//! This mirrors crossdev's `parse_target` + the package-class table
//! (`/usr/bin/crossdev`, `BCAT/GCAT/KCAT/LCAT/...`) and crossdev-stages'
//! `gentoo_arch`/`gentoo_profile`/`target_cflags` (`lib/common.sh`), reduced to
//! the libc models em supports today: glibc (`gnu`), musl, and newlib
//! (bare-metal `-elf`/`-eabi`).

use anyhow::{Result, bail};
use gentoo_core::Arch;

/// The target C library, chosen from the tuple's last field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Libc {
    /// `…-linux-gnu` — `sys-libs/glibc`.
    Glibc,
    /// `…-linux-musl` — `sys-libs/musl`.
    Musl,
    /// `…-elf`/`-eabi`/`-newlib` — `sys-libs/newlib`, bare metal (no kernel).
    Newlib,
}

impl Libc {
    /// The real `category/package` providing this libc in `::gentoo`.
    fn package(self) -> (&'static str, &'static str) {
        match self {
            Libc::Glibc => ("sys-libs", "glibc"),
            Libc::Musl => ("sys-libs", "musl"),
            Libc::Newlib => ("sys-libs", "newlib"),
        }
    }
}

/// A parsed cross target plus the toolchain model (`--llvm`).
#[derive(Debug, Clone)]
pub struct CrossTarget {
    /// The full `CTARGET` tuple, e.g. `riscv64-unknown-linux-gnu`.
    pub tuple: String,
    /// The CPU field (`tuple` before the first `-`), e.g. `riscv64`.
    pub cpu: String,
    /// The target libc.
    pub libc: Libc,
    /// Whether the OS has a kernel (`linux`) — bare-metal targets do not, so they
    /// skip `sys-kernel/linux-headers`.
    pub has_kernel: bool,
    /// LLVM/Clang model (`cross_llvm-*`, no per-target compiler) vs GCC.
    pub llvm: bool,
}

impl CrossTarget {
    /// Parse `tuple` (`ARCH-VENDOR-OS-LIBC`); `llvm` selects the Clang model.
    pub fn parse(tuple: &str, llvm: bool) -> Result<Self> {
        let cpu = tuple
            .split('-')
            .next()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("empty target tuple"))?;

        // libc/OS from the tuple suffix (crossdev `parse_target`, abbreviated).
        let (libc, has_kernel) = if tuple.ends_with("gnu")
            || tuple.ends_with("gnueabi")
            || tuple.ends_with("gnueabihf")
        {
            (Libc::Glibc, true)
        } else if tuple.ends_with("musl") {
            (Libc::Musl, true)
        } else if tuple.ends_with("elf") || tuple.ends_with("eabi") || tuple.ends_with("newlib") {
            (Libc::Newlib, false)
        } else {
            bail!(
                "unsupported target '{tuple}': em crossdev handles gnu (glibc), \
                 musl, and bare-metal -elf/-eabi (newlib) tuples"
            );
        };

        // crossdev rejects glibc under LLVM ("cannot currently compile glibc").
        if llvm && libc == Libc::Glibc {
            bail!(
                "LLVM/Clang cannot build glibc — use a musl (…-linux-musl) or \
                 bare-metal (…-elf) target with -L, or drop -L for the GCC model"
            );
        }

        Ok(Self {
            tuple: tuple.to_owned(),
            cpu,
            libc,
            has_kernel,
            llvm,
        })
    }

    /// The overlay category for this target: `cross_llvm-<tuple>` (LLVM) or
    /// `cross-<tuple>` (GCC).
    pub fn category(&self) -> String {
        let prefix = if self.llvm { "cross_llvm-" } else { "cross-" };
        format!("{prefix}{}", self.tuple)
    }

    /// The Gentoo `ARCH`/keyword for the target CPU (e.g. `riscv64` → `riscv`).
    pub fn gentoo_arch(&self) -> String {
        Arch::from_chost(&self.tuple)
            .map(|a| a.as_keyword().to_owned())
            .unwrap_or_else(|| self.cpu.clone())
    }

    /// The repo-relative target profile path (`gentoo_profile` in
    /// crossdev-stages). Linked **directly** — `eselect profile` rejects a
    /// foreign arch.
    ///
    /// This deliberately uses the **arch-specific** profile, the crossdev-stages
    /// fix (`lib/sysroot.sh`): canonical `crossdev` hardcodes the arch-neutral
    /// `embedded` profile for every sysroot and then has to re-inject
    /// ARCH/ELIBC/KERNEL + the multilib ABI chain via a `profile/` shim — a
    /// shortcoming. The arch profile supplies all of that directly.
    pub fn profile_path(&self) -> String {
        // Bare-metal (newlib, no kernel) is the one case the arch fix can't
        // cover: there is no `default/linux/<arch>` profile, so fall back to the
        // arch-neutral `embedded` base (the `default/linux/*` profiles force
        // `kernel_linux` and assume a full OS the target does not have).
        if !self.has_kernel {
            return "embedded".to_owned();
        }
        match self.gentoo_arch().as_str() {
            "riscv" => "default/linux/riscv/23.0/rv64/lp64d".to_owned(),
            "x86" => "default/linux/x86/23.0/i686".to_owned(),
            arch => format!("default/linux/{arch}/23.0"),
        }
    }

    /// Target `CFLAGS` (`target_cflags` in crossdev-stages).
    pub fn cflags(&self) -> &'static str {
        match self.cpu.as_str() {
            "x86_64" => "-O3 -march=x86-64 -pipe",
            "aarch64" => "-O3 -pipe",
            "riscv64" => "-O3 -march=rv64gc -pipe",
            _ => "-O2 -pipe",
        }
    }

    /// The `(real_category, package)` set to symlink into the overlay category,
    /// in stage order. The cross magic lives in the eclasses, triggered by the
    /// `cross-*` category, so these point at the ordinary `::gentoo` ebuilds.
    ///
    /// Each entry's [`PackageArch`] comes from [`CROSS_PACKAGE_ARCH`], the one
    /// table for package.env / keywords — so adding a package here without
    /// declaring its arch there trips `planned_packages_are_all_declared`.
    ///
    /// `dev-debug/gdb` is deliberately NOT here: real crossdev only builds a
    /// cross gdb when `--ex-gdb` is explicitly passed, same as any other
    /// `--ex-pkg` — an opt-in extra, not part of the base
    /// binutils/headers/gcc/libc toolchain (`em`'s own `--ex-gdb`/`--ex-pkg`
    /// wire into it separately, see `crossdev::ex_pkg_atoms`). It was
    /// previously here unconditionally by mistake.
    pub fn packages(&self) -> Vec<(&'static str, &'static str, PackageArch)> {
        let mut pkgs: Vec<(&'static str, &'static str)> = Vec::new();
        if self.llvm {
            // Clang already cross-targets: no per-target compiler, just the
            // wrapper + the target runtimes built into the sysroot.
            pkgs.push(("sys-devel", "clang-crossdev-wrappers"));
            if self.has_kernel {
                pkgs.push(("sys-kernel", "linux-headers"));
            }
            pkgs.push(self.libc.package());
            pkgs.push(("llvm-runtimes", "compiler-rt"));
            pkgs.push(("llvm-runtimes", "libunwind"));
            pkgs.push(("llvm-runtimes", "libcxxabi"));
            pkgs.push(("llvm-runtimes", "libcxx"));
        } else {
            // GCC: the classic binutils → headers → gcc → libc toolchain.
            pkgs.push(("sys-devel", "binutils"));
            if self.has_kernel {
                pkgs.push(("sys-kernel", "linux-headers"));
            }
            pkgs.push(("sys-devel", "gcc"));
            pkgs.push(self.libc.package());
        }
        pkgs.into_iter()
            .map(|(cat, pkg)| {
                let arch = cross_package_arch(cat, pkg);
                debug_assert!(
                    arch.is_some(),
                    "{cat}/{pkg} is planned but absent from CROSS_PACKAGE_ARCH"
                );
                // Host is the same fall-through real crossdev gives an
                // undeclared package; `planned_packages_are_all_declared`
                // pins that this never fires for a package we do plan.
                (cat, pkg, arch.unwrap_or(PackageArch::Host))
            })
            .collect()
    }
}

/// Every `(real_category, package)` a `cross-<tuple>/` category can hold, with
/// the package.env arch from bash-crossdev's letter split (`set_env` `K|L` →
/// Target, `*` → Host). [`CrossTarget::packages`] and
/// [`cross_env_entries`](super::cross_env_entries) read this table — not a
/// BuildClass stamp (`todo/drop-buildclass.md`).
///
/// LLVM runtimes (R/U/A/P) are **host** env in bash-crossdev even though
/// ebuilds install under `/usr/${CTARGET}` — see
/// [`docs/design/bash-crossdev-matrix.md`](../../../docs/design/bash-crossdev-matrix.md).
///
/// Membership, not order, matters to the lookup.
const CROSS_PACKAGE_ARCH: &[(&str, &str, PackageArch)] = &[
    ("sys-devel", "binutils", PackageArch::Host),
    ("sys-devel", "gcc", PackageArch::Host),
    ("sys-devel", "clang-crossdev-wrappers", PackageArch::Host),
    ("sys-kernel", "linux-headers", PackageArch::Target),
    ("sys-libs", "glibc", PackageArch::Target),
    ("sys-libs", "musl", PackageArch::Target),
    ("sys-libs", "newlib", PackageArch::Target),
    ("llvm-runtimes", "compiler-rt", PackageArch::Host),
    ("llvm-runtimes", "libunwind", PackageArch::Host),
    ("llvm-runtimes", "libcxxabi", PackageArch::Host),
    ("llvm-runtimes", "libcxx", PackageArch::Host),
];

/// The arch a `cross-<tuple>/<pkg>` package builds for, looked up by the
/// **real** `category/package` it was cloned from (`RepoData::real_cpn_of`) —
/// the `cross-<tuple>` category itself says nothing about host-vs-target.
///
/// A caller that could not resolve the real category (no `real_cpn_of`
/// redirect) may pass the `cross-<tuple>` one: package names are unique across
/// the table, so the name alone still answers. Without that, an unresolved
/// redirect would silently read as "undeclared" and take the host branch —
/// exactly the misclassification this table exists to prevent.
///
/// `None` for anything outside the declared toolchain: an `--ex-pkg` extra,
/// which real crossdev's `set_env` gives the host branch (`case ${l} in K|L)`
/// target `;; *)` host — `l=X` falls through to host), as does
/// [`cross_env_entries`](super::cross_env_entries)' own extras loop.
pub fn cross_package_arch(real_category: &str, package: &str) -> Option<PackageArch> {
    let by_name = || {
        CROSS_PACKAGE_ARCH
            .iter()
            .find(|(_, pkg, _)| *pkg == package)
    };
    CROSS_PACKAGE_ARCH
        .iter()
        .find(|(cat, pkg, _)| *cat == real_category && *pkg == package)
        .or_else(by_name)
        .map(|(_, _, arch)| *arch)
}

/// Whether a cross package runs on the build host (`CBUILD`) or compiles code
/// for `<CTARGET>` (crossdev's `K|L`) — decides both which multilib env block
/// it gets ([`multilib::env_block`](super::multilib::env_block)) and whether
/// it needs a `**` `package.accept_keywords` entry (host tools must never be
/// keyword-checked against the target's arch).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageArch {
    /// Runs on the host — the toolchain itself (binutils/gcc/clang wrapper)
    /// and host-side tools like gdb.
    Host,
    /// Installs into the target sysroot, built for `<CTARGET>`.
    Target,
}

impl PackageArch {
    /// `true` for [`PackageArch::Target`] — the historical bool shape
    /// `multilib::env_block`'s third argument expects.
    pub fn is_target(self) -> bool {
        self == PackageArch::Target
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn riscv_gnu_is_glibc_with_kernel() {
        let t = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        assert_eq!(t.cpu, "riscv64");
        assert_eq!(t.libc, Libc::Glibc);
        assert!(t.has_kernel);
        assert_eq!(t.category(), "cross-riscv64-unknown-linux-gnu");
        assert_eq!(t.gentoo_arch(), "riscv");
        assert_eq!(t.profile_path(), "default/linux/riscv/23.0/rv64/lp64d");
        // binutils, linux-headers, gcc, glibc — no gdb (that's --ex-gdb, an
        // opt-in extra in real crossdev, not part of the base toolchain)
        assert!(
            t.packages()
                .contains(&("sys-libs", "glibc", PackageArch::Target))
        );
        assert!(
            t.packages()
                .contains(&("sys-kernel", "linux-headers", PackageArch::Target))
        );
        assert!(
            !t.packages()
                .contains(&("dev-debug", "gdb", PackageArch::Host))
        );
    }

    #[test]
    fn baremetal_elf_is_newlib_no_kernel() {
        let t = CrossTarget::parse("riscv64-unknown-elf", false).unwrap();
        assert_eq!(t.libc, Libc::Newlib);
        assert!(!t.has_kernel);
        assert!(
            !t.packages()
                .contains(&("sys-kernel", "linux-headers", PackageArch::Target))
        );
        assert!(
            t.packages()
                .contains(&("sys-libs", "newlib", PackageArch::Target))
        );
        // bare metal uses the arch-neutral embedded profile, not a linux one
        assert_eq!(t.profile_path(), "embedded");
    }

    #[test]
    fn llvm_uses_cross_llvm_category_and_runtimes() {
        let t = CrossTarget::parse("aarch64-unknown-linux-musl", true).unwrap();
        assert_eq!(t.category(), "cross_llvm-aarch64-unknown-linux-musl");
        assert!(t.packages().contains(&(
            "sys-devel",
            "clang-crossdev-wrappers",
            PackageArch::Host
        )));
        assert!(
            t.packages()
                .contains(&("llvm-runtimes", "compiler-rt", PackageArch::Host)),
            "llvm-runtimes are host-env (bash letter R), not K|L"
        );
        // no per-target gcc/binutils
        assert!(
            !t.packages()
                .contains(&("sys-devel", "gcc", PackageArch::Host))
        );
    }

    #[test]
    fn llvm_rejects_glibc() {
        let err = CrossTarget::parse("riscv64-unknown-linux-gnu", true).unwrap_err();
        assert!(err.to_string().contains("glibc"));
    }

    /// Every shape of target `em crossdev` can plan, so the `debug_assert` in
    /// `packages()` covers the whole space rather than whichever tuple a test
    /// happened to name.
    fn every_target() -> Vec<CrossTarget> {
        ["riscv64-unknown-linux-gnu", "riscv64-unknown-elf"]
            .iter()
            .filter_map(|t| CrossTarget::parse(t, false).ok())
            .chain(
                ["aarch64-unknown-linux-musl", "riscv64-unknown-elf"]
                    .iter()
                    .filter_map(|t| CrossTarget::parse(t, true).ok()),
            )
            .collect()
    }

    #[test]
    fn planned_packages_are_all_declared() {
        for t in every_target() {
            for (cat, pkg, _) in t.packages() {
                assert!(
                    cross_package_arch(cat, pkg).is_some(),
                    "{cat}/{pkg} is planned for {} but missing from CROSS_PACKAGE_ARCH",
                    t.tuple
                );
            }
        }
    }

    /// package.env letter fidelity (bash-crossdev matrix): K|L target, else host.
    #[test]
    fn packages_match_the_arch_table() {
        for t in every_target() {
            for (cat, pkg, arch) in t.packages() {
                assert_eq!(
                    cross_package_arch(cat, pkg),
                    Some(arch),
                    "{cat}/{pkg} disagrees between packages() and the table"
                );
            }
        }
    }

    /// Host codegen specials (PATH/ESYSROOT) are a narrow PN allowlist — not
    /// every host-env package (llvm runtimes are host-env, not host-codegen).
    #[test]
    fn host_codegen_is_only_code_generators() {
        use portage_repo::EbuildShell;
        for t in every_target() {
            let category = t.category();
            for (_cat, pkg, _arch) in t.packages() {
                let codegen = EbuildShell::is_cross_host_codegen(&category, pkg);
                let expect = matches!(pkg, "binutils" | "gcc" | "clang-crossdev-wrappers");
                assert_eq!(
                    codegen, expect,
                    "{category}/{pkg} host_codegen={codegen}, expected {expect}"
                );
            }
        }
        assert!(EbuildShell::is_cross_host_codegen(
            "cross-riscv64-unknown-linux-gnu",
            "gdb"
        ));
        assert!(!EbuildShell::is_cross_host_codegen(
            "cross-riscv64-unknown-elf",
            "newlib"
        ));
        assert!(!EbuildShell::is_cross_host_codegen(
            "cross_llvm-aarch64-unknown-linux-musl",
            "libcxx"
        ));
    }

    #[test]
    fn package_env_arch_matches_bash_crossdev_letters() {
        for pkg in ["newlib", "glibc", "musl"] {
            assert_eq!(
                cross_package_arch("sys-libs", pkg),
                Some(PackageArch::Target),
                "K|L target env"
            );
        }
        // R/U/A/P: host env in bash-crossdev (not K|L).
        for pkg in ["compiler-rt", "libunwind", "libcxxabi", "libcxx"] {
            assert_eq!(
                cross_package_arch("llvm-runtimes", pkg),
                Some(PackageArch::Host),
                "llvm runtime host env"
            );
        }
        for pkg in ["gcc", "binutils", "clang-crossdev-wrappers"] {
            assert_eq!(
                cross_package_arch("sys-devel", pkg),
                Some(PackageArch::Host)
            );
        }
        // An `--ex-pkg` extra is undeclared → host env (crossdev `*)`).
        assert_eq!(cross_package_arch("dev-debug", "gdb"), None);
    }

    /// The depgraph looks up the *real* cpn, but if a `real_cpn_of` redirect
    /// is ever missing it passes the `cross-<tuple>` category through. The
    /// name must still resolve, or the answer silently degrades to host.
    #[test]
    fn an_unresolved_redirect_still_finds_the_arch_by_name() {
        assert_eq!(
            cross_package_arch("cross-riscv64-unknown-elf", "newlib"),
            Some(PackageArch::Target)
        );
        assert_eq!(
            cross_package_arch("cross_llvm-aarch64-unknown-linux-musl", "libcxx"),
            Some(PackageArch::Host)
        );
        assert_eq!(
            cross_package_arch("cross-riscv64-unknown-linux-gnu", "gcc"),
            Some(PackageArch::Host)
        );
        // Package names are unique across the table, which is what makes the
        // name-only fallback unambiguous.
        let mut names: Vec<&str> = CROSS_PACKAGE_ARCH.iter().map(|(_, pkg, _)| *pkg).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "duplicate package name in the table");
    }
}
