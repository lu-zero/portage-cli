//! Effective per-package USE after profile/env overrides and IUSE defaults

use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, DepEntry, Version};
use portage_atom_pubgrub::{CededFlag, Glob, IUseDefault, PortagePackage, UseConfig, UseFlagState};
use portage_metadata::{CacheEntry, EbuildMetadata, IUseDefault as MetaIUseDefault};

use crate::force_mask::{ForceMask, IuseInjection, iuse_effective_set};
use crate::repo::{self, RepoData, ResolvePolicy};

/// Re-apply ceded (`--autosolve-use`) flag decisions on a resolved `UseConfig`
///
/// Like `use.force`/`use.mask`, ceded flags must win over an env-level `-*`
/// that caused the `REQUIRED_USE` violation — not be stored as wipeable
/// `package.use` entries.
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
/// entry's parsed `IUSE` list (`+flag`/`-flag` → enabled/disabled default)
fn iuse_defaults(cache: &CacheEntry) -> HashMap<Interned<DefaultInterner>, IUseDefault> {
    iuse_defaults_from_metadata(&cache.metadata)
}

pub(crate) fn iuse_defaults_from_metadata(
    meta: &EbuildMetadata,
) -> HashMap<Interned<DefaultInterner>, IUseDefault> {
    meta.iuse
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

/// Enabled-only IUSE defaults for solver fact ingestion.
///
/// Disabled defaults are implicit in `UseConfig` lookups and must not be
/// materialized as solver facts.
pub(crate) fn enabled_iuse_defaults_from_metadata(
    meta: &EbuildMetadata,
) -> HashMap<Interned<DefaultInterner>, IUseDefault> {
    meta.iuse
        .iter()
        .filter_map(|iuse| {
            (iuse.default == Some(MetaIUseDefault::Enabled))
                .then_some((iuse.into(), IUseDefault::Enabled))
        })
        .collect()
}

/// Expand the prefix globs the fold left standing over the package's IUSE; force/mask apply after
fn expand_globs(cfg: &mut UseConfig, iuse: &HashSet<Interned<DefaultInterner>>) {
    let globs = cfg.globs().to_vec();
    for &flag in iuse {
        let matching = || {
            globs
                .iter()
                .filter(|(p, _)| flag.as_str().starts_with(p.as_str()))
        };
        // A USE_EXPAND assignment's literal '*' is expanded after the whole USE fold, as Portage
        // does. Otherwise use the last matching prefix rule only for a flag not set explicitly.
        if matching().any(|(_, g)| *g == Glob::Literal) {
            cfg.enable(flag);
        } else if cfg.get_opt(flag).is_none() {
            match matching().next_back().map(|(_, g)| *g) {
                Some(Glob::Expand) => cfg.enable(flag),
                Some(Glob::Clear) => cfg.disable(flag),
                _ => {}
            }
        }
    }
}

/// Apply profile force/mask as the unconditional post-fold step
///
/// Portage's `use.force`/`use.mask` outside the USE_ORDER stack.
///
/// Must run **after** `resolve_effective_use` and **before** [`apply_ceded`] so
/// env-level `-*` cannot wipe forced flags (and ceded decisions still win
/// last).
fn apply_force_mask(
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

/// `IUSE_EFFECTIVE` as interned flags for force/mask filtering
pub fn iuse_set(
    cache: &CacheEntry,
    injection: &IuseInjection,
) -> HashSet<Interned<DefaultInterner>> {
    iuse_set_from_metadata(&cache.metadata, injection)
}

fn iuse_set_from_metadata(
    meta: &EbuildMetadata,
    injection: &IuseInjection,
) -> HashSet<Interned<DefaultInterner>> {
    iuse_effective_set(meta.eapi, meta.iuse.iter().map(Interned::from), injection)
}

fn fold_effective_use(
    policy: &ResolvePolicy,
    cpv: &Cpv,
    slot: Option<Interned<DefaultInterner>>,
    iuse_defaults: &HashMap<Interned<DefaultInterner>, IUseDefault>,
    iuse: &HashSet<Interned<DefaultInterner>>,
    stable: bool,
) -> UseConfig {
    let mut cfg = portage_atom_pubgrub::resolve_effective_use(
        iuse_defaults,
        policy.defaults,
        cpv,
        slot,
        policy.package_use,
        policy.env_use,
        policy.profile_package_use,
        policy.conf,
    );
    expand_globs(&mut cfg, iuse);
    let slot_dep = slot.map(portage_atom::Slot::from_name);
    apply_force_mask(
        &mut cfg,
        policy.force_mask,
        cpv,
        slot_dep.as_ref(),
        stable,
        iuse,
    );
    cfg
}

/// Effective USE for parsed metadata, used by acceptance checks that do not
/// have a solver [`PortagePackage`].
pub(crate) fn effective_use_metadata(
    policy: &ResolvePolicy,
    cpv: &Cpv,
    meta: &EbuildMetadata,
    slot: Option<Interned<DefaultInterner>>,
) -> UseConfig {
    let iuse_defaults = iuse_defaults_from_metadata(meta);
    let (iuse, stable) = if policy.force_mask.is_empty() {
        (HashSet::new(), false)
    } else {
        (
            iuse_set_from_metadata(meta, &policy.force_mask.iuse_injection),
            policy.accept_keywords.is_stable(&meta.keywords, cpv, slot),
        )
    };
    fold_effective_use(policy, cpv, slot, &iuse_defaults, &iuse, stable)
}

/// Effective USE for a CPV whose repository metadata is unavailable
pub fn effective_use_without_metadata(
    policy: &ResolvePolicy,
    cpv: &Cpv,
    slot: Option<Interned<DefaultInterner>>,
) -> UseConfig {
    fold_effective_use(policy, cpv, slot, &HashMap::new(), &HashSet::new(), false)
}

/// The flags `use.force`/`use.mask` pin for a version, which `-p` shows in parentheses
pub fn pinned_flags(
    policy: &ResolvePolicy,
    pkg: &PortagePackage,
    ver: &Version,
    cache: &CacheEntry,
    stable: bool,
) -> HashSet<Interned<DefaultInterner>> {
    if policy.force_mask.is_empty() {
        return HashSet::new();
    }
    let cpv = Cpv::new(*pkg.cpn(), ver.clone());
    let slot = pkg.slot().map(portage_atom::Slot::from_name);
    let iuse = iuse_set(cache, &policy.force_mask.iuse_injection);
    let (forced, masked) = policy
        .force_mask
        .effective(&cpv, slot.as_ref(), stable, &iuse);
    forced.union(&masked).copied().collect()
}

/// Package-use pins only, for the cede gate's "user pinned this" check.
pub(crate) fn pinned_package_use(
    policy: &ResolvePolicy,
    cpv: &Cpv,
    slot: Option<Interned<DefaultInterner>>,
) -> UseConfig {
    let empty = portage_atom_pubgrub::UseLayer::default();
    portage_atom_pubgrub::resolve_effective_use(
        &HashMap::new(),
        &empty,
        cpv,
        slot,
        policy.package_use,
        &empty,
        policy.profile_package_use,
        &empty,
    )
}

/// The full effective USE fold for one `(pkg, ver)`: IUSE defaults, profile and conf
/// layers, `package_use`, `env_use`, then profile force/mask, then any
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
    let mut cfg = policy
        .facts
        .map(|facts| facts.effective(*policy, pkg, ver, cache))
        .unwrap_or_else(|| effective_use_base(policy, pkg, ver, cache, stable));
    apply_ceded(&mut cfg, *pkg.cpn(), ceded);
    cfg
}

/// Build the pre-cede effective USE fold for one `(pkg, ver)`.
///
/// Keeping this separate lets policy-fact caching stop at the ceding boundary;
/// each caller applies its own solver decisions after the shared base.
pub(crate) fn effective_use_base(
    policy: &ResolvePolicy,
    pkg: &PortagePackage,
    ver: &Version,
    cache: &CacheEntry,
    stable: bool,
) -> UseConfig {
    let cpv = Cpv::new(*pkg.cpn(), ver.clone());
    let iuse_map = iuse_defaults(cache);
    let iuse = iuse_set(cache, &policy.force_mask.iuse_injection);
    fold_effective_use(policy, &cpv, pkg.slot(), &iuse_map, &iuse, stable)
}

/// A `(pkg, ver)`'s cache entry plus its effective USE
///
/// Each dep class is evaluated against that USE on demand — the
/// `find_cache` + [`effective_use`] + `DepEntry::evaluate_use` triple
/// shared by `root_closure`, `bdepend_trim`, and `depend_trim`.
///
/// `None` when the CPV isn't in `data` at all (a within-run merge whose cache
/// entry vanished, e.g. across a repo reload — every caller already treats this
/// as "skip").
pub struct EvaluatedDeps<'a> {
    cache: &'a CacheEntry,
    effective: UseConfig,
    depend: OnceCell<Vec<DepEntry>>,
    bdepend: OnceCell<Vec<DepEntry>>,
    rdepend: OnceCell<Vec<DepEntry>>,
    pdepend: OnceCell<Vec<DepEntry>>,
    idepend: OnceCell<Vec<DepEntry>>,
}

/// One USE-evaluated dep-class accessor per PMS dep class; each just picks
/// the field and evaluates it once for the lifetime of this value.
impl EvaluatedDeps<'_> {
    fn eval<'cell>(
        &self,
        deps: &[DepEntry],
        cell: &'cell OnceCell<Vec<DepEntry>>,
    ) -> &'cell [DepEntry] {
        cell.get_or_init(|| {
            DepEntry::evaluate_use_groups(
                deps,
                &self.effective,
                self.cache.metadata.eapi.empty_any_of_matches(),
            )
        })
    }

    /// `DEPEND` edges
    pub fn depend(&self) -> &[DepEntry] {
        self.eval(self.cache.metadata.depend.list(), &self.depend)
    }

    /// `BDEPEND` edges
    pub fn bdepend(&self) -> &[DepEntry] {
        self.eval(self.cache.metadata.bdepend.list(), &self.bdepend)
    }

    /// `RDEPEND` edges
    pub fn rdepend(&self) -> &[DepEntry] {
        self.eval(self.cache.metadata.rdepend.list(), &self.rdepend)
    }

    /// `PDEPEND` edges
    pub fn pdepend(&self) -> &[DepEntry] {
        self.eval(self.cache.metadata.pdepend.list(), &self.pdepend)
    }

    /// `IDEPEND` edges
    pub fn idepend(&self) -> &[DepEntry] {
        self.eval(self.cache.metadata.idepend.list(), &self.idepend)
    }
}

/// Look up `(pkg, ver)`'s cache entry and compute its [`EvaluatedDeps`] in one
/// step; `None` when the cpv has no cache entry (see [`EvaluatedDeps`]'s doc
/// comment)
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
    Some(EvaluatedDeps {
        cache,
        effective,
        depend: OnceCell::new(),
        bdepend: OnceCell::new(),
        rdepend: OnceCell::new(),
        pdepend: OnceCell::new(),
        idepend: OnceCell::new(),
    })
}

/// Per-pass cache for effective USE and evaluated dependency classes.
pub(crate) struct EvaluatedDepsCache<'a> {
    data: &'a RepoData,
    policy: ResolvePolicy<'a>,
    stable: bool,
    entries: HashMap<(PortagePackage, Version), Option<EvaluatedDeps<'a>>>,
}

impl<'a> EvaluatedDepsCache<'a> {
    pub(crate) fn new(data: &'a RepoData, policy: ResolvePolicy<'a>, stable: bool) -> Self {
        Self {
            data,
            policy,
            stable,
            entries: HashMap::new(),
        }
    }

    pub(crate) fn get(
        &mut self,
        pkg: &PortagePackage,
        ver: &Version,
    ) -> Option<&EvaluatedDeps<'a>> {
        let key = (pkg.clone(), ver.clone());
        if !self.entries.contains_key(&key) {
            let value = evaluated_deps(self.data, &self.policy, pkg, ver, self.stable);
            self.entries.insert(key.clone(), value);
        }
        self.entries.get(&key).and_then(Option::as_ref)
    }

    /// Fill the cache and return one evaluated entry per plan position.
    pub(crate) fn snapshot(
        &mut self,
        order: &[(PortagePackage, Version)],
    ) -> Vec<Option<&EvaluatedDeps<'a>>> {
        for (pkg, ver) in order {
            let _ = self.get(pkg, ver);
        }
        order
            .iter()
            .map(|(pkg, ver)| {
                self.entries
                    .get(&(pkg.clone(), ver.clone()))
                    .and_then(Option::as_ref)
            })
            .collect()
    }
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
            &UseLayer::default(),
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
            &UseLayer::default(),
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

    // Critical: force must survive env-level `-*`, same shape as the ceded fix
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
            &UseLayer::default(),
        );
        assert!(matches!(
            cfg.get(Interned::intern("multilib")),
            UseFlagState::Disabled
        ));

        let fm = ForceMask::one(crate::force_mask::ForceMaskLayer {
            use_force: crate::force_mask::fold_signed(vec!["multilib".into()]),
            ..Default::default()
        });
        let iuse: HashSet<_> = [Interned::intern("multilib")].into_iter().collect();
        apply_force_mask(&mut cfg, &fm, &cpv, None, false, &iuse);

        assert!(matches!(
            cfg.get(Interned::intern("multilib")),
            UseFlagState::Enabled
        ));
    }

    // All prefixes ending in '_' are accepted user globs, not just whole USE_EXPAND groups.
    #[test]
    fn nested_prefix_globs_obey_token_order() {
        use portage_atom_pubgrub::UseLayer;
        let cpv = Cpv::parse("app-misc/test-1").unwrap();
        let flags: HashSet<_> = ["cpu_flags_x", "cpu_other"]
            .into_iter()
            .map(Interned::intern)
            .collect();
        for (tokens, want) in [
            ("cpu_* -cpu_flags_*", false),
            ("cpu_* -cpu_flags_* cpu_*", true),
            ("cpu_* -cpu_flags_* cpu_flags_x", true),
            ("cpu_* -cpu_flags_* cpu_flags_* -cpu_flags_x", false),
        ] {
            for which in 0..3 {
                let empty = UseLayer::empty();
                let layer = UseLayer::parse(tokens);
                let pkg = vec![(
                    portage_atom::Dep::parse("app-misc/test").unwrap(),
                    tokens
                        .split_whitespace()
                        .filter_map(gentoo_core::UseToken::user)
                        .collect(),
                )];
                let mut cfg = portage_atom_pubgrub::resolve_effective_use(
                    &HashMap::new(),
                    &empty,
                    &cpv,
                    None,
                    if which == 2 { &pkg } else { &[] },
                    if which == 1 { &layer } else { &empty },
                    &[],
                    if which == 0 { &layer } else { &empty },
                );
                expand_globs(&mut cfg, &flags);
                assert_eq!(
                    cfg.get(Interned::intern("cpu_flags_x")),
                    if want {
                        UseFlagState::Enabled
                    } else {
                        UseFlagState::Disabled
                    },
                    "{tokens}, layer {which}"
                );
                assert_eq!(
                    cfg.get(Interned::intern("cpu_other")),
                    UseFlagState::Enabled
                );
            }
        }
    }

    #[test]
    fn deferred_prefix_fold_matches_an_eager_iuse_fold() {
        use gentoo_core::UseToken;
        use portage_atom_pubgrub::UseLayer;
        let flags: HashSet<_> = ["cpu_flags_x", "cpu_other", "ssl"]
            .into_iter()
            .map(Interned::intern)
            .collect();
        let cpv = Cpv::parse("app-misc/test-1").unwrap();
        let vocab = [
            "cpu_*",
            "cpu_flags_*",
            "-cpu_*",
            "-cpu_flags_*",
            "-*",
            "cpu_flags_x",
            "-cpu_flags_x",
            "cpu_other",
            "-cpu_other",
            "ssl",
            "-ssl",
        ];
        let mut seed = 7u64;
        for _ in 0..1000 {
            let mut layers = Vec::new();
            for _ in 0..3 {
                let mut tokens = Vec::new();
                for _ in 0..6 {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    tokens
                        .push(UseToken::user(vocab[(seed >> 32) as usize % vocab.len()]).unwrap());
                }
                layers.push(tokens);
            }
            let mut expected: HashMap<_, _> =
                flags.iter().map(|&f| (f, f.as_str() == "ssl")).collect();
            for &token in layers.iter().flatten() {
                match token {
                    UseToken::Flag { flag, enable } => {
                        expected.insert(flag, enable);
                    }
                    UseToken::ClearAll => expected.values_mut().for_each(|v| *v = false),
                    UseToken::ClearPrefix(prefix) | UseToken::ExpandPrefix(prefix) => {
                        let on = matches!(token, UseToken::ExpandPrefix(_));
                        for (flag, value) in &mut expected {
                            if flag.as_str().starts_with(prefix.as_str()) {
                                *value = on;
                            }
                        }
                    }
                    _ => unreachable!(),
                }
            }
            let conf = UseLayer::new(layers[0].iter().copied());
            let env = UseLayer::new(layers[2].iter().copied());
            let pu = [(
                portage_atom::Dep::parse("app-misc/test").unwrap(),
                layers[1].clone(),
            )];
            let defaults = HashMap::from([(Interned::intern("ssl"), IUseDefault::Enabled)]);
            let mut cfg = portage_atom_pubgrub::resolve_effective_use(
                &defaults,
                &UseLayer::empty(),
                &cpv,
                None,
                &pu,
                &env,
                &[],
                &conf,
            );
            expand_globs(&mut cfg, &flags);
            for (&flag, &on) in &expected {
                assert_eq!(
                    cfg.get(flag),
                    if on {
                        UseFlagState::Enabled
                    } else {
                        UseFlagState::Disabled
                    },
                    "flag {flag}, layers {layers:?}"
                );
            }
        }
    }
}
