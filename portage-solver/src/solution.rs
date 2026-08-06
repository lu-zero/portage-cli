//! Solver-agnostic solution and plan vocabulary.
//!
//! These are the types a [`crate::Solver`] implementation produces after a
//! resolve, expressed in plain Portage terms (`Cpn`, `Version`, slot) rather
//! than a solver's internal IDs. Consumers iterate these without knowing which
//! algorithm produced them.

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Operator, Version};
use thiserror::Error;

/// Where a real package instance is merged — host `BROOT` or target `ROOT`.
///
/// Under cross-compilation the same CPV can appear twice (native host tool +
/// cross target runtime). Native builds are always [`MergeRoot::Target`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord, Default)]
pub enum MergeRoot {
    /// Native build merged to the build host (`BROOT`, `/`).
    Host,
    /// Cross (or native target) build merged to `ROOT` / `EROOT`.
    #[default]
    Target,
}

/// What kind of build a plan entry is — the structural answer to "is this a
/// host-class or target-class build?", replacing the `cross_host_tool_tuple`
/// name-allowlist + `CTARGET`/`TARGET_ABI` shell-env sniffing the build shell
/// re-derives per entry today.
///
/// Computed once from the entry's identity + the solve's cross context (see
/// [`BuildClass::classify`]) and carried on the [`SelectedPackage`], so every
/// downstream consumer (build shell, preflight, display) reads one field
/// instead of re-deriving it from shadows. Part of the root-topology
/// refactor's Track A; see `todo/root-topology-refactor.md`.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum BuildClass {
    /// A native build merged to the build host (`BROOT`,
    /// [`MergeRoot::Host`]): typically an unsatisfied `BDEPEND` the host
    /// lacks, scheduled onto the host side.
    NativeHost,
    /// A native build merged to the target root (`ROOT`): `CBUILD == CHOST`,
    /// no `CTARGET`. Plain `em <atom>`, `--root`, `--prefix` target packages.
    NativeTarget,
    /// A foreign-arch build merged to the target root: a `--target`
    /// invocation with `CBUILD != CHOST`. `triple` is `CTARGET` where the
    /// topology carries it (threaded in Track A4; `None` until then).
    CrossTarget {
        /// The target triple (`CTARGET`), populated once Track A4 lifts the
        /// triples out of shell-env rediscovery.
        triple: Option<String>,
    },
    /// A `cross-<tuple>/` package that is a **host binary generating target
    /// code** — binutils/gcc/gdb/clang-crossdev-wrappers, and every
    /// `--ex-pkg` extra. It runs on the build host (`CHOST`); only its output
    /// targets `CTARGET`. Its own compile identity is the host's, so its
    /// toolchain vars are `${CHOST}-<tool>`.
    CrossToolHost {
        /// The `cross-<tuple>` triple parsed from the category.
        triple: String,
    },
    /// A `cross-<tuple>/` package that is a **native target library populating
    /// the sysroot** — the libc (glibc/musl) and kernel-headers. It is
    /// `CTARGET` code itself (toolchain vars `${CTARGET}-<tool>`), installed
    /// into the sysroot for the target.
    ///
    /// Mirrors bash crossdev's `set_env` `case ${l} in K|L)` (target) vs `*)`
    /// (host). The libc name varies per target, so this name set is an
    /// approximation of crossdev's planning letter; the principled source is
    /// `CrossTarget::packages()`'s `PackageArch` (a future follow-up routes
    /// the refinement through that, at the depgraph layer).
    CrossToolTarget {
        /// The `cross-<tuple>` triple parsed from the category.
        triple: String,
    },
}

impl BuildClass {
    /// Derive the build class from a plan entry's identity and the solve's
    /// cross context.
    ///
    /// A `cross-<tuple>/` category holds crossdev's toolchain, split the same
    /// way bash crossdev's `set_env` does (`K|L` vs `*`): the libc
    /// (glibc/musl) and kernel-headers are the **target** packages
    /// ([`CrossToolTarget`](Self::CrossToolTarget) — they are `CTARGET` code);
    /// everything else — binutils/gcc/gdb/wrappers and every `--ex-pkg` extra
    /// — defaults to **host** ([`CrossToolHost`](Self::CrossToolHost) — host
    /// binaries generating target code). Otherwise [`MergeRoot`] plus the
    /// cross flags pick between native-host, native-target, and
    /// foreign-arch-target.
    pub fn classify(
        cpn: &Cpn,
        merge_root: MergeRoot,
        cross_active: bool,
        is_cross_arch: bool,
    ) -> Self {
        if let Some(triple) = cpn.category.as_str().strip_prefix("cross-") {
            return match cpn.package.as_str() {
                "linux-headers" | "glibc" | "musl" => Self::CrossToolTarget {
                    triple: triple.to_string(),
                },
                _ => Self::CrossToolHost {
                    triple: triple.to_string(),
                },
            };
        }
        match merge_root {
            MergeRoot::Host => Self::NativeHost,
            MergeRoot::Target if cross_active && is_cross_arch => {
                Self::CrossTarget { triple: None }
            }
            MergeRoot::Target => Self::NativeTarget,
        }
    }
}

/// Stable token form for crossing the `__worker` subprocess boundary
/// (`--build-class=<token>`) and, later, the bashrc shell seam
/// (`EM_BUILD_CLASS`). Round-trips with `FromStr`. Triples contain no `:`,
/// so `tag:triple` is unambiguous.
impl std::fmt::Display for BuildClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NativeHost => f.write_str("native-host"),
            Self::NativeTarget => f.write_str("native-target"),
            Self::CrossTarget { triple: None } => f.write_str("cross-target"),
            Self::CrossTarget { triple: Some(t) } => write!(f, "cross-target:{t}"),
            Self::CrossToolHost { triple } => write!(f, "cross-tool-host:{triple}"),
            Self::CrossToolTarget { triple } => write!(f, "cross-tool-target:{triple}"),
        }
    }
}

impl std::str::FromStr for BuildClass {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "native-host" => return Ok(Self::NativeHost),
            "native-target" => return Ok(Self::NativeTarget),
            "cross-target" => return Ok(Self::CrossTarget { triple: None }),
            _ => {}
        }
        let (tag, triple) = s
            .split_once(':')
            .ok_or_else(|| format!("bad build-class token {s:?} (expected tag or tag:triple)"))?;
        let triple = triple.to_string();
        match tag {
            "cross-target" => Ok(Self::CrossTarget {
                triple: Some(triple),
            }),
            "cross-tool-host" => Ok(Self::CrossToolHost { triple }),
            "cross-tool-target" => Ok(Self::CrossToolTarget { triple }),
            other => Err(format!("bad build-class tag {other:?}")),
        }
    }
}

/// A resolved package in a plan: identity + selected version.
///
/// This is the solver-agnostic counterpart of pubgrub's
/// `(PortagePackage, Version)` solution entry, with the virtual
/// (`UseDecision`/`Choice`/`SlotChoice`) nodes stripped — only real packages
/// appear.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct SelectedPackage {
    /// Category/package name.
    pub cpn: Cpn,
    /// Selected version.
    pub version: Version,
    /// Bound slot, if the package is slotted.
    pub slot: Option<Interned<DefaultInterner>>,
    /// Merge destination.
    pub merge_root: MergeRoot,
    /// What kind of build this entry is (host-class vs target-class, native
    /// vs cross). Computed once at selection; see [`BuildClass`].
    pub build_class: BuildClass,
}

impl SelectedPackage {
    /// Create a target-root selected package.
    ///
    /// Defaults to [`BuildClass::NativeTarget`] — callers resolving into a
    /// cross or host context should construct the struct directly (or use
    /// [`BuildClass::classify`]) so the field reflects the real topology.
    pub fn new(cpn: Cpn, version: Version, slot: Option<Interned<DefaultInterner>>) -> Self {
        Self {
            cpn,
            version,
            slot,
            merge_root: MergeRoot::Target,
            build_class: BuildClass::NativeTarget,
        }
    }
}

impl std::fmt::Display for SelectedPackage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.slot, self.merge_root) {
            (Some(slot), MergeRoot::Target) => write!(f, "{}-{}:{}", self.cpn, self.version, slot),
            (Some(slot), MergeRoot::Host) => {
                write!(f, "{}-{}:{}@host", self.cpn, self.version, slot)
            }
            (None, MergeRoot::Target) => write!(f, "{}-{}", self.cpn, self.version),
            (None, MergeRoot::Host) => write!(f, "{}-{}@host", self.cpn, self.version),
        }
    }
}

/// A labeled dependency edge in the plan graph.
///
/// Solver-agnostic counterpart of pubgrub's `DepEdge`, keyed on
/// [`SelectedPackage`] rather than solver-internal package IDs.
#[derive(Clone, Debug)]
pub struct DepEdge {
    /// The package that declares the dependency.
    pub from: SelectedPackage,
    /// The package that is depended upon.
    pub to: SelectedPackage,
    /// Which dependency class this edge belongs to.
    pub class: crate::DepClass,
    /// The USE flag in `from` that gates this dep, if it was inside
    /// `flag? ( dep )`.
    pub via_use_flag: Option<Interned<DefaultInterner>>,
}

/// Policy for how the solver treats an installed package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstalledPolicy {
    /// Solver prefers this version but may choose a different one.
    Favor,
    /// Solver MUST keep this exact version; solve fails if impossible.
    Lock,
    /// Treat as a rebuild source (native `--emptytree`): the installed version
    /// is not favoured, the full deep closure is expanded.
    Rebuild,
}

/// A package currently installed on the system, fed to the solver before
/// resolve so it can prefer (or pin) installed versions and compute action
/// tags / rebuilds.
#[derive(Clone, Debug)]
pub struct InstalledPackage {
    /// Category/package name.
    pub cpn: Cpn,
    /// Installed version.
    pub version: Version,
    /// Bound slot, if slotted.
    pub slot: Option<Interned<DefaultInterner>>,
    /// How the solver treats this installed package.
    pub policy: InstalledPolicy,
    /// Active USE flags on the installed instance.
    pub active_use: Vec<Interned<DefaultInterner>>,
    /// IUSE flags declared by the installed instance.
    pub iuse: Vec<Interned<DefaultInterner>>,
    /// Blocker atoms (`!foo`/`!!foo`) this installed instance declares, so the
    /// solver can report a conflict if the plan would co-install a blocked
    /// package. Empty for most packages.
    pub blockers: Vec<crate::Dep>,
}

/// A resolve target, in already-resolved form (the consumer's slot/version
/// pinning, e.g. keyword/mask-aware best-slot selection, is done before this).
///
/// Each [`crate::Solver::resolve_targets`] call solves all targets in one joint
/// solve over a synthetic root.
#[derive(Clone, Debug)]
pub struct TargetSpec {
    /// Category/package name.
    pub cpn: Cpn,
    /// Bound slot the target pins, if any.
    pub slot: Option<Interned<DefaultInterner>>,
    /// Version operator for `version`, or `None` for "any".
    pub op: Option<Operator>,
    /// Version operand for `op`, or `None` for "any".
    pub version: Option<Version>,
    /// Whether `version` is a `=*` glob (only meaningful with
    /// [`Operator::Equal`]).
    pub glob: bool,
}

impl TargetSpec {
    /// Any version of `cpn` (optionally in `slot`).
    pub fn any_in(cpn: Cpn, slot: Option<Interned<DefaultInterner>>) -> Self {
        Self {
            cpn,
            slot,
            op: None,
            version: None,
            glob: false,
        }
    }
}

/// A dependency the solver had to drop because no candidate satisfied it in the
/// reachable closure (e.g. an atom referencing a package absent from the
/// repository). Reported for diagnostics; the plan is still produced.
#[derive(Clone, Debug)]
pub struct DroppedDep {
    /// CPN of the dropped dependency.
    pub cpn: Cpn,
}

/// A USE flag the solver was ceded (Level-C `REQUIRED_USE`) and the value it
/// picked, for display as autounmask-style output.
#[derive(Clone, Debug)]
pub struct CededFlag {
    /// Package whose flag was ceded.
    pub cpn: Cpn,
    /// The ceded flag.
    pub flag: Interned<DefaultInterner>,
    /// Value the solver chose.
    pub value: bool,
    /// `true` if this differs from the caller's configured value.
    pub flipped: bool,
}

/// A per-target USE-flag requirement the solve derived (the "needed" set),
/// surfaced as autounmask `package.use` suggestions.
#[derive(Clone, Debug)]
pub struct UseFlagRequirement {
    /// Package whose flags are constrained.
    pub cpn: Cpn,
    /// Version the constraint applies to.
    pub version: Version,
    /// If the post-solve fixpoint upgraded the version, the upgraded target.
    pub upgrade_to: Option<Version>,
    /// Flags that must be enabled.
    pub required_enabled: Vec<Interned<DefaultInterner>>,
    /// Flags that must be disabled.
    pub required_disabled: Vec<Interned<DefaultInterner>>,
    /// CPNs of the packages driving this requirement.
    pub required_by: Vec<String>,
}

/// A post-solve advisory violation (reported after the plan, as portage does).
#[derive(Clone, Debug, Error)]
pub enum Violation {
    /// A blocker (`!foo` / `!!foo`) conflict.
    #[error("{strength} blocker conflict: {pkg} blocks {blocker}")]
    Blocker {
        /// The package declaring the blocker.
        pkg: String,
        /// The blocker atom string.
        blocker: String,
        /// `"weak"` for `!`, `"strong"` for `!!`.
        strength: &'static str,
    },
    /// A USE-dep constraint (`[flag]` etc.) was violated.
    #[error("USE-dep conflict: {0}: {1}")]
    UseDep(String, String),
    /// A `::repo` constraint was violated.
    #[error("repo constraint conflict: {0}: {1}")]
    Repo(String, String),
}

/// The full result of a resolve: the plan plus the solver's advisory output,
/// in solver-agnostic vocabulary. Returned owned by
/// [`crate::Solver::resolve_targets`] so a consumer reads one value without the
/// solver having to cache per-accessor state.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    /// Selected real packages (virtual/decision nodes stripped), in no
    /// guaranteed order.
    pub selected: Vec<SelectedPackage>,
    /// The labelled dependency graph (edges with both endpoints selected).
    pub graph: Vec<DepEdge>,
    /// The selected packages in topological install order: a dependency is
    /// merged before the package that needs it. Cycles are broken on soft
    /// (RDEPEND) edges, falling back to a deterministic tie-break on genuine
    /// build-time cycles.
    pub install_order: Vec<SelectedPackage>,
    /// Dependencies the solver had to drop (no satisfying candidate in the
    /// reachable closure). Reported for diagnostics.
    pub dropped_deps: Vec<DroppedDep>,
    /// USE flags the solver was ceded (Level-C `REQUIRED_USE`) and the values
    /// it picked. Empty when nothing was ceded.
    pub ceded_flags: Vec<CededFlag>,
    /// Per-target USE-flag requirements the solve derived (the "needed" set),
    /// surfaced as autounmask `package.use` suggestions.
    pub use_flag_requirements: Vec<UseFlagRequirement>,
    /// Post-solve advisory violations (blockers, USE-deps, `::repo`), reported
    /// after the plan as portage does. Empty when the solution is clean.
    pub violations: Vec<Violation>,
}

/// Error returned by [`crate::Solver::resolve_targets`].
#[derive(Debug, Error)]
pub enum SolveError {
    /// The target set has no satisfying solution. The string carries a
    /// solver-specific human-readable derivation/report.
    #[error("no solution: {0}")]
    NoSolution(String),
    /// The provider could not satisfy the request for another reason.
    #[error("{0}")]
    Provider(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cn(cat: &str, pkg: &str) -> Cpn {
        Cpn::new(cat, pkg)
    }

    #[test]
    fn cross_host_tool_defaults_host_sysroot_lib_is_target() {
        // bash crossdev's `set_env` `case ${l} in K|L)` (target) vs `*)`
        // (host): a `cross-<tuple>/` package is HOST unless it's the libc
        // (glibc/musl) or kernel-headers. So gcc (and any `--ex-pkg` extra)
        // is a host code-generator; glibc is a target sysroot library.
        let tuple = "riscv64-unknown-linux-gnu";
        let gcc = cn("cross-riscv64-unknown-linux-gnu", "gcc");
        let gdb = cn("cross-riscv64-unknown-linux-gnu", "gdb"); // an --ex-pkg extra
        let glibc = cn("cross-riscv64-unknown-linux-gnu", "glibc");
        let headers = cn("cross-riscv64-unknown-linux-gnu", "linux-headers");
        for cpn in [gcc, gdb] {
            assert_eq!(
                BuildClass::classify(&cpn, MergeRoot::Host, false, false),
                BuildClass::CrossToolHost {
                    triple: tuple.to_string(),
                }
            );
            // Category wins over merge_root/cross flags for the host tools.
            assert_eq!(
                BuildClass::classify(&cpn, MergeRoot::Target, true, true),
                BuildClass::CrossToolHost {
                    triple: tuple.to_string(),
                }
            );
        }
        for cpn in [glibc, headers] {
            assert_eq!(
                BuildClass::classify(&cpn, MergeRoot::Target, true, true),
                BuildClass::CrossToolTarget {
                    triple: tuple.to_string(),
                }
            );
        }
    }

    #[test]
    fn native_target_with_no_cross() {
        let cpn = cn("dev-libs", "foo");
        assert_eq!(
            BuildClass::classify(&cpn, MergeRoot::Target, false, false),
            BuildClass::NativeTarget
        );
    }

    #[test]
    fn merge_root_host_is_native_host() {
        let cpn = cn("dev-libs", "foo");
        assert_eq!(
            BuildClass::classify(&cpn, MergeRoot::Host, false, false),
            BuildClass::NativeHost
        );
        // Even under an active cross context: a Host-rooted ordinary package
        // is a native host build (an unsatisfied BDEPEND), not a cross tool.
        assert_eq!(
            BuildClass::classify(&cpn, MergeRoot::Host, true, true),
            BuildClass::NativeHost
        );
    }

    #[test]
    fn build_class_display_fromstr_round_trips() {
        // Every variant round-trips Display -> FromStr.
        let cases = [
            BuildClass::NativeHost,
            BuildClass::NativeTarget,
            BuildClass::CrossTarget { triple: None },
            BuildClass::CrossTarget {
                triple: Some("riscv64-unknown-linux-gnu".to_string()),
            },
            BuildClass::CrossToolHost {
                triple: "riscv64-unknown-linux-gnu".to_string(),
            },
            BuildClass::CrossToolTarget {
                triple: "aarch64-unknown-linux-gnu".to_string(),
            },
        ];
        for c in cases {
            let s = c.to_string();
            assert_eq!(
                s.parse::<BuildClass>().unwrap(),
                c,
                "round-trip {c:?} via {s:?}"
            );
        }
    }

    #[test]
    fn build_class_fromstr_rejects_garbage() {
        assert!("".parse::<BuildClass>().is_err());
        // A triple-bearing tag with no triple is malformed.
        assert!("cross-tool-host".parse::<BuildClass>().is_err());
        assert!("cross-tool-target".parse::<BuildClass>().is_err());
        // Unknown tag.
        assert!(
            "bogus:riscv64-unknown-linux-gnu"
                .parse::<BuildClass>()
                .is_err()
        );
    }

    #[test]
    fn cross_arch_target_is_cross_target_with_deferred_triple() {
        let cpn = cn("sys-apps", "foo");
        // Real foreign-arch build: cross_active AND is_cross_arch both on.
        assert_eq!(
            BuildClass::classify(&cpn, MergeRoot::Target, true, true),
            BuildClass::CrossTarget { triple: None }
        );
        // Same-arch offset (`--root <dir>`) has cross_active on but
        // is_cross_arch off — it stays NativeTarget, the routing that
        // `broot_filtered` already handles.
        assert_eq!(
            BuildClass::classify(&cpn, MergeRoot::Target, true, false),
            BuildClass::NativeTarget
        );
    }
}
