//! Profile USE *force* and *mask*, resolved per package.
//!
//! Global `use.force`/`use.mask` are already folded into the base `UseConfig`
//! by `portage-repo`'s `resolve_use_flags`, so they are kept here only for the
//! Level-C cede gate (a globally pinned flag must never be ceded). The
//! **package-level** sets (`package.use.force`/`package.use.mask`) and all the
//! **`*.stable.*`** variants are otherwise applied *nowhere*; this module layers
//! them onto a package's effective USE.
//!
//! The `.stable.*` files only influence a package "merged due to a stable
//! keyword" (portage(5)); [`is_stable`] reproduces Portage's
//! `KeywordsManager.isStable`. On a `~arch` `ACCEPT_KEYWORDS` every package is
//! merged via an unstable keyword, so the stable sets never apply there — they
//! matter on pure-stable systems.
//!
//! This is what makes cross-compilation targets resolve correctly: crossdev
//! ships `/etc/portage/profile/package.use.{force,mask}/cross-*` pinning
//! `multilib`/`cet`/`nopie`, which only take effect once package-level force/mask
//! are applied to effective USE.

use std::collections::{BTreeSet, HashMap};

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, Dep};
use portage_atom_pubgrub::UseConfig;

use super::repo::mask_matches;

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
pub(super) type PkgRules = HashMap<Cpn, Vec<(Dep, Vec<ForceTok>)>>;

/// Resolved profile force/mask policy, flags interned once at config-read time.
/// The globals are already `-`-resolved (`merge_use_flags`); the per-atom sets
/// keep the removal bit so a `-flag` (unforce/unmask) is resolved against the
/// accumulated set per package.
#[derive(Default)]
pub(super) struct ForceMask {
    pub use_force: Vec<Flag>,
    pub use_mask: Vec<Flag>,
    pub use_stable_force: Vec<Flag>,
    pub use_stable_mask: Vec<Flag>,
    pub pkg_force: PkgRules,
    pub pkg_mask: PkgRules,
    pub pkg_stable_force: PkgRules,
    pub pkg_stable_mask: PkgRules,
}

/// Group flat per-atom entries by `Cpn` (see [`PkgRules`]), parsing each token
/// to interned form (`-flag` → removal) once.
pub(super) fn index_by_cpn(entries: Vec<(Dep, Vec<String>)>) -> PkgRules {
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

/// Accumulate per-atom tokens matching `cpv` into `set`, honouring `-flag`
/// removal (incremental, in list order).
fn accumulate(rules: &PkgRules, cpv: &Cpv, set: &mut BTreeSet<Flag>) {
    let Some(entries) = rules.get(&cpv.cpn) else {
        return;
    };
    for (dep, toks) in entries {
        if !mask_matches(dep, cpv) {
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
    /// The net forced and masked flag names for `cpv` that are **not** already in
    /// the base config — i.e. package-level force/mask always, plus the
    /// `*.stable.*` sets when `stable`. Global non-stable `use.force`/`use.mask`
    /// are excluded (they live in the base config). Mask wins over force.
    pub(super) fn effective(&self, cpv: &Cpv, stable: bool) -> (BTreeSet<Flag>, BTreeSet<Flag>) {
        let mut forced = BTreeSet::new();
        let mut masked = BTreeSet::new();
        // Global `use.mask` must force a flag *off* per package, overriding the
        // ebuild's IUSE default — `config` only excludes masked flags from the
        // globally-enabled set, which a `+flag` IUSE default (e.g.
        // `llvm-runtimes/compiler-rt-sanitizers`'s `+abi_x86_32`, masked on
        // arm64 by `arch/base/use.mask`) would otherwise re-enable. Added before
        // the per-package rules so `package.use.mask -flag` can unmask it.
        masked.extend(self.use_mask.iter().copied());
        accumulate(&self.pkg_force, cpv, &mut forced);
        accumulate(&self.pkg_mask, cpv, &mut masked);
        if stable {
            forced.extend(self.use_stable_force.iter().copied());
            accumulate(&self.pkg_stable_force, cpv, &mut forced);
            masked.extend(self.use_stable_mask.iter().copied());
            accumulate(&self.pkg_stable_mask, cpv, &mut masked);
        }
        forced.retain(|f| !masked.contains(f));
        (forced, masked)
    }

    /// Apply force/mask to a package's effective USE: enable forced flags, then
    /// disable masked ones (mask wins). Overrides `package.use` and the
    /// configured value, matching Portage. Flags are already interned.
    pub(super) fn apply(&self, cfg: &mut UseConfig, cpv: &Cpv, stable: bool) {
        let (forced, masked) = self.effective(cpv, stable);
        for &f in &forced {
            cfg.enable(f);
        }
        for &f in &masked {
            cfg.disable(f);
        }
    }

    /// Every flag pinned for `cpv` (global force/mask + the package-level and
    /// stable sets) — the Level-C cede gate must never cede any of these.
    pub(super) fn pins(&self, cpv: &Cpv, stable: bool) -> BTreeSet<Flag> {
        let (mut pins, masked) = self.effective(cpv, stable);
        pins.extend(masked);
        pins.extend(self.use_force.iter().copied());
        pins.extend(self.use_mask.iter().copied());
        pins
    }

    /// Whether `cpv` carries any force/mask entries at all (cheap skip for the
    /// common no-policy case).
    pub(super) fn is_empty(&self) -> bool {
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
        let (forced, masked) = fm.effective(&c, false);
        assert!(forced.contains(&flag("multilib")));
        assert!(
            !forced.contains(&flag("shared")),
            "shared is masked → dropped from force"
        );
        assert!(masked.contains(&flag("cet")));
        assert!(masked.contains(&flag("shared")));

        let mut cfg = UseConfig::new();
        cfg.enable(Interned::intern("cet")); // user tried to enable a masked flag
        fm.apply(&mut cfg, &c, false);
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
        let (forced, _) = fm.effective(&cpv("cross-foo/gcc-13.2"), false);
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
            !fm.effective(&c, false).1.contains(&flag("risky")),
            "ignored when unstable"
        );
        assert!(
            fm.effective(&c, true).1.contains(&flag("risky")),
            "applied when stable"
        );
    }
}
