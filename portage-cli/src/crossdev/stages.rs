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
/// `cross-<tuple>`, native keeps the real `::gentoo` category. This is the
/// single typed decision point for "build a toolchain into a fresh root": the
/// cross-vs-native split that the driver previously re-derived at each call
/// site.
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
/// `self_contained` distinguishes a from-scratch `--root DIR` EPREFIX (its own
/// empty VDB, no host-shared libs/merged-usr skeleton) from the default
/// `--local`/`--prefix` crossdev EPREFIX, which shares the host's already-
/// merged-usr, already-populated system. `BootstrapKind::Native` is always
/// self-contained (that's its only use case) and ignores this flag; it only
/// changes `BootstrapKind::Cross`'s plan — see the two call sites below for
/// what "self-contained" unlocks (baselayout skeleton, dropping debuginfod).
pub fn toolchain_plan(kind: &BootstrapKind, self_contained: bool) -> StagePlan {
    let atom = |real_cat: &str, pkg: &str| kind.atom(real_cat, pkg);
    let owned = |toks: &[&str]| toks.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let mut steps = Vec::new();

    if kind.llvm() {
        // LLVM model: host clang already cross-targets, so there is no two-stage
        // gcc. Wrappers → kernel headers → libc → runtimes.
        steps.push(StageStep {
            label: "clang wrappers".into(),
            atoms: vec![atom("sys-devel", "clang-crossdev-wrappers")],
            use_override: vec![],
            nodeps: false,
        });
        if kind.has_kernel() {
            steps.push(StageStep {
                label: "kernel headers".into(),
                atoms: vec![kind.kernel_headers_atom()],
                use_override: owned(&["headers-only"]),
                nodeps: false,
            });
        }
        steps.push(StageStep {
            label: "libc".into(),
            atoms: vec![atom("sys-libs", kind.libc_pkg())],
            use_override: vec![],
            nodeps: false,
        });
        for rt in ["compiler-rt", "libunwind", "libcxxabi", "libcxx"] {
            steps.push(StageStep {
                label: rt.into(),
                atoms: vec![atom("llvm-runtimes", rt)],
                use_override: vec![],
                nodeps: false,
            });
        }
        return StagePlan { steps };
    }

    // Native: baselayout first for the `/usr/lib` skeleton. gcc's startfile osdir
    // is `../lib64`, so it resolves CRT objects via `<sysroot>/usr/lib/../lib64`,
    // which a fresh ROOT can't traverse without `/usr/lib` (→ `cannot find
    // crti.o`). Cross bridges the libdir with `link_abi_osdirs` instead. A
    // self-contained cross EPREFIX needs the same bare-FS skeleton for the same
    // reason native does (its `--root` has no host-shared `/usr/lib`/merged-usr
    // layout either) — found 2026-07-03 doing a from-scratch cross-stage1 test,
    // see [[stage-build-shakeout]].
    let is_self_contained_bootstrap = matches!(kind, BootstrapKind::Native) || self_contained;
    if is_self_contained_bootstrap {
        // Always the real category: baselayout is a host/EPREFIX-arch package
        // in both cases (never part of the `cross-<tuple>` overlay's package
        // set — that overlay only symlinks the toolchain components), so it
        // must bypass `atom()`'s cross rewrite.
        steps.push(StageStep {
            label: "baselayout".into(),
            atoms: vec!["sys-apps/baselayout".to_string()],
            use_override: owned(&["build"]),
            nodeps: false,
        });
    }

    // Native binutils drops `debuginfod`: into the empty ROOT it would pull
    // elfutils → curl → … → glibc (47 pkgs vs 7) and trip the os-headers
    // pre-flight. The default cross EPREFIX is host-rooted, so those deps are
    // satisfied — keep it. A self-contained cross EPREFIX is just as empty as
    // native's, so it hits the exact same explosion — drop it there too.
    //
    // 2026-07-09 correction: this was briefly changed to *always* drop it,
    // after `em --prefix P --target T crossdev --setup` hit exactly this
    // explosion for a *non*-self-contained EPREFIX — but the real cause was a
    // separate bug (`crossdev::setup`'s `run_staged` call still passing
    // `use_outer_eroot: false` after the `--cross`/`-t` -> `--target`
    // unification, so `cross-<tuple>/binutils` wrongly resolved DEPEND
    // against the empty *sysroot* instead of the host). With that fixed,
    // "the default cross EPREFIX is host-rooted" is true again, and
    // unconditionally dropping debuginfod was an unnecessary, wrong-reason
    // workaround layered on top of the real fix — reverted.
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
    });

    // Native breaks the glibc ↔ gcc cycle with the seed compiler (`BROOT=/`),
    // which builds full glibc directly; a single full gcc links against it. No
    // two-stage split — `toolchain.eclass` gates that on `is_crosscompile`, so a
    // native gcc is always `--enable-shared` and needs a full in-ROOT libc.
    //
    // Unlike cross (which has a real `gcc-stage1` merged into the sysroot
    // *before* its own full "libc" step, trivially satisfying glibc's
    // `>=sys-devel/gcc-6.2` COMMON_DEPEND — see glibc.ebuild — from the
    // already-installed ROOT view), native never installs a ROOT-resident gcc
    // ahead of libc at all: the seed compiler that actually does the
    // compiling lives at BROOT, not ROOT. A normal-resolution "libc" step
    // therefore sees glibc's own DEPEND as unsatisfiable in ROOT and tries to
    // plan a fresh in-ROOT gcc/libxcrypt build to satisfy it — which in turn
    // DEPENDs back on glibc, a real, unbreakable-by-ordering cycle confirmed
    // live (2026-07-11: a real, non-pretend run still hit this after
    // baselayout/binutils/kernel-headers had genuinely merged). `--nodeps`
    // here, mirroring cross's analogous "libc headers first (--nodeps)"
    // break, sidesteps it the same way cross does: rely on what's already
    // merged (baselayout/binutils/kernel-headers) plus the host's seed
    // compiler at BROOT, not a fresh in-ROOT one.
    if let BootstrapKind::Native = kind {
        if kind.has_kernel() {
            steps.push(StageStep {
                label: "kernel headers".into(),
                atoms: vec![kind.kernel_headers_atom()],
                use_override: owned(&["headers-only"]),
                nodeps: false,
            });
        }
        steps.push(StageStep {
            label: "libc".into(),
            atoms: vec![atom("sys-libs", kind.libc_pkg())],
            use_override: vec![],
            nodeps: true,
        });
        steps.push(StageStep {
            label: "gcc".into(),
            atoms: vec![atom("sys-devel", "gcc")],
            use_override: owned(GCC_DISABLE),
            nodeps: false,
        });
        return StagePlan { steps };
    }

    // Cross has no compiler for CTARGET yet, so it needs the classic two-stage
    // bootstrap: kernel headers → libc *headers* (--nodeps) → gcc-stage1 (a
    // freestanding C compiler, `--disable-shared` via is_crosscompile) → full
    // libc → gcc-stage2.
    if kind.has_kernel() {
        // The cross-specific `linux-headers` step below installs the target's
        // arch-tailored UAPI headers *into the target sysroot subdirectory* —
        // it does NOT satisfy `virtual/os-headers`, a totally different
        // any-of dependency (`sys-kernel/linux-headers` et al) that
        // `cross-<tuple>/glibc`'s BDEPEND checks against the EPREFIX's own
        // installed view. In host-shared mode that's already satisfied by the
        // host's real installed headers; a self-contained EPREFIX has nothing
        // installed at all, so it needs the real virtual merged here too —
        // found 2026-07-03 doing a from-scratch cross-stage1 test (see
        // [[stage-build-shakeout]]).
        if self_contained {
            steps.push(StageStep {
                label: "os-headers (EPREFIX)".into(),
                atoms: vec!["virtual/os-headers".to_string()],
                use_override: owned(&["headers-only"]),
                nodeps: false,
            });
        }
        steps.push(StageStep {
            label: "kernel headers".into(),
            atoms: vec![kind.kernel_headers_atom()],
            use_override: owned(&["headers-only"]),
            nodeps: false,
        });
        // libc headers first (--nodeps): gcc-stage1 needs them, but glibc itself
        // may DEPEND on a newer gcc we don't have yet — break the cycle.
        steps.push(StageStep {
            label: "libc headers".into(),
            atoms: vec![atom("sys-libs", kind.libc_pkg())],
            use_override: owned(&["headers-only"]),
            nodeps: true,
        });
    }
    let mut stage1 = owned(GCC_DISABLE);
    stage1.extend(owned(GCC_DISABLE_STAGE1));
    steps.push(StageStep {
        label: "gcc-stage1".into(),
        atoms: vec![atom("sys-devel", "gcc")],
        use_override: stage1,
        nodeps: false,
    });
    steps.push(StageStep {
        label: "libc".into(),
        atoms: vec![atom("sys-libs", kind.libc_pkg())],
        use_override: vec![],
        nodeps: false,
    });
    let mut stage2 = owned(GCC_DISABLE);
    stage2.extend(owned(GCC_DISABLE_STAGE2));
    steps.push(StageStep {
        label: "gcc-stage2".into(),
        atoms: vec![atom("sys-devel", "gcc")],
        use_override: stage2,
        nodeps: false,
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
/// variant on top of the real, full glibc already there. A version-only gcc
/// refresh needs neither that nor the full "libc" rebuild step
/// `toolchain_plan` does between its own gcc-stage1/gcc-stage2 (that step
/// exists there because, mid-*bootstrap*, only libc *headers* exist before
/// it runs; here the full libc is already in place from the original
/// bootstrap and gcc-stage2 just links against it).
///
/// Used when `sys-devel/gcc`'s resolved version needs a newer
/// `cross-<CTARGET>/gcc` than what `gcc-config` currently has active — see
/// `stage1()` in `crossdev/mod.rs`.
///
/// `version` pins the exact `sys-devel/gcc` version just resolved (e.g.
/// `"16.1.1_p20260606"`), via an `=` atom rather than a bare `cross-<CTARGET>
/// /gcc`. A bare atom resolves like a plain `emerge <atom>` — reinstalling
/// whatever's already satisfied/installed rather than upgrading — which
/// silently rebuilds the *same* old major version and defeats the whole
/// point of this plan (caught live: the woven-in refresh reinstalled
/// `gcc-15.2.1` unchanged while `sys-devel/gcc-16.1.1` still failed on the
/// same driver-flag mismatch). Pinning the exact version also keeps the
/// build tool and the package it builds on the same major release.
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
            },
            StageStep {
                label: "gcc-stage2 (refresh)".into(),
                atoms: vec![atom("sys-devel", "gcc")],
                use_override: stage2,
                nodeps: false,
            },
        ],
    }
}

/// The native **stage1** plan (catalyst `stage1/chroot.sh`): baselayout first
/// (USE=build, `--nodeps` — the bare FS skeleton), then the profile's
/// [`packages.build`](ProfileStack::stage1_packages) set in one batch with
/// `USE="-* build ${BOOTSTRAP_USE}"` (catalyst's own recipe,
/// `targets/stage1/chroot.sh`: `CATALYST_USE="-* build ${BINDIST} ..."`;
/// `USE="${CATALYST_USE} ${USE} ${BOOTSTRAP_USE} ..."` — `BINDIST` dropped, a
/// catalyst-only var). `bootstrap_use` is the profile's own `BOOTSTRAP_USE`
/// variable (`profiles/base/make.defaults`), already merged across the
/// profile chain — its own comment there explains why this re-add is
/// required: "stage1 builds break because of `USE="-* ${BOOTSTRAP_USE}"`"
/// (it restores flags like `python_targets_python3_14` that a bare `-*`
/// would otherwise wipe, matching the exact fold-order semantics
/// `resolve_effective_use` implements — `BOOTSTRAP_USE` isn't itself part of
/// the profile's `USE` fold, so it has to be spliced in here explicitly,
/// same as catalyst does). Distinct from [`toolchain_plan`]'s
/// `BootstrapKind::Native`, which builds the *compiler* itself
/// (binutils/glibc/gcc) — stage1 assumes that toolchain already exists in the
/// root and just emerges the minimal bootable package set with it, mirroring
/// crossdev-stages' `install_stage1`.
pub fn stage1_plan(stack: &ProfileStack, bootstrap_use: &[String]) -> Result<StagePlan> {
    let mut steps = vec![StageStep {
        label: "baselayout".into(),
        atoms: vec!["sys-apps/baselayout".to_string()],
        use_override: vec!["build".to_string()],
        nodeps: true,
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
        // with the collapse-all USE.
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
    // comment) gets wiped by `-*` and never comes back, breaking any
    // stage1 package needing a python impl (found live: `dev-lang/python-exec`
    // failing `configure: error: Python implementations must be specified`).
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
        let plan = toolchain_plan(&BootstrapKind::Cross(t), false);
        assert_eq!(
            labels(&plan),
            [
                "binutils",
                "kernel headers",
                "libc headers",
                "gcc-stage1",
                "libc",
                "gcc-stage2",
            ]
        );
        // Atoms live in the cross-* overlay category.
        assert!(
            plan.steps[0].atoms[0].starts_with("cross-riscv64-unknown-linux-gnu/"),
            "{:?}",
            plan.steps[0].atoms
        );
        // Cross builds the linux-headers provider directly (no virtual/* in the
        // overlay; the cross DEPENDs resolve against the host).
        assert_eq!(
            plan.steps[1].atoms[0],
            "cross-riscv64-unknown-linux-gnu/linux-headers"
        );
        // libc headers step is the --nodeps cycle-breaker.
        let libc_headers = &plan.steps[2];
        assert!(libc_headers.nodeps);
        assert!(
            libc_headers
                .use_override
                .contains(&"headers-only".to_string())
        );
        // stage1 gcc drops cxx/libc-dependent USE; stage2 keeps them.
        assert!(plan.steps[3].use_override.contains(&"-cxx".to_string()));
        assert!(!plan.steps[5].use_override.contains(&"-cxx".to_string()));
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
            let plan = toolchain_plan(&BootstrapKind::Cross(t.clone()), true);
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
        // merged-usr skeleton or libs — same needs as native, found 2026-07-03
        // doing a real from-scratch cross-stage1 test.
        let t = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let plan = toolchain_plan(&BootstrapKind::Cross(t), true);
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
    fn default_cross_has_no_baselayout_and_keeps_debuginfod() {
        // The default (host-shared) cross EPREFIX is unaffected by the
        // self-contained fix above. debuginfod stays on: `use_outer_eroot`
        // (crossdev::setup) routes this step's DEPEND check to the host,
        // which already satisfies the closure.
        let t = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let plan = toolchain_plan(&BootstrapKind::Cross(t), false);
        assert!(!labels(&plan).contains(&"baselayout"));
        let binutils = plan.steps.iter().find(|s| s.label == "binutils").unwrap();
        assert!(binutils.use_override.is_empty());
        assert!(!labels(&plan).contains(&"os-headers (EPREFIX)"));
    }

    #[test]
    fn baremetal_newlib_has_no_kernel_headers() {
        let t = CrossTarget::parse("riscv64-unknown-elf", false).unwrap();
        let plan = toolchain_plan(&BootstrapKind::Cross(t), false);
        assert!(!labels(&plan).contains(&"kernel headers"));
        assert!(plan.steps.iter().any(|s| s.atoms[0].ends_with("/newlib")));
    }

    #[test]
    fn llvm_plan_has_runtimes_not_two_stage_gcc() {
        let t = CrossTarget::parse("aarch64-unknown-linux-musl", true).unwrap();
        let plan = toolchain_plan(&BootstrapKind::Cross(t), false);
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
        let plan = toolchain_plan(&BootstrapKind::Native, true);
        assert_eq!(
            labels(&plan),
            ["baselayout", "binutils", "kernel headers", "libc", "gcc"]
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
        assert_eq!(atoms[1], "sys-devel/binutils");
        // Native merges the virtual (registers it in the ROOT VDB for glibc's
        // DEPEND), not the bare linux-headers provider.
        assert_eq!(atoms[2], "virtual/os-headers");
        assert_eq!(atoms[3], "sys-libs/glibc");
        assert_eq!(atoms[4], "sys-devel/gcc");
        assert!(atoms.iter().all(|a| !a.starts_with("cross-")));
        // The full libc step is a real (non-headers-only) build, but --nodeps:
        // glibc's own COMMON_DEPEND (`>=sys-devel/gcc-6.2`) can't be satisfied
        // from ROOT here (no gcc-stage1 landed first, unlike cross) — the seed
        // compiler at BROOT does the actual compiling instead.
        assert!(plan.steps[3].nodeps);
        assert!(plan.steps[3].use_override.is_empty());
        // The single gcc is full (keeps cxx — only GCC_DISABLE applies, no STAGE1).
        assert!(!plan.steps[4].use_override.contains(&"-cxx".to_string()));
        assert!(plan.steps[4].use_override.contains(&"-vtv".to_string()));
        // Native binutils drops debuginfod (else its elfutils→…→glibc closure
        // explodes the binutils step into the empty ROOT).
        assert!(
            plan.steps[1]
                .use_override
                .contains(&"-debuginfod".to_string())
        );
    }

    #[test]
    fn cross_binutils_keeps_debuginfod() {
        // Cross binutils is host-rooted (via crossdev::setup's
        // use_outer_eroot), so its debuginfod deps are host-satisfied —
        // no need to force the flag off (behaviour-preserving).
        let t = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let plan = toolchain_plan(&BootstrapKind::Cross(t), false);
        assert_eq!(plan.steps[0].label, "binutils");
        assert!(plan.steps[0].use_override.is_empty());
    }
}
