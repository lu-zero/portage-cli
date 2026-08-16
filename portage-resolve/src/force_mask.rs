//! Profile USE *force* and *mask*, resolved per package.
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

/// An interned USE flag.
type Flag = Interned<DefaultInterner>;

/// A parsed per-atom force/mask token: the interned flag and whether it is the
/// incremental removal form (`-flag`, i.e. unforce/unmask).
type ForceTok = (Flag, bool);

/// Per-atom force/mask entries grouped by `Cpn`. The profile chain contributes
/// hundreds of `package.use.{force,mask}` atoms; grouping by `Cpn` turns a
/// package's lookup into O(1) (a miss costs nothing) instead of a scan over the
/// whole list for every package the solver evaluates. Per-`Cpn` insertion order
/// is preserved so the incremental `-flag` (unforce/unmask) resolution is exact.
pub type PkgRules = HashMap<Cpn, Vec<(Dep, Vec<ForceTok>)>>;

/// Resolved profile force/mask policy, flags interned once at config-read time.
/// The globals are already `-`-resolved (`merge_use_flags`); the per-atom sets
/// keep the removal bit so a `-flag` (unforce/unmask) is resolved against the
/// accumulated set per package.
#[derive(Default)]
pub struct ForceMask {
    /// Global `use.force` flags.
    pub use_force: Vec<Flag>,
    /// Global `use.mask` flags.
    pub use_mask: Vec<Flag>,
    /// Global `use.stable.force` flags (only applied when merging on a stable
    /// keyword).
    pub use_stable_force: Vec<Flag>,
    /// Global `use.stable.mask` flags (only applied when merging on a stable
    /// keyword).
    pub use_stable_mask: Vec<Flag>,
    /// Per-package `package.use.force` entries.
    pub pkg_force: PkgRules,
    /// Per-package `package.use.mask` entries.
    pub pkg_mask: PkgRules,
    /// Per-package `package.use.stable.force` entries.
    pub pkg_stable_force: PkgRules,
    /// Per-package `package.use.stable.mask` entries.
    pub pkg_stable_mask: PkgRules,
}

/// Group flat per-atom entries by `Cpn` (see [`PkgRules`]), parsing each token
/// to interned form (`-flag` → removal) once.
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

/// Accumulate per-atom tokens matching `cpv` (+ optional slot) into `set`,
/// honouring `-flag` removal (incremental, in list order). Uses
/// [`Dep::matches_cpv`] so version **and** `:slot` constraints on
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
    /// The net forced and masked flag names for `cpv` — global `use.force`/
    /// `use.mask` (both, as of 2026-07-12; see below) plus package-level
    /// force/mask always, plus the `*.stable.*` sets when `stable`. Mask wins
    /// over force. Portage applies force/mask strictly *after* the rest of
    /// USE resolution (profile/`make.conf`/`package.use`/environment,
    /// `portage_solver::resolve_effective_use`'s fold) — this is that final
    /// step, not folded into any earlier layer.
    ///
    /// `iuse` restricts which global `use.force`/`use.mask` flags are even
    /// considered: a forced/masked flag the package doesn't declare in `IUSE`
    /// can never affect that package (there's no `+flag` IUSE default to
    /// resurrect, and forcing on a flag outside `IUSE` is a no-op — the
    /// package's own dependencies can't reference it), so it costs nothing to
    /// skip. Global `use.force`/`use.mask` commonly have hundreds of entries
    /// while a package's own IUSE is a few dozen at most, so this turns a
    /// per-package O(global set) cost into O(package IUSE ∩ global set) — the
    /// dominant cost of `apply()` before this filter, since it ran for every
    /// version the solver instantiated.
    pub fn effective(
        &self,
        cpv: &Cpv,
        slot: Option<&portage_atom::Slot>,
        stable: bool,
        iuse: &HashSet<Flag>,
    ) -> (BTreeSet<Flag>, BTreeSet<Flag>) {
        let mut forced = BTreeSet::new();
        let mut masked = BTreeSet::new();
        // Global `use.force`/`use.mask` are no longer folded into any earlier
        // layer (`resolve_effective_use`'s `pre_env` doesn't carry them —
        // unlike the pre-2026-07-12 collapsed `config`), so both are applied
        // here unconditionally, restricted to the package's own IUSE (see
        // above). `use.mask` in particular must force a flag *off* per
        // package, overriding the ebuild's IUSE default — e.g.
        // `llvm-runtimes/compiler-rt-sanitizers`'s `+abi_x86_32`, masked on
        // arm64 by `arch/base/use.mask`, would otherwise be re-enabled by the
        // fold. Added before the per-package rules so `package.use.mask
        // -flag` can unmask it.
        forced.extend(self.use_force.iter().copied().filter(|f| iuse.contains(f)));
        masked.extend(self.use_mask.iter().copied().filter(|f| iuse.contains(f)));
        accumulate(&self.pkg_force, cpv, slot, &mut forced);
        accumulate(&self.pkg_mask, cpv, slot, &mut masked);
        if stable {
            forced.extend(self.use_stable_force.iter().copied());
            accumulate(&self.pkg_stable_force, cpv, slot, &mut forced);
            masked.extend(self.use_stable_mask.iter().copied());
            accumulate(&self.pkg_stable_mask, cpv, slot, &mut masked);
        }
        forced.retain(|f| !masked.contains(f));
        (forced, masked)
    }

    /// Apply force/mask to a package's effective USE: enable forced flags, then
    /// disable masked ones (mask wins). Overrides `package.use` and the
    /// configured value, matching Portage. Flags are already interned.
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
    /// stable sets) — the Level-C cede gate must never cede any of these.
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
        pins.extend(self.use_force.iter().copied());
        pins.extend(self.use_mask.iter().copied());
        pins
    }

    /// Whether `cpv` carries any force/mask entries at all (cheap skip for the
    /// common no-policy case).
    pub fn is_empty(&self) -> bool {
        self.use_force.is_empty()
            && self.use_mask.is_empty()
            && self.use_stable_force.is_empty()
            && self.use_stable_mask.is_empty()
            && self.pkg_force.is_empty()
            && self.pkg_mask.is_empty()
            && self.pkg_stable_force.is_empty()
            && self.pkg_stable_mask.is_empty()
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
        let fm = ForceMask {
            pkg_force: index_by_cpn(vec![(
                dep("cross-foo/gcc"),
                vec!["multilib".into(), "shared".into()],
            )]),
            pkg_mask: index_by_cpn(vec![(
                dep("cross-foo/gcc"),
                vec!["cet".into(), "shared".into()],
            )]),
            ..Default::default()
        };
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
        let fm = ForceMask {
            // parent forces multilib, leaf unforces it for this atom
            pkg_force: index_by_cpn(vec![
                (dep("cross-foo/gcc"), vec!["multilib".into()]),
                (dep("cross-foo/gcc"), vec!["-multilib".into()]),
            ]),
            ..Default::default()
        };
        let (forced, _) = fm.effective(&cpv("cross-foo/gcc-13.2"), None, false, &HashSet::new());
        assert!(!forced.contains(&flag("multilib")), "-multilib unforced it");
    }

    #[test]
    fn stable_sets_only_apply_when_stable() {
        let fm = ForceMask {
            use_stable_mask: vec!["risky".into()],
            ..Default::default()
        };
        let c = cpv("dev-libs/foo-1");
        assert!(
            !fm.effective(&c, None, false, &HashSet::new())
                .1
                .contains(&flag("risky")),
            "ignored when unstable"
        );
        assert!(
            fm.effective(&c, None, true, &HashSet::new())
                .1
                .contains(&flag("risky")),
            "applied when stable"
        );
    }

    #[test]
    fn package_force_respects_slot_constraint() {
        let fm = ForceMask {
            pkg_force: index_by_cpn(vec![(dep("sys-devel/gcc:13"), vec!["openmp".into()])]),
            ..Default::default()
        };
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
        let fm = ForceMask {
            use_mask: vec!["abi_x86_32".into(), "unrelated_flag".into()],
            ..Default::default()
        };
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
        let fm = ForceMask {
            use_force: vec!["abi_x86_32".into(), "unrelated_flag".into()],
            ..Default::default()
        };
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
}
