//! Staged ordered-build plans — the stage1/toolchain-bootstrap problem.
//!
//! A [`StagePlan`] is a curated, *ordered* list of builds that the dependency
//! solver cannot produce on its own, because the steps break a bootstrap
//! chicken-and-egg cycle (a compiler needs a libc; a libc needs a compiler).
//! Two flavours ([`BootstrapKind`]) break that cycle differently:
//!
//! - **cross** — the toolchain bootstrap into the crossdev prefix
//!   (`/usr/<chost>`), atoms under the `cross-<tuple>` overlay. There is no
//!   compiler for `CTARGET` yet, so it needs the classic two-stage bootstrap:
//!   binutils → headers → libc-headers (`--nodeps`) → gcc-stage1 → libc →
//!   gcc-stage2.
//! - **native** — a self-hosting stage1 into `--root` (`CHOST == CBUILD`), plain
//!   `::gentoo` atoms. The seed compiler at `BROOT=/` already targets this arch,
//!   so it builds *full* glibc directly and a single full gcc links against it:
//!   baselayout → binutils → os-headers → glibc → gcc. The two-stage split is
//!   cross-only —
//!   `toolchain.eclass` gates every stage1 affordance on `is_crosscompile`, so a
//!   native gcc is always `--enable-shared` and *requires* a full in-ROOT libc.
//!
//! Each step is one `em`-equivalent merge with a per-step USE override and the
//! `--nodeps` / `headers-only` bootstrap flags crossdev uses (`/usr/bin/crossdev`
//! `doemerge` loop). em owns only the ordering + USE/flags here; the
//! stage1-vs-stage2 gcc *behaviour* (cross) is auto-detected by
//! `toolchain.eclass` from whether the libc is present in the prefix yet.

use anyhow::Result;
use portage_repo::ProfileStack;

use super::target::{CrossTarget, Libc};

/// gcc USE forced off for **every** cross gcc build (crossdev `GUSE_DISABLE`).
const GCC_DISABLE: &[&str] = &["-objc", "-objc++", "-objc-gc", "-vtv"];
/// Additional gcc USE forced off for **stage1** — a freestanding C compiler with
/// no libc yet (crossdev `GUSE_DISABLE_STAGE_1`).
const GCC_DISABLE_STAGE1: &[&str] = &[
    "-fortran",
    "-d",
    "-go",
    "-jit",
    "-cxx",
    "-openmp",
    "-sanitize",
    "-zstd",
    "-zlib",
];
/// Additional gcc USE forced off for **stage2** (crossdev `GUSE_DISABLE_STAGE_2`).
const GCC_DISABLE_STAGE2: &[&str] = &["-sanitize"];

/// One ordered build in a [`StagePlan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageStep {
    /// Human label for progress / dry-run (e.g. `gcc-stage1`).
    pub label: String,
    /// Atoms to merge for this step, in order (e.g. `cross-riscv64-…/gcc`).
    pub atoms: Vec<String>,
    /// USE tokens forced for this step, in emerge syntax (`headers-only`,
    /// `-cxx`). Applied on top of the configured USE.
    pub use_override: Vec<String>,
    /// Skip dependency resolution (crossdev's `--nodeps`): used for the
    /// headers-only libc step, to break the glibc→newer-gcc cycle before a
    /// compiler exists.
    pub nodeps: bool,
    /// Merge into `--target`'s sysroot even when the plan-wide driver uses
    /// outer EROOT (`use_outer_eroot` for host-side `cross-*` tools). Needed
    /// for sysroot baselayout: it is not a `cross-*` atom, so without this it
    /// only seeds the outer prefix while libc writes under `usr/<tuple>/`.
    /// Keep the default merged-usr profile; match disk to it.
    pub into_sysroot: bool,
}

/// An ordered sequence of [`StageStep`]s run against one root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagePlan {
    /// The steps, in build order.
    pub steps: Vec<StageStep>,
}

impl Libc {
    /// Package name in `::gentoo` (the `cross-*` overlay symlinks the same name).
    fn pkg_name(self) -> &'static str {
        match self {
            Libc::Glibc => "glibc",
            Libc::Musl => "musl",
            Libc::Newlib => "newlib",
        }
    }
}

/// The flavour of staged toolchain bootstrap: **cross** or **native**. The
/// ordered step sequence ([`toolchain_plan`]) is identical; only how each
/// component is named as an atom differs — cross rewrites the category to
/// `cross-<tuple>`, native keeps the real `::gentoo` category. Single typed
/// decision point for "build a toolchain into a fresh root", replacing a
/// cross-vs-native split the driver used to re-derive at each call site.
#[derive(Debug, Clone)]
pub enum BootstrapKind {
    /// Cross-compilation into a `<CTARGET>` sysroot (`CBUILD ≠ CHOST`): atoms
    /// resolve to the `cross-<tuple>` overlay category.
    Cross(CrossTarget),
    /// Native self-hosting stage1 into `--root` (`CBUILD == CHOST`): atoms keep
    /// their real `::gentoo` category. Single full gcc (the seed compiler builds
    /// glibc — no two-stage split), with kernel headers. (A native LLVM stage1
    /// has the same shape but is not yet wired.)
    Native,
}

impl BootstrapKind {
    /// The category-qualified atom for component `(real_cat, pkg)` in
    /// `::gentoo`. Cross maps every component under `cross-<tuple>`; native uses
    /// the real category verbatim.
    fn atom(&self, real_cat: &str, pkg: &str) -> String {
        match self {
            BootstrapKind::Cross(t) => format!("{}/{pkg}", t.category()),
            BootstrapKind::Native => format!("{real_cat}/{pkg}"),
        }
    }

    /// LLVM/Clang model (target runtimes, no two-stage gcc) vs the GCC
    /// two-stage.
    fn llvm(&self) -> bool {
        match self {
            BootstrapKind::Cross(t) => t.llvm,
            BootstrapKind::Native => false,
        }
    }

    /// Whether the target OS has a kernel (the `sys-kernel/linux-headers` step).
    fn has_kernel(&self) -> bool {
        match self {
            BootstrapKind::Cross(t) => t.has_kernel,
            BootstrapKind::Native => true,
        }
    }

    /// The libc package name (`glibc` / `musl` / `newlib`).
    fn libc_pkg(&self) -> &'static str {
        match self {
            BootstrapKind::Cross(t) => t.libc.pkg_name(),
            BootstrapKind::Native => "glibc",
        }
    }

    /// The kernel-headers step atom. Native merges the `virtual/os-headers` meta
    /// (glibc DEPENDs on the virtual, which must be installed *in* a SYSROOT=ROOT
    /// build — merging it registers the virtual plus the linux-headers provider in
    /// the ROOT VDB). Cross builds the provider directly: no `virtual/*` in its
    /// overlay, and its DEPENDs resolve against the host where the virtual exists.
    fn kernel_headers_atom(&self) -> String {
        match self {
            BootstrapKind::Cross(_) => self.atom("sys-kernel", "linux-headers"),
            BootstrapKind::Native => "virtual/os-headers".to_string(),
        }
    }
}

/// The staged toolchain-bootstrap plan for `kind`: the ordered crossdev
/// sequence that produces a working compiler + headers + libc in a fresh root.
/// The driver must run the whole thing — the compiler is not usable until the
/// libc step lands, so the toolchain and the (stage1) libc are one intertwined
/// bootstrap.
///
/// `self_contained` distinguishes a from-scratch `--root DIR` EPREFIX (own
/// empty VDB, no host-shared merged-usr skeleton) from the default
/// `--local`/`--prefix` crossdev EPREFIX, which shares the host's
/// already-populated system. `BootstrapKind::Native` is always
/// self-contained and ignores this flag; it only changes
/// `BootstrapKind::Cross`'s plan (baselayout skeleton, dropping debuginfod).
///
/// `prefix_guest` is `BootstrapKind::Native`-only (`Cross` ignores it — a
/// host-OS concept, not a cross-target one). `virtual/libc`'s RDEPEND
/// already collapses to a bare blocker under it, so the *kernel-headers*
/// step needs no change — it just resolves to an empty merge. Only the
/// *libc* step needs an explicit skip: it merges `sys-libs/<libc>` directly
/// under `--nodeps` (to break the glibc-needs-gcc cycle), bypassing
/// `virtual/libc`'s conditional RDEPEND entirely, so the flag must be read
/// explicitly instead. See [prefix-guest is
/// host-OS-agnostic](../../docs/user/root-model.md) for the flag itself.
pub fn toolchain_plan(kind: &BootstrapKind, self_contained: bool, prefix_guest: bool) -> StagePlan {
    let atom = |real_cat: &str, pkg: &str| kind.atom(real_cat, pkg);
    let owned = |toks: &[&str]| toks.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let mut steps = Vec::new();

    // Baselayout first for every empty-ROOT/sysroot bootstrap — native,
    // self-contained cross, **and** default host-shared cross. gcc's startfile
    // osdir is `../lib64` (needs `/usr/lib`); modern `merged-usr` profiles also
    // need baselayout's bin↔usr/bin skeleton before any package writes real
    // content into `/bin` vs `/usr/bin`. `link_abi_osdirs` only bridges libdirs
    // and does not create that layout.
    //
    // Real category (not `atom()`): baselayout is not in the cross overlay.
    // Cross: `into_sysroot` so it lands under `--target` despite plan-wide
    // `use_outer_eroot` for host tools. Keep merged-usr profile; match disk.
    let baselayout_into_sysroot = matches!(kind, BootstrapKind::Cross(_));
    steps.push(StageStep {
        label: "baselayout".into(),
        atoms: vec!["sys-apps/baselayout".to_string()],
        use_override: owned(&["build"]),
        nodeps: false,
        into_sysroot: baselayout_into_sysroot,
    });

    if kind.llvm() {
        // LLVM model: host clang already cross-targets, so there is no two-stage
        // gcc. baselayout → wrappers → kernel headers → libc → runtimes.
        steps.push(StageStep {
            label: "clang wrappers".into(),
            atoms: vec![atom("sys-devel", "clang-crossdev-wrappers")],
            use_override: vec![],
            nodeps: false,
            into_sysroot: false,
        });
        if kind.has_kernel() {
            steps.push(StageStep {
                label: "kernel headers".into(),
                atoms: vec![kind.kernel_headers_atom()],
                use_override: owned(&["headers-only"]),
                nodeps: false,
                into_sysroot: false,
            });
        }
        steps.push(StageStep {
            label: "libc".into(),
            atoms: vec![atom("sys-libs", kind.libc_pkg())],
            use_override: vec![],
            nodeps: false,
            into_sysroot: false,
        });
        for rt in ["compiler-rt", "libunwind", "libcxxabi", "libcxx"] {
            steps.push(StageStep {
                label: rt.into(),
                atoms: vec![atom("llvm-runtimes", rt)],
                use_override: vec![],
                nodeps: false,
                into_sysroot: false,
            });
        }
        return StagePlan { steps };
    }

    // Self-contained still gates other empty-ROOT specials (debuginfod drop,
    // os-headers for EPREFIX) below — baselayout is no longer one of them.
    let is_self_contained_bootstrap = matches!(kind, BootstrapKind::Native) || self_contained;

    // A real python merge, not a `package.provided` claim: python.eclass's
    // own checks re-verify against the VDB at build time, independent of
    // whatever satisfied the depgraph — found live, `dev-build/ninja` died
    // `No supported Python implementation installed` even with
    // `dev-lang/python` in `package.provided` (that only ever satisfies
    // dependency *resolution*, never writes a VDB entry). Host-shared cross
    // skips this: the host VDB is dual-rooted in already.
    //
    // Placed before binutils: on an empty VDB, binutils's own dependency
    // closure already reaches python-consuming build tools (meson, ninja),
    // so python must be real by the time that step resolves, not just gcc's.
    if is_self_contained_bootstrap {
        steps.push(StageStep {
            label: "python".into(),
            atoms: vec!["dev-lang/python".to_string()],
            use_override: vec![],
            nodeps: false,
            into_sysroot: false,
        });
    }

    // Empty ROOT + debuginfod pulls elfutils→curl→…→glibc and trips
    // os-headers pre-flight. Host-shared cross keeps debuginfod; native and
    // self-contained cross drop it.
    let binutils_use = if is_self_contained_bootstrap {
        owned(&["-debuginfod"])
    } else {
        vec![]
    };
    steps.push(StageStep {
        label: "binutils".into(),
        atoms: vec![atom("sys-devel", "binutils")],
        use_override: binutils_use,
        nodeps: false,
        into_sysroot: false,
    });

    // Native: seed compiler at BROOT builds full glibc; one full gcc after.
    // No in-ROOT gcc exists yet, so a normal libc resolve tries to pull gcc
    // (and libxcrypt) into ROOT and cycles with glibc. `--nodeps` on libc
    // mirrors cross's headers-first break: use already-merged headers + BROOT.
    if let BootstrapKind::Native = kind {
        if kind.has_kernel() {
            steps.push(StageStep {
                label: "kernel headers".into(),
                atoms: vec![kind.kernel_headers_atom()],
                use_override: owned(&["headers-only"]),
                nodeps: false,
                into_sysroot: false,
            });
        }
        if !prefix_guest {
            steps.push(StageStep {
                label: "libc".into(),
                atoms: vec![atom("sys-libs", kind.libc_pkg())],
                use_override: vec![],
                nodeps: true,
                into_sysroot: false,
            });
        }
        steps.push(StageStep {
            label: "gcc".into(),
            atoms: vec![atom("sys-devel", "gcc")],
            use_override: owned(GCC_DISABLE),
            nodeps: false,
            into_sysroot: false,
        });
        return StagePlan { steps };
    }

    // Cross has no compiler for CTARGET yet, so it needs the classic two-stage
    // bootstrap: kernel headers → libc *headers* (--nodeps) → gcc-stage1 (a
    // freestanding C compiler, `--disable-shared` via is_crosscompile) → full
    // libc → gcc-stage2.
    if kind.has_kernel() {
        // Target `linux-headers` into the sysroot does not satisfy
        // `virtual/os-headers` on the EPREFIX installed view (glibc BDEPEND).
        // Host-shared mode already has it; self-contained needs it merged.
        if self_contained {
            steps.push(StageStep {
                label: "os-headers (EPREFIX)".into(),
                atoms: vec!["virtual/os-headers".to_string()],
                use_override: owned(&["headers-only"]),
                nodeps: false,
                into_sysroot: false,
            });
        }
        steps.push(StageStep {
            label: "kernel headers".into(),
            atoms: vec![kind.kernel_headers_atom()],
            use_override: owned(&["headers-only"]),
            nodeps: false,
            into_sysroot: false,
        });
        // libc headers first (--nodeps): gcc-stage1 needs them, but glibc itself
        // may DEPEND on a newer gcc we don't have yet — break the cycle.
        steps.push(StageStep {
            label: "libc headers".into(),
            atoms: vec![atom("sys-libs", kind.libc_pkg())],
            use_override: owned(&["headers-only"]),
            nodeps: true,
            into_sysroot: false,
        });
    }
    let mut stage1 = owned(GCC_DISABLE);
    stage1.extend(owned(GCC_DISABLE_STAGE1));
    steps.push(StageStep {
        label: "gcc-stage1".into(),
        atoms: vec![atom("sys-devel", "gcc")],
        use_override: stage1,
        nodeps: false,
        into_sysroot: false,
    });
    steps.push(StageStep {
        label: "libc".into(),
        atoms: vec![atom("sys-libs", kind.libc_pkg())],
        use_override: vec![],
        nodeps: false,
        into_sysroot: false,
    });
    let mut stage2 = owned(GCC_DISABLE);
    stage2.extend(owned(GCC_DISABLE_STAGE2));
    steps.push(StageStep {
        label: "gcc-stage2".into(),
        atoms: vec![atom("sys-devel", "gcc")],
        use_override: stage2,
        nodeps: false,
        into_sysroot: false,
    });
    StagePlan { steps }
}

/// Just the gcc refresh for an **already-bootstrapped** cross toolchain:
/// gcc-stage1 → gcc-stage2, reusing the existing binutils/libc untouched.
///
/// Not part of [`toolchain_plan`] (which is for a from-scratch bootstrap and
/// includes the unconditional-reinstall `libc headers` `--nodeps` step to
/// break the empty-sysroot cycle) — rerunning that against an
/// already-bootstrapped sysroot would blindly reinstall the headers-only
/// variant on top of the real, full glibc already there.
///
/// A version-only gcc refresh needs neither that nor the full "libc"
/// rebuild step between gcc-stage1/gcc-stage2 (that step exists there
/// because, mid-*bootstrap*, only libc *headers* exist before it runs; here
/// the full libc is already in place and gcc-stage2 just links against it).
///
/// Used when `sys-devel/gcc`'s resolved version needs a newer
/// `cross-<CTARGET>/gcc` than what `gcc-config` currently has active — see
/// `stage1()` in `crossdev/mod.rs`.
///
/// `version` pins the exact `sys-devel/gcc` version just resolved (e.g.
/// `"16.1.1_p20260606"`), via an `=` atom rather than a bare `cross-<CTARGET>
/// /gcc`. A bare atom resolves like a plain `emerge <atom>` — reinstalling
/// whatever's already satisfied/installed rather than upgrading — which
/// silently rebuilds the same old major. Pinning keeps the cross compiler
/// and the `sys-devel/gcc` it builds on the same major release.
pub fn gcc_refresh_plan(target: &CrossTarget, version: &str) -> StagePlan {
    let kind = BootstrapKind::Cross(target.clone());
    let atom = |real_cat: &str, pkg: &str| format!("={}-{version}", kind.atom(real_cat, pkg));
    let owned = |toks: &[&str]| toks.iter().map(|s| s.to_string()).collect::<Vec<_>>();

    let mut stage1 = owned(GCC_DISABLE);
    stage1.extend(owned(GCC_DISABLE_STAGE1));
    let mut stage2 = owned(GCC_DISABLE);
    stage2.extend(owned(GCC_DISABLE_STAGE2));

    StagePlan {
        steps: vec![
            StageStep {
                label: "gcc-stage1 (refresh)".into(),
                atoms: vec![atom("sys-devel", "gcc")],
                use_override: stage1,
                nodeps: false,
                into_sysroot: false,
            },
            StageStep {
                label: "gcc-stage2 (refresh)".into(),
                atoms: vec![atom("sys-devel", "gcc")],
                use_override: stage2,
                nodeps: false,
                into_sysroot: false,
            },
        ],
    }
}

/// The native **stage1** plan (catalyst `stage1/chroot.sh`): baselayout first
/// (`USE=build`, `--nodeps` — the bare FS skeleton), then the profile's
/// [`packages.build`](ProfileStack::stage1_packages) set with
/// `USE="-* build ${BOOTSTRAP_USE}"`, matching catalyst's own recipe. See
/// [Stage1](../../docs/user/stages-and-testing.md) for why `BOOTSTRAP_USE`
/// must be spliced back in explicitly (it isn't part of the profile's `USE`
/// fold itself) and why `--autosolve-use` is always on for this step.
///
/// Distinct from [`toolchain_plan`]'s `BootstrapKind::Native`, which builds
/// the *compiler* itself (binutils/glibc/gcc) — stage1 assumes that
/// toolchain already exists and just emerges the minimal bootable package
/// set with it, mirroring crossdev-stages' `install_stage1`.
pub fn stage1_plan(stack: &ProfileStack, bootstrap_use: &[String]) -> Result<StagePlan> {
    let mut steps = vec![StageStep {
        label: "baselayout".into(),
        atoms: vec!["sys-apps/baselayout".to_string()],
        use_override: vec!["build".to_string()],
        nodeps: true,
        into_sysroot: false,
    }];
    let atoms: Vec<String> = stack
        .stage1_packages()?
        .iter()
        .map(|d| d.to_string())
        .collect();
    let mut use_override = vec!["-*".to_string(), "build".to_string()];
    use_override.extend(bootstrap_use.iter().cloned());
    steps.push(StageStep {
        label: "packages.build".into(),
        atoms,
        use_override,
        nodeps: false,
        into_sysroot: false,
    });
    Ok(StagePlan { steps })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(plan: &StagePlan) -> Vec<&str> {
        plan.steps.iter().map(|s| s.label.as_str()).collect()
    }

    #[test]
    fn stage1_plan_is_baselayout_then_the_versioned_build_set() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(
            profile.join("packages.build"),
            "sys-devel/binutils\nsys-apps/baselayout\nsys-devel/gcc\n",
        )
        .unwrap();
        std::fs::write(profile.join("packages"), "*>=sys-devel/gcc-13\n").unwrap();

        let stack = ProfileStack::build(profile).unwrap();
        let plan = stage1_plan(&stack, &[]).unwrap();

        assert_eq!(labels(&plan), ["baselayout", "packages.build"]);
        // Step 1: the isolated USE=build --nodeps baselayout merge.
        assert!(plan.steps[0].nodeps);
        assert_eq!(plan.steps[0].atoms, ["sys-apps/baselayout"]);
        assert_eq!(plan.steps[0].use_override, ["build"]);
        // Step 2: the full build-order list, version-qualified from `packages`,
        // with the collapse-all USE (alternative defaults via --autosolve-use).
        assert_eq!(plan.steps[1].use_override, ["-*", "build"]);
        assert_eq!(
            plan.steps[1].atoms,
            [
                "sys-devel/binutils",
                "sys-apps/baselayout",
                ">=sys-devel/gcc-13"
            ]
        );
    }

    // Regression: catalyst's stage1 recipe re-adds the profile's own
    // `BOOTSTRAP_USE` after the `-*` clear (`targets/stage1/chroot.sh`:
    // `USE="${CATALYST_USE} ${USE} ${BOOTSTRAP_USE} ..."`) — without it,
    // anything gated only on a profile-level default (e.g.
    // `python_targets_python3_14`, per `profiles/base/make.defaults`'s own
    // comment) gets wiped by `-*` and never comes back, breaking packages
    // that need a python impl.
    #[test]
    fn stage1_plan_reapplies_bootstrap_use_after_the_wildcard_reset() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(profile.join("packages.build"), "sys-apps/baselayout\n").unwrap();

        let stack = ProfileStack::build(profile).unwrap();
        let bootstrap_use = [
            "unicode".to_string(),
            "python_targets_python3_14".to_string(),
        ];
        let plan = stage1_plan(&stack, &bootstrap_use).unwrap();

        assert_eq!(
            plan.steps[1].use_override,
            ["-*", "build", "unicode", "python_targets_python3_14"]
        );
    }

    #[test]
    fn gcc_glibc_plan_is_the_two_stage_bootstrap() {
        let t = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let plan = toolchain_plan(&BootstrapKind::Cross(t), false, false);
        assert_eq!(
            labels(&plan),
            [
                "baselayout",
                "binutils",
                "kernel headers",
                "libc headers",
                "gcc-stage1",
                "libc",
                "gcc-stage2",
            ]
        );
        // Baselayout is always real-category; cross-* atoms start at binutils.
        assert_eq!(plan.steps[0].atoms, ["sys-apps/baselayout"]);
        assert!(
            plan.steps[1].atoms[0].starts_with("cross-riscv64-unknown-linux-gnu/"),
            "{:?}",
            plan.steps[1].atoms
        );
        // Cross builds the linux-headers provider directly (no virtual/* in the
        // overlay; the cross DEPENDs resolve against the host).
        assert_eq!(
            plan.steps[2].atoms[0],
            "cross-riscv64-unknown-linux-gnu/linux-headers"
        );
        // libc headers step is the --nodeps cycle-breaker.
        let libc_headers = &plan.steps[3];
        assert!(libc_headers.nodeps);
        assert!(
            libc_headers
                .use_override
                .contains(&"headers-only".to_string())
        );
        // stage1 gcc drops cxx/libc-dependent USE; stage2 keeps them.
        assert!(plan.steps[4].use_override.contains(&"-cxx".to_string()));
        assert!(!plan.steps[6].use_override.contains(&"-cxx".to_string()));
    }

    /// **Invariant:** every `cross-<tuple>/<pkg>` atom `toolchain_plan` emits
    /// for a cross target must be derivable — i.e. its underlying real
    /// `(category, package)` must be in `CrossTarget::packages()`, the single
    /// source of truth the alias-derivation map (`Location::Alias`) is built
    /// from. If this fails, the plan references a package the resolver cannot
    /// alias, so a from-scratch `--setup` would `NoVersions` at runtime.
    ///
    /// Real-category bypass atoms (`sys-apps/baselayout`, `virtual/os-headers`)
    /// are intentionally not cross-aliased — they're host/EPREFIX-arch packages
    /// merged via their real category — so they're filtered out before the
    /// check, keeping the build plan honest.
    #[test]
    fn toolchain_plan_atoms_are_all_in_packages_set() {
        use portage_atom::Dep;
        for tuple in [
            "riscv64-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu",
            "armv7a-unknown-linux-gnueabihf",
        ] {
            let t = CrossTarget::parse(tuple, false).unwrap();
            let category = t.category();
            let plan = toolchain_plan(&BootstrapKind::Cross(t.clone()), true, false);
            let packages_set: std::collections::HashSet<(String, String)> = t
                .packages()
                .into_iter()
                .map(|(c, p, _)| (c.to_string(), p.to_string()))
                .collect();
            for step in &plan.steps {
                for atom in &step.atoms {
                    // Only check cross-category atoms; real-category bypass
                    // atoms (baselayout, virtual/os-headers) are intentionally
                    // not aliased.
                    let Ok(dep) = Dep::parse(atom) else {
                        continue;
                    };
                    if dep.category() != category {
                        continue;
                    }
                    let pkg = dep.package();
                    assert!(
                        packages_set.iter().any(|(_, p)| p == pkg),
                        "{tuple}: plan atom {atom:?} (pkg {pkg}) is not in \
                         CrossTarget::packages() {packages_set:?} — the alias \
                         derivation cannot resolve it",
                    );
                    // The derivation maps cross-<tuple>/<pkg> → <real-cat>/<pkg>,
                    // so the real category for this package must exist in the set
                    // (a package with no real category can't be aliased).
                    assert!(
                        packages_set
                            .iter()
                            .any(|(c, p)| p == pkg && c.as_str() != category),
                        "{tuple}: plan atom {atom:?}: package {pkg} has no real \
                         category in CrossTarget::packages() {packages_set:?}",
                    );
                }
            }
        }
    }

    #[test]
    fn gcc_refresh_plan_is_just_the_two_gcc_stages() {
        // Refreshing an already-bootstrapped toolchain's gcc must not touch
        // binutils/headers/libc — those are the fresh-bootstrap-only steps
        // toolchain_plan needs, and rerunning "libc headers" (an unconditional
        // --nodeps reinstall) would overwrite an already-full glibc with the
        // stripped bootstrap headers.
        let t = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let plan = gcc_refresh_plan(&t, "16.1.1_p20260606");
        assert_eq!(
            labels(&plan),
            ["gcc-stage1 (refresh)", "gcc-stage2 (refresh)"]
        );
        for step in &plan.steps {
            assert!(!step.nodeps);
            // Pinned to the exact resolved version (`=` atom), not a bare
            // atom — see the doc comment on why a bare atom is wrong here.
            assert_eq!(
                step.atoms[0],
                "=cross-riscv64-unknown-linux-gnu/gcc-16.1.1_p20260606"
            );
        }
        // Same USE split as toolchain_plan's own gcc-stage1/gcc-stage2.
        assert!(plan.steps[0].use_override.contains(&"-cxx".to_string()));
        assert!(!plan.steps[1].use_override.contains(&"-cxx".to_string()));
    }

    #[test]
    fn self_contained_cross_gets_baselayout_and_drops_debuginfod() {
        // A from-scratch `--root DIR` crossdev EPREFIX has no host-shared
        // merged-usr skeleton or libs — same needs as native
        let t = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let plan = toolchain_plan(&BootstrapKind::Cross(t), true, false);
        assert_eq!(labels(&plan)[0], "baselayout");
        assert!(plan.steps[0].atoms[0].ends_with("/baselayout"));
        let binutils = plan.steps.iter().find(|s| s.label == "binutils").unwrap();
        assert!(binutils.use_override.contains(&"-debuginfod".to_string()));
        // The EPREFIX's own installed view has nothing satisfying
        // virtual/os-headers (unlike host-shared mode), so it needs its own
        // real merge of the virtual, distinct from the cross-specific target
        // linux-headers step.
        let os_headers = plan
            .steps
            .iter()
            .find(|s| s.label == "os-headers (EPREFIX)")
            .expect("self-contained cross plan must merge virtual/os-headers for the EPREFIX");
        assert_eq!(os_headers.atoms, ["virtual/os-headers"]);
        let idx_os_headers = plan.steps.iter().position(|s| s == os_headers).unwrap();
        let idx_kernel_headers = plan
            .steps
            .iter()
            .position(|s| s.label == "kernel headers")
            .unwrap();
        assert!(idx_os_headers < idx_kernel_headers);
    }

    #[test]
    fn default_cross_seeds_baselayout_and_keeps_debuginfod() {
        // Host-shared cross still needs baselayout in the sysroot (merged-usr);
        // debuginfod stays on (host satisfies DEPEND). os-headers is
        // self-contained-only.
        let t = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let plan = toolchain_plan(&BootstrapKind::Cross(t), false, false);
        assert_eq!(labels(&plan)[0], "baselayout");
        assert_eq!(plan.steps[0].atoms, ["sys-apps/baselayout"]);
        let binutils = plan.steps.iter().find(|s| s.label == "binutils").unwrap();
        assert!(binutils.use_override.is_empty());
        assert!(!labels(&plan).contains(&"os-headers (EPREFIX)"));
    }

    #[test]
    fn llvm_plan_seeds_baselayout_before_wrappers() {
        let t = CrossTarget::parse("aarch64-unknown-linux-musl", true).unwrap();
        let plan = toolchain_plan(&BootstrapKind::Cross(t), false, false);
        assert_eq!(labels(&plan)[0], "baselayout");
        assert_eq!(plan.steps[0].atoms, ["sys-apps/baselayout"]);
        assert!(
            plan.steps[0].into_sysroot,
            "cross baselayout must merge into the target sysroot, not only outer EROOT"
        );
        assert_eq!(labels(&plan)[1], "clang wrappers");
        assert!(!plan.steps[1].into_sysroot);
    }

    #[test]
    fn native_baselayout_does_not_force_sysroot() {
        let plan = toolchain_plan(&BootstrapKind::Native, true, false);
        assert_eq!(plan.steps[0].label, "baselayout");
        assert!(
            !plan.steps[0].into_sysroot,
            "native toolchain has no --target sysroot substitution to force"
        );
    }

    #[test]
    fn baremetal_newlib_has_no_kernel_headers() {
        let t = CrossTarget::parse("riscv64-unknown-elf", false).unwrap();
        let plan = toolchain_plan(&BootstrapKind::Cross(t), false, false);
        assert!(!labels(&plan).contains(&"kernel headers"));
        assert!(plan.steps.iter().any(|s| s.atoms[0].ends_with("/newlib")));
    }

    #[test]
    fn llvm_plan_has_runtimes_not_two_stage_gcc() {
        let t = CrossTarget::parse("aarch64-unknown-linux-musl", true).unwrap();
        let plan = toolchain_plan(&BootstrapKind::Cross(t), false, false);
        let l = labels(&plan);
        assert!(l.contains(&"clang wrappers"));
        assert!(l.contains(&"compiler-rt"));
        assert!(!l.iter().any(|s| s.starts_with("gcc-stage")));
        assert!(plan.steps.iter().any(|s| s.atoms[0].ends_with("/musl")));
    }

    #[test]
    fn native_plan_is_seed_built_single_stage_gcc() {
        // A native stage1 (CHOST == CBUILD) uses plain ::gentoo atoms (no cross-*
        // overlay) and — unlike cross — has NO two-stage gcc: the seed compiler
        // builds full glibc, then a single full gcc links against it.
        // toolchain.eclass gates all stage1 affordances on is_crosscompile, so a
        // native gcc is always --enable-shared and needs a full libc present.
        let plan = toolchain_plan(&BootstrapKind::Native, true, false);
        assert_eq!(
            labels(&plan),
            [
                "baselayout",
                "python",
                "binutils",
                "kernel headers",
                "libc",
                "gcc"
            ]
        );
        // Real categories, no `cross-` rewrite.
        let atoms: Vec<&str> = plan
            .steps
            .iter()
            .flat_map(|s| s.atoms.iter().map(|a| a.as_str()))
            .collect();
        // baselayout first: lays down the /usr/lib skeleton for gcc's osdir.
        assert_eq!(atoms[0], "sys-apps/baselayout");
        assert!(plan.steps[0].use_override.contains(&"build".to_string()));
        // Real python next: an empty VDB means anything binutils's own
        // closure needs that's python-eclass-based must already be real,
        // not just depgraph-satisfied.
        assert_eq!(atoms[1], "dev-lang/python");
        assert_eq!(atoms[2], "sys-devel/binutils");
        // Native merges the virtual (registers it in the ROOT VDB for glibc's
        // DEPEND), not the bare linux-headers provider.
        assert_eq!(atoms[3], "virtual/os-headers");
        assert_eq!(atoms[4], "sys-libs/glibc");
        assert_eq!(atoms[5], "sys-devel/gcc");
        assert!(atoms.iter().all(|a| !a.starts_with("cross-")));
        // The full libc step is a real (non-headers-only) build, but --nodeps:
        // glibc's own COMMON_DEPEND (`>=sys-devel/gcc-6.2`) can't be satisfied
        // from ROOT here (no gcc-stage1 landed first, unlike cross) — the seed
        // compiler at BROOT does the actual compiling instead.
        assert!(plan.steps[4].nodeps);
        assert!(plan.steps[4].use_override.is_empty());
        // The single gcc is full (keeps cxx — only GCC_DISABLE applies, no STAGE1).
        assert!(!plan.steps[5].use_override.contains(&"-cxx".to_string()));
        assert!(plan.steps[5].use_override.contains(&"-vtv".to_string()));
        // Native binutils drops debuginfod (else its elfutils→…→glibc closure
        // explodes the binutils step into the empty ROOT).
        assert!(
            plan.steps[2]
                .use_override
                .contains(&"-debuginfod".to_string())
        );
    }

    #[test]
    fn native_prefix_guest_skips_the_libc_step() {
        // FreeBSD/Darwin: ::gentoo has no libc ebuilds for the target OS at
        // all, but virtual/os-headers already collapses to an empty merge
        // under prefix-guest via its own RDEPEND conditional (no plan change
        // needed there) — only the libc step, which bypasses virtual/libc
        // entirely via --nodeps, needs an explicit skip.
        let plan = toolchain_plan(&BootstrapKind::Native, true, true);
        assert_eq!(
            labels(&plan),
            ["baselayout", "python", "binutils", "kernel headers", "gcc"]
        );
        let atoms: Vec<&str> = plan
            .steps
            .iter()
            .flat_map(|s| s.atoms.iter().map(|a| a.as_str()))
            .collect();
        assert!(!atoms.contains(&"sys-libs/glibc"));
        // gcc step is otherwise unchanged.
        assert_eq!(atoms.last(), Some(&"sys-devel/gcc"));
    }

    #[test]
    fn cross_binutils_keeps_debuginfod() {
        // Cross binutils is host-rooted (via crossdev::setup's
        // use_outer_eroot), so its debuginfod deps are host-satisfied —
        // no need to force the flag off (behaviour-preserving).
        let t = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let plan = toolchain_plan(&BootstrapKind::Cross(t), false, false);
        let binutils = plan
            .steps
            .iter()
            .find(|s| s.label == "binutils")
            .expect("binutils step");
        assert!(binutils.use_override.is_empty());
    }
}
