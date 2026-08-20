//! Profile USE *force* and *mask*, resolved per package
//!
//! Real portage applies `use.force`/`use.mask` as unconditional post-filters
//! *outside* the 8-layer USE fold (`config.py`'s `_getUseMask`/`_getUseForce`,
//! applied after `pkg`/`env`) — this module is that final step for `em`, for
//! both the global (`use.force`/`use.mask`) and **package-level**
//! (`package.use.force`/`package.use.mask`) sets, plus all the **`*.stable.*`**
//! variants. None of these are folded into `resolve_effective_use`'s `pre_env`/
//! `package_use`/`env_use` layers; `effective()`/`apply()` here are what
//! layers them onto a package's already-resolved USE.
//!
//! The `.stable.*` files only influence a package "merged due to a stable
//! keyword" (portage(5)); the caller passes that `stable` decision in (Portage's
//! `KeywordsManager.isStable`). On a `~arch` `ACCEPT_KEYWORDS` every package is
//! merged via an unstable keyword, so the stable sets never apply there — they
//! matter on pure-stable systems.
//!
//! This is what makes cross-compilation targets resolve correctly: crossdev
//! ships `/etc/portage/profile/package.use.{force,mask}/cross-*` pinning
//! `multilib`/`cet`/`nopie`, which only take effect once package-level force/mask
//! are applied to effective USE.

use std::collections::{BTreeSet, HashMap, HashSet};

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, Dep};
use portage_atom_pubgrub::UseConfig;
use portage_metadata::Eapi;

/// An interned USE flag
pub type Flag = Interned<DefaultInterner>;

/// A parsed per-atom force/mask token: the interned flag and whether it is the
/// incremental removal form (`-flag`, i.e. unforce/unmask)
pub type ForceTok = (Flag, bool);

/// Per-atom force/mask entries grouped by `Cpn`
///
/// The profile chain contributes hundreds of `package.use.{force,mask}` atoms;
/// grouping by `Cpn` turns a package's lookup into O(1) (a miss costs nothing)
/// instead of a scan over the whole list for every package the solver
/// evaluates. Per-`Cpn` insertion order is preserved so the incremental `-flag`
/// (unforce/unmask) resolution is exact.
pub type PkgRules = HashMap<Cpn, Vec<(Dep, Vec<ForceTok>)>>;

/// One profile node's force/mask files, tokens still signed (`-flag` unforce/unmask)
///
/// Portage `getUseMask`/`getUseForce` walk the profile chain and incremental-
/// fold **per node**: `use.mask[i]`, then `use.stable.mask[i]` (if stable), then
/// matching `package.use.mask[i]`, then matching `package.use.stable.mask[i]`.
/// A later node's `use.mask: -sysprof` therefore unmasks an earlier node's
/// `package.use.stable.mask: cat/pkg sysprof` — pre-merging all `use.mask`
/// first, then adding `package.use.stable.mask`, cannot see that unmask.
#[derive(Default)]
pub struct ForceMaskLayer {
    /// This node's `use.force` (`-flag` = unforce)
    pub use_force: Vec<ForceTok>,
    /// This node's `use.mask` (`-flag` = unmask)
    pub use_mask: Vec<ForceTok>,
    /// This node's `use.stable.force`
    pub use_stable_force: Vec<ForceTok>,
    /// This node's `use.stable.mask`
    pub use_stable_mask: Vec<ForceTok>,
    /// This node's `package.use.force`
    pub pkg_force: PkgRules,
    /// This node's `package.use.mask`
    pub pkg_mask: PkgRules,
    /// This node's `package.use.stable.force`
    pub pkg_stable_force: PkgRules,
    /// This node's `package.use.stable.mask`
    pub pkg_stable_mask: PkgRules,
}

/// Profile inputs for `IUSE_EFFECTIVE` (PMS 11.1.1)
///
/// Shared across packages; the per-package IUSE and EAPI come from the cache.
#[derive(Debug, Clone, Default)]
pub struct IuseInjection {
    /// Profile `IUSE_IMPLICIT`
    pub iuse_implicit: Vec<String>,
    /// Profile `USE_EXPAND`
    pub use_expand: Vec<String>,
    /// Profile `USE_EXPAND_IMPLICIT`
    pub use_expand_implicit: Vec<String>,
    /// Profile `USE_EXPAND_UNPREFIXED`
    pub use_expand_unprefixed: Vec<String>,
    /// `USE_EXPAND_VALUES_${v}` for each expand name
    pub expand_values: HashMap<String, Vec<String>>,
}

/// Resolved profile force/mask policy, interned once at config-read time
///
/// Layers are parent-first, matching [`ProfileStack::profiles`].
#[derive(Default)]
pub struct ForceMask {
    /// Profile-chain nodes, ancestors first
    pub layers: Vec<ForceMaskLayer>,
    /// Profile IUSE injection used to build `IUSE_EFFECTIVE` for the global
    /// `use.force`/`use.mask` filter (Algorithm 5.1)
    pub iuse_injection: IuseInjection,
}

impl ForceMask {
    /// A single-layer policy (tests, or a stack of one node)
    pub fn one(layer: ForceMaskLayer) -> Self {
        Self {
            layers: vec![layer],
            ..Default::default()
        }
    }
}

/// `IUSE_EFFECTIVE` interned for force/mask filtering
pub fn iuse_effective_set(
    eapi: Eapi,
    iuse: impl IntoIterator<Item = impl AsRef<str>>,
    injection: &IuseInjection,
) -> HashSet<Flag> {
    portage_metadata::iuse_effective(
        eapi,
        iuse,
        &injection.iuse_implicit,
        &injection.use_expand,
        &injection.use_expand_implicit,
        &injection.use_expand_unprefixed,
        &injection.expand_values,
    )
    .into_iter()
    .map(|s| Interned::intern(&s))
    .collect()
}

/// Parse `use.mask`/`use.force` lines to interned signed tokens
pub fn signed_flags(flags: Vec<String>) -> Vec<ForceTok> {
    flags
        .iter()
        .map(|f| match f.strip_prefix('-') {
            Some(name) => (Interned::intern(name), true),
            None => (Interned::intern(f), false),
        })
        .collect()
}

/// Group flat per-atom entries by `Cpn` (see [`PkgRules`]), parsing each token
/// to interned form (`-flag` → removal) once
pub fn index_by_cpn(entries: Vec<(Dep, Vec<String>)>) -> PkgRules {
    let mut map = PkgRules::new();
    for (dep, flags) in entries {
        let toks: Vec<ForceTok> = flags
            .iter()
            .map(|f| match f.strip_prefix('-') {
                Some(name) => (Interned::intern(name), true),
                None => (Interned::intern(f), false),
            })
            .collect();
        map.entry(dep.cpn).or_default().push((dep, toks));
    }
    map
}

/// Accumulate per-atom tokens matching `cpv` (+ optional slot) into `set`
///
/// Honours `-flag` removal (incremental, in list order).
///
/// Uses [`Dep::matches_cpv`] so version **and** `:slot` constraints on
/// `package.use.force`/`package.use.mask` atoms are honoured (mask_matches
/// alone is slot-blind).
fn accumulate(
    rules: &PkgRules,
    cpv: &Cpv,
    slot: Option<&portage_atom::Slot>,
    set: &mut BTreeSet<Flag>,
) {
    let Some(entries) = rules.get(&cpv.cpn) else {
        return;
    };
    for (dep, toks) in entries {
        if !dep.matches_cpv(cpv, slot) {
            continue;
        }
        for &(flag, remove) in toks {
            if remove {
                set.remove(&flag);
            } else {
                set.insert(flag);
            }
        }
    }
}

impl ForceMask {
    /// The net forced and masked flag names for `cpv`
    ///
    /// Global `use.force`/`use.mask` plus package-level force/mask always,
    /// plus the `*.stable.*` sets when `stable`.
    ///
    /// Mask wins over force. Applied strictly *after* the rest of USE resolution
    /// (profile/`make.conf`/ `package.use`/environment) — this is that final step,
    /// not folded into any earlier layer.
    ///
    /// `iuse` restricts which global flags are even considered: one the
    /// package doesn't declare in `IUSE` can never affect it, so it costs
    /// nothing to skip. Global sets commonly have hundreds of entries while
    /// a package's own IUSE is a few dozen at most, turning a per-package
    /// O(global set) cost into O(package IUSE ∩ global set) — the dominant
    /// cost of `apply()` before this filter.
    pub fn effective(
        &self,
        cpv: &Cpv,
        slot: Option<&portage_atom::Slot>,
        stable: bool,
        iuse: &HashSet<Flag>,
    ) -> (BTreeSet<Flag>, BTreeSet<Flag>) {
        let mut forced = BTreeSet::new();
        let mut masked = BTreeSet::new();
        // Same per-node order as Portage `UseManager.getUseMask`/`getUseForce`.
        for layer in &self.layers {
            apply_signed(&layer.use_force, &mut forced, Some(iuse));
            apply_signed(&layer.use_mask, &mut masked, Some(iuse));
            if stable {
                apply_signed(&layer.use_stable_force, &mut forced, Some(iuse));
                apply_signed(&layer.use_stable_mask, &mut masked, Some(iuse));
            }
            accumulate(&layer.pkg_force, cpv, slot, &mut forced);
            accumulate(&layer.pkg_mask, cpv, slot, &mut masked);
            if stable {
                accumulate(&layer.pkg_stable_force, cpv, slot, &mut forced);
                accumulate(&layer.pkg_stable_mask, cpv, slot, &mut masked);
            }
        }
        forced.retain(|f| !masked.contains(f));
        (forced, masked)
    }

    /// Apply force/mask to a package's effective USE
    ///
    /// Enables forced flags, then disables masked ones (mask wins).
    ///
    /// Overrides `package.use` and the configured value, matching Portage. Flags
    /// are already interned.
    ///
    /// `iuse` is the package's own declared `IUSE` flags — see [`Self::effective`].
    /// `slot` is the package's main slot (for `:slot`-scoped force/mask atoms).
    pub fn apply(
        &self,
        cfg: &mut UseConfig,
        cpv: &Cpv,
        slot: Option<&portage_atom::Slot>,
        stable: bool,
        iuse: &HashSet<Flag>,
    ) {
        let (forced, masked) = self.effective(cpv, slot, stable, iuse);
        for &f in &forced {
            cfg.enable(f);
        }
        for &f in &masked {
            cfg.disable(f);
        }
    }

    /// Every flag pinned for `cpv` (global force/mask + the package-level and
    /// stable sets) — the Level-C cede gate must never cede any of these
    ///
    /// Unlike [`Self::apply`], this always includes the *full* global
    /// `use.force`/`use.mask` sets regardless of `iuse` — a flag outside the
    /// package's `IUSE` can never be ceded anyway (the caller in
    /// `cede_required_use` already skips any flag not in `IUSE`), so the
    /// `iuse` restriction on [`Self::effective`]'s internal call here is a
    /// no-op for the final result, just cheaper to compute.
    pub fn pins(
        &self,
        cpv: &Cpv,
        slot: Option<&portage_atom::Slot>,
        stable: bool,
        iuse: &HashSet<Flag>,
    ) -> BTreeSet<Flag> {
        let (mut pins, masked) = self.effective(cpv, slot, stable, iuse);
        pins.extend(masked);
        // Globals without the IUSE filter: a flag outside IUSE can never be
        // ceded, but the gate still wants the full global set. Incremental
        // `-flag` is resolved (a later `use.mask: -sysprof` is not a pin).
        let mut gforce = BTreeSet::new();
        let mut gmask = BTreeSet::new();
        for layer in &self.layers {
            apply_signed(&layer.use_force, &mut gforce, None);
            apply_signed(&layer.use_mask, &mut gmask, None);
        }
        pins.extend(gforce);
        pins.extend(gmask);
        pins
    }

    /// Whether `cpv` carries any force/mask entries at all (cheap skip for the
    /// common no-policy case)
    pub fn is_empty(&self) -> bool {
        self.layers.iter().all(|l| {
            l.use_force.is_empty()
                && l.use_mask.is_empty()
                && l.use_stable_force.is_empty()
                && l.use_stable_mask.is_empty()
                && l.pkg_force.is_empty()
                && l.pkg_mask.is_empty()
                && l.pkg_stable_force.is_empty()
                && l.pkg_stable_mask.is_empty()
        })
    }
}

fn apply_signed(toks: &[ForceTok], set: &mut BTreeSet<Flag>, iuse: Option<&HashSet<Flag>>) {
    for &(flag, remove) in toks {
        if let Some(iuse) = iuse
            && !iuse.contains(&flag)
        {
            continue;
        }
        if remove {
            set.remove(&flag);
        } else {
            set.insert(flag);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use portage_atom::Version;
    use portage_atom_pubgrub::UseFlagState;

    fn cpv(s: &str) -> Cpv {
        let (cpn, ver) = s.rsplit_once('-').unwrap();
        Cpv::new(
            portage_atom::Cpn::parse(cpn).unwrap(),
            Version::parse(ver).unwrap(),
        )
    }

    fn dep(s: &str) -> Dep {
        Dep::parse(s).unwrap()
    }

    fn flag(s: &str) -> Flag {
        Interned::intern(s)
    }

    fn iuse_of(names: &[&str]) -> HashSet<Flag> {
        names.iter().map(|n| flag(n)).collect()
    }

    #[test]
    fn package_force_and_mask_apply_with_mask_winning() {
        let fm = ForceMask::one(ForceMaskLayer {
            pkg_force: index_by_cpn(vec![(
                dep("cross-foo/gcc"),
                vec!["multilib".into(), "shared".into()],
            )]),
            pkg_mask: index_by_cpn(vec![(
                dep("cross-foo/gcc"),
                vec!["cet".into(), "shared".into()],
            )]),
            ..Default::default()
        });
        let c = cpv("cross-foo/gcc-13.2");
        let iuse = iuse_of(&["multilib", "shared", "cet"]);
        let (forced, masked) = fm.effective(&c, None, false, &iuse);
        assert!(forced.contains(&flag("multilib")));
        assert!(
            !forced.contains(&flag("shared")),
            "shared is masked → dropped from force"
        );
        assert!(masked.contains(&flag("cet")));
        assert!(masked.contains(&flag("shared")));

        let mut cfg = UseConfig::new();
        cfg.enable(Interned::intern("cet")); // user tried to enable a masked flag
        fm.apply(&mut cfg, &c, None, false, &iuse);
        assert_eq!(cfg.get(Interned::intern("multilib")), UseFlagState::Enabled);
        assert_eq!(cfg.get(Interned::intern("cet")), UseFlagState::Disabled);
        assert_eq!(cfg.get(Interned::intern("shared")), UseFlagState::Disabled);
    }

    #[test]
    fn unforce_token_removes_from_set() {
        let fm = ForceMask::one(ForceMaskLayer {
            // parent forces multilib, leaf unforces it for this atom
            pkg_force: index_by_cpn(vec![
                (dep("cross-foo/gcc"), vec!["multilib".into()]),
                (dep("cross-foo/gcc"), vec!["-multilib".into()]),
            ]),
            ..Default::default()
        });
        let (forced, _) = fm.effective(&cpv("cross-foo/gcc-13.2"), None, false, &HashSet::new());
        assert!(!forced.contains(&flag("multilib")), "-multilib unforced it");
    }

    #[test]
    fn stable_sets_only_apply_when_stable() {
        let fm = ForceMask::one(ForceMaskLayer {
            use_stable_mask: vec![(flag("risky"), false)],
            ..Default::default()
        });
        let c = cpv("dev-libs/foo-1");
        let iuse = iuse_of(&["risky"]);
        assert!(
            !fm.effective(&c, None, false, &iuse)
                .1
                .contains(&flag("risky")),
            "ignored when unstable"
        );
        assert!(
            fm.effective(&c, None, true, &iuse)
                .1
                .contains(&flag("risky")),
            "applied when stable"
        );
    }

    #[test]
    fn package_force_respects_slot_constraint() {
        let fm = ForceMask::one(ForceMaskLayer {
            pkg_force: index_by_cpn(vec![(dep("sys-devel/gcc:13"), vec!["openmp".into()])]),
            ..Default::default()
        });
        let c = cpv("sys-devel/gcc-13.2.0");
        let iuse = iuse_of(&["openmp"]);
        let (forced_match, _) =
            fm.effective(&c, Some(&portage_atom::Slot::new("13")), false, &iuse);
        assert!(forced_match.contains(&flag("openmp")));
        let (forced_other, _) =
            fm.effective(&c, Some(&portage_atom::Slot::new("14")), false, &iuse);
        assert!(
            !forced_other.contains(&flag("openmp")),
            "slot-scoped force must not apply to a different slot"
        );
    }

    #[test]
    fn global_use_mask_only_applies_to_packages_own_iuse() {
        // Global use.mask commonly has hundreds of entries; a package that
        // doesn't declare a masked flag in its own IUSE can never have it
        // resurrected by a `+flag` IUSE default, so it must not appear in
        // `effective()`'s masked set (the whole point of the `iuse` filter).
        let fm = ForceMask::one(ForceMaskLayer {
            use_mask: vec![(flag("abi_x86_32"), false), (flag("unrelated_flag"), false)],
            ..Default::default()
        });
        let c = cpv("dev-libs/foo-1");
        let iuse = iuse_of(&["abi_x86_32"]);
        let (_, masked) = fm.effective(&c, None, false, &iuse);
        assert!(
            masked.contains(&flag("abi_x86_32")),
            "flag in the package's IUSE stays masked"
        );
        assert!(
            !masked.contains(&flag("unrelated_flag")),
            "flag absent from the package's IUSE is filtered out"
        );

        // pins() must still protect the *full* global set regardless — a
        // flag outside IUSE can't be ceded in the first place, so this is
        // just cheaper to compute, not narrower in its final result.
        let pins = fm.pins(&c, None, false, &iuse);
        assert!(pins.contains(&flag("abi_x86_32")));
        assert!(pins.contains(&flag("unrelated_flag")));
    }

    // Regression for the gap found migrating off the collapsed `UseConfig`
    // (`use-config-duplicate-fallback-logic`): global `use.force` used to be
    // excluded from `effective()` on the assumption it was already baked into
    // the base config by `resolve_use_flags`. Since `resolve_effective_use`'s
    // `pre_env` no longer carries it, `effective()` must apply it directly —
    // mirrors `global_use_mask_only_applies_to_packages_own_iuse` for force.
    #[test]
    fn global_use_force_only_applies_to_packages_own_iuse() {
        let fm = ForceMask::one(ForceMaskLayer {
            use_force: vec![(flag("abi_x86_32"), false), (flag("unrelated_flag"), false)],
            ..Default::default()
        });
        let c = cpv("dev-libs/foo-1");
        let iuse = iuse_of(&["abi_x86_32"]);
        let (forced, _) = fm.effective(&c, None, false, &iuse);
        assert!(
            forced.contains(&flag("abi_x86_32")),
            "flag in the package's IUSE is forced on"
        );
        assert!(
            !forced.contains(&flag("unrelated_flag")),
            "flag absent from the package's IUSE is filtered out"
        );

        let mut cfg = UseConfig::new();
        cfg.disable(Interned::intern("abi_x86_32")); // user tried to disable a forced flag
        fm.apply(&mut cfg, &c, None, false, &iuse);
        assert_eq!(
            cfg.get(Interned::intern("abi_x86_32")),
            UseFlagState::Enabled,
            "use.force overrides an explicit disable"
        );
    }

    // `profiles/base/package.use.stable.mask: dev-libs/glib sysprof` then
    // `arch/amd64/use.mask: -sysprof`. Portage's per-node stack_lists lets the
    // later unmask cancel the earlier per-package stable mask.
    #[test]
    fn later_use_mask_unmask_cancels_earlier_package_stable_mask() {
        let glib = cpv("dev-libs/glib-2.88.2");
        let iuse = iuse_of(&["sysprof"]);
        let fm = ForceMask {
            layers: vec![
                ForceMaskLayer {
                    pkg_stable_mask: index_by_cpn(vec![(
                        dep("dev-libs/glib"),
                        vec!["sysprof".into()],
                    )]),
                    ..Default::default()
                },
                ForceMaskLayer {
                    use_mask: vec![(flag("sysprof"), true)],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert!(
            !fm.effective(&glib, None, true, &iuse)
                .1
                .contains(&flag("sysprof")),
            "amd64 use.mask -sysprof unmasks glib's package.use.stable.mask"
        );
        let only_stable = ForceMask::one(ForceMaskLayer {
            pkg_stable_mask: index_by_cpn(vec![(dep("dev-libs/glib"), vec!["sysprof".into()])]),
            ..Default::default()
        });
        assert!(
            only_stable
                .effective(&glib, None, true, &iuse)
                .1
                .contains(&flag("sysprof")),
            "without a later unmask the stable mask still applies"
        );
        assert!(
            !only_stable
                .effective(&glib, None, false, &iuse)
                .1
                .contains(&flag("sysprof")),
            "unstable merges ignore package.use.stable.mask"
        );
    }

    #[test]
    fn iuse_effective_includes_profile_implicit() {
        let inj = IuseInjection {
            iuse_implicit: vec!["prefix".into()],
            ..Default::default()
        };
        let set = iuse_effective_set(portage_metadata::Eapi::Eight, ["ssl"], &inj);
        assert!(set.contains(&flag("ssl")));
        assert!(set.contains(&flag("prefix")));
    }
}
