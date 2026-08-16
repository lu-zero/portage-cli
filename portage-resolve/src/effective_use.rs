//! Effective per-package USE after profile/env overrides and IUSE defaults.

use std::collections::{HashMap, HashSet};

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, DepEntry, Version};
use portage_atom_pubgrub::{CededFlag, IUseDefault, PortagePackage, UseConfig, UseFlagState};
use portage_metadata::{CacheEntry, IUseDefault as MetaIUseDefault};

use crate::force_mask::ForceMask;
use crate::repo::{self, RepoData, ResolvePolicy};

/// Re-apply ceded (`--autosolve-use`) flag decisions on a resolved
/// `UseConfig`. Like `use.force`/`use.mask`, ceded flags must win over an
/// env-level `-*` that caused the `REQUIRED_USE` violation — not be stored
/// as wipeable `package.use` entries.
pub fn apply_ceded(cfg: &mut UseConfig, cpn: Cpn, ceded: &[CededFlag]) {
    for c in ceded.iter().filter(|c| c.cpn == cpn) {
        cfg.set(
            c.flag,
            if c.value {
                UseFlagState::Enabled
            } else {
                UseFlagState::Disabled
            },
        );
    }
}

/// Build the `iuse_defaults` map `resolve_effective_use` needs from a cache
/// entry's parsed `IUSE` list (`+flag`/`-flag` → enabled/disabled default).
pub fn iuse_defaults(cache: &CacheEntry) -> HashMap<Interned<DefaultInterner>, IUseDefault> {
    cache
        .metadata
        .iuse
        .iter()
        .filter_map(|iuse| {
            iuse.default.map(|def| {
                (
                    iuse.into(),
                    match def {
                        MetaIUseDefault::Enabled => IUseDefault::Enabled,
                        MetaIUseDefault::Disabled => IUseDefault::Disabled,
                    },
                )
            })
        })
        .collect()
}

/// Apply profile force/mask as the unconditional post-fold step (Portage's
/// `use.force`/`use.mask` outside the USE_ORDER stack). Must run **after**
/// `resolve_effective_use` and **before** [`apply_ceded`] so env-level `-*`
/// cannot wipe forced flags (and ceded decisions still win last).
pub fn apply_force_mask(
    cfg: &mut UseConfig,
    force_mask: &ForceMask,
    cpv: &Cpv,
    slot: Option<&portage_atom::Slot>,
    stable: bool,
    iuse: &HashSet<Interned<DefaultInterner>>,
) {
    if !force_mask.is_empty() {
        force_mask.apply(cfg, cpv, slot, stable, iuse);
    }
}

/// IUSE set as interned flags for force/mask filtering.
pub fn iuse_set(cache: &CacheEntry) -> HashSet<Interned<DefaultInterner>> {
    cache.metadata.iuse.iter().map(Interned::from).collect()
}

/// The full effective USE fold for one `(pkg, ver)`: IUSE defaults, `pre_env`,
/// `package_use`, `env_use`, then profile force/mask, then any
/// `--autosolve-use` ceded flags on top.
///
/// Force/mask is applied post-fold (not as synthetic `package.use`) so a
/// process-env `USE="-* …"` cannot clear forced flags — matching
/// `Adapter::desired_use` and real Portage.
pub fn effective_use(
    policy: &ResolvePolicy,
    pkg: &PortagePackage,
    ver: &Version,
    cache: &CacheEntry,
    stable: bool,
    ceded: &[CededFlag],
) -> UseConfig {
    let cpv = Cpv::new(*pkg.cpn(), ver.clone());
    let defaults = iuse_defaults(cache);
    let mut cfg = portage_atom_pubgrub::resolve_effective_use(
        &defaults,
        policy.pre_env,
        &cpv,
        pkg.slot(),
        policy.package_use,
        policy.env_use,
        policy.profile_package_use,
    ); // pre_env/env_use are already UseLayer
    let iuse = iuse_set(cache);
    let slot_key = pkg.slot().map(portage_atom::Slot::from_name);
    apply_force_mask(
        &mut cfg,
        policy.force_mask,
        &cpv,
        slot_key.as_ref(),
        stable,
        &iuse,
    );
    apply_ceded(&mut cfg, *pkg.cpn(), ceded);
    cfg
}

/// A `(pkg, ver)`'s cache entry plus its effective USE, with each dep class
/// evaluated against that USE on demand — the `find_cache` +
/// [`effective_use`] + `DepEntry::evaluate_use` triple shared by
/// `host_copies`, `bdepend_trim`, and `depend_trim`. `None` when the CPV
/// isn't in `data` at all (a within-run merge whose cache entry vanished,
/// e.g. across a repo reload — every caller already treats this as "skip").
pub struct EvaluatedDeps<'a> {
    cache: &'a CacheEntry,
    effective: UseConfig,
}

/// One USE-evaluated dep-class accessor per PMS dep class; each just picks
/// the field and hands it to `DepEntry::evaluate_use`.
impl EvaluatedDeps<'_> {
    /// `DEPEND` edges.
    pub fn depend(&self) -> Vec<DepEntry> {
        DepEntry::evaluate_use(&self.cache.metadata.depend, &self.effective)
    }

    /// `BDEPEND` edges.
    pub fn bdepend(&self) -> Vec<DepEntry> {
        DepEntry::evaluate_use(&self.cache.metadata.bdepend, &self.effective)
    }

    /// `RDEPEND` edges.
    pub fn rdepend(&self) -> Vec<DepEntry> {
        DepEntry::evaluate_use(&self.cache.metadata.rdepend, &self.effective)
    }

    /// `PDEPEND` edges.
    pub fn pdepend(&self) -> Vec<DepEntry> {
        DepEntry::evaluate_use(&self.cache.metadata.pdepend, &self.effective)
    }

    /// `IDEPEND` edges.
    pub fn idepend(&self) -> Vec<DepEntry> {
        DepEntry::evaluate_use(&self.cache.metadata.idepend, &self.effective)
    }
}

/// Look up `(pkg, ver)`'s cache entry and compute its [`EvaluatedDeps`] in one
/// step; `None` when the cpv has no cache entry (see [`EvaluatedDeps`]'s doc
/// comment).
pub fn evaluated_deps<'a>(
    data: &'a RepoData,
    policy: &ResolvePolicy,
    pkg: &PortagePackage,
    ver: &Version,
    stable: bool,
) -> Option<EvaluatedDeps<'a>> {
    let cache = repo::find_cache(data, pkg, ver)?;
    // Pre-/mid-solve utility (feeds the solver's own dependency-graph
    // construction) — the solver's ceded (`--autosolve-use`) decisions don't
    // exist yet at this point, so there is nothing to apply here.
    let effective = effective_use(policy, pkg, ver, cache, stable, &[]);
    Some(EvaluatedDeps { cache, effective })
}

#[cfg(test)]
mod tests {
    use portage_atom::Cpn;
    use portage_atom_pubgrub::{UseLayer, resolve_effective_use};

    use super::*;

    // Env-level `-*` would wipe a package.use-folded ceded flag;
    // `apply_ceded` must win regardless.
    #[test]
    fn apply_ceded_survives_an_env_level_wildcard_reset() {
        let cpv = Cpv::new(Cpn::new("app-alternatives", "lex"), "0-r1".parse().unwrap());
        let mut cfg = resolve_effective_use(
            &HashMap::new(),
            &UseLayer::default(),
            &cpv,
            None,
            &[],
            &UseLayer::parse("-* build"),
            &[],
        );
        assert!(
            matches!(cfg.get(Interned::intern("reflex")), UseFlagState::Disabled),
            "env-level -* must leave reflex off before ceding"
        );

        let ceded = vec![
            CededFlag {
                cpn: cpv.cpn,
                flag: Interned::intern("reflex"),
                value: true,
                flipped: true,
            },
            CededFlag {
                cpn: cpv.cpn,
                flag: Interned::intern("flex"),
                value: false,
                flipped: false,
            },
        ];
        apply_ceded(&mut cfg, cpv.cpn, &ceded);

        assert!(matches!(
            cfg.get(Interned::intern("reflex")),
            UseFlagState::Enabled
        ));
        assert!(matches!(
            cfg.get(Interned::intern("flex")),
            UseFlagState::Disabled
        ));
    }

    #[test]
    fn apply_ceded_ignores_flags_for_a_different_package() {
        let cpv = Cpv::new(Cpn::new("app-alternatives", "lex"), "0-r1".parse().unwrap());
        let mut cfg = resolve_effective_use(
            &HashMap::new(),
            &UseLayer::default(),
            &cpv,
            None,
            &[],
            &UseLayer::parse("-* build"),
            &[],
        );

        let ceded = vec![CededFlag {
            cpn: Cpn::new("app-alternatives", "awk"),
            flag: Interned::intern("gawk"),
            value: true,
            flipped: true,
        }];
        apply_ceded(&mut cfg, cpv.cpn, &ceded);

        assert!(matches!(
            cfg.get(Interned::intern("gawk")),
            UseFlagState::Disabled
        ));
    }

    /// Critical: force must survive env-level `-*`, same shape as the ceded fix.
    #[test]
    fn apply_force_mask_survives_an_env_level_wildcard_reset() {
        use crate::force_mask::ForceMask;

        let cpv = Cpv::new(Cpn::new("sys-devel", "gcc"), "14.2.0".parse().unwrap());
        let mut cfg = resolve_effective_use(
            &HashMap::new(),
            &UseLayer::default(),
            &cpv,
            None,
            &[],
            &UseLayer::parse("-*"),
            &[],
        );
        assert!(matches!(
            cfg.get(Interned::intern("multilib")),
            UseFlagState::Disabled
        ));

        let fm = ForceMask {
            use_force: vec![Interned::intern("multilib")],
            ..Default::default()
        };
        let iuse: HashSet<_> = [Interned::intern("multilib")].into_iter().collect();
        apply_force_mask(&mut cfg, &fm, &cpv, None, false, &iuse);

        assert!(matches!(
            cfg.get(Interned::intern("multilib")),
            UseFlagState::Enabled
        ));
    }
}
