use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use gentoo_core::Arch;
use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, Dep, Operator, Version};
use portage_atom_pubgrub::{
    DroppedDep, IUseDefault, PackageDeps, PackageRepository, PackageVersions, RequiredUse,
    UseOverride,
};
use portage_metadata::{CacheEntry, Keyword, LicenseExpr, RequiredUseExpr, Stability};
use portage_repo::{CacheReadOpts, Repository, cache_entries_parallel};

/// A reason a package version was excluded from the solver.
#[derive(Debug, Clone)]
pub(super) enum FilterReason {
    /// Needs a keyword the system doesn't accept (e.g. `~arm64`).
    Keyword(String),
    /// Masked by the profile or user `package.mask`.
    Masked,
    /// One or more licenses not in ACCEPT_LICENSE.
    License(Vec<String>),
}

/// A package version that was excluded and could resolve a dropped dep.
#[derive(Debug, Clone)]
pub(super) struct AutounmaskCandidate {
    pub cpv: Cpv,
    pub slot: Option<Interned<DefaultInterner>>,
    pub reasons: Vec<FilterReason>,
}

/// One parsed `ACCEPT_KEYWORDS` / `package.accept_keywords` token.
///
/// Tokens are parsed (and their arches interned) at config-read time, so the
/// solver never sees a keyword string — matching against a package's
/// [`Keyword`] list is a `u32` comparison with no per-check allocation.
#[derive(Clone, Copy)]
pub(super) enum AcceptToken {
    /// `arch` — accept the stable keyword for this arch.
    Stable(Interned<DefaultInterner>),
    /// `~arch` — accept the testing keyword (implies stable) for this arch.
    Testing(Interned<DefaultInterner>),
    /// `*` — accept a stable keyword for any arch.
    AnyStable,
    /// `~*` — accept a testing keyword for any arch (implies stable).
    AnyTesting,
    /// `**` — accept regardless of keywords (even an unkeyworded/live ebuild).
    Any,
}

impl AcceptToken {
    /// Parse one token. Returns `None` for tokens we don't model — the
    /// incremental `-arch` removal form, which the previous string matcher also
    /// silently ignored.
    pub(super) fn parse(tok: &str) -> Option<Self> {
        match tok {
            "**" => Some(Self::Any),
            "~*" => Some(Self::AnyTesting),
            "*" => Some(Self::AnyStable),
            _ if tok.starts_with('-') => None,
            _ => match tok.strip_prefix('~') {
                Some(arch) => Some(Self::Testing(Interned::intern(arch))),
                None => Some(Self::Stable(Interned::intern(tok))),
            },
        }
    }
}

/// The accept decision for a single (interned) arch, reduced to flags.
#[derive(Clone, Copy, Default)]
struct ArchAccept {
    /// A stable keyword for the arch is accepted.
    stable: bool,
    /// A testing (`~arch`) keyword is accepted (implies `stable`).
    testing: bool,
    /// `**` — accept even with no matching keyword.
    any: bool,
}

impl ArchAccept {
    /// Fold one token's contribution, relative to the target `arch`.
    fn add(&mut self, tok: AcceptToken, arch: Interned<DefaultInterner>) {
        match tok {
            // testing is a superset of stable, so accepting it implies stable.
            AcceptToken::Any => {
                self.any = true;
                self.testing = true;
                self.stable = true;
            }
            AcceptToken::AnyTesting => {
                self.testing = true;
                self.stable = true;
            }
            AcceptToken::AnyStable => self.stable = true,
            AcceptToken::Testing(a) if a == arch => {
                self.testing = true;
                self.stable = true;
            }
            AcceptToken::Stable(a) if a == arch => self.stable = true,
            _ => {}
        }
    }

    /// Whether a package with `keywords` is accepted under this decision.
    fn accepts(self, keywords: &[Keyword], arch: Interned<DefaultInterner>) -> bool {
        if self.any {
            return true;
        }
        keywords.iter().any(|kw| {
            kw.arch == arch
                && match kw.stability {
                    Stability::Stable => self.stable,
                    Stability::Testing => self.testing,
                    _ => false,
                }
        })
    }
}

/// Global `ACCEPT_KEYWORDS` plus per-package `package.accept_keywords`, parsed
/// into interned tokens once.
///
/// The global decision is precomputed for the host arch; per-package entries
/// are folded in per version only when present, so the common no-override path
/// is allocation-free and reduces to a keyword scan with `u32` comparisons.
pub(super) struct AcceptKeywords {
    /// Host arch, interned — the axis every acceptance check compares against.
    arch: Interned<DefaultInterner>,
    /// Precomputed decision from the global `ACCEPT_KEYWORDS` tokens.
    global: ArchAccept,
    /// Per-package overrides: `(atom, [tokens])`. A bare atom is pre-expanded to
    /// `~arch` (portage's rule). Empty ⇒ no per-package work in the hot path.
    per_package: Vec<(Dep, Vec<AcceptToken>)>,
}

impl AcceptKeywords {
    /// Build from pre-parsed tokens: the global `ACCEPT_KEYWORDS` list and the
    /// per-package `(atom, tokens)` entries from `package.accept_keywords`.
    /// Tokens are already interned (parsed at config-read time); a bare
    /// per-package atom arrives as an empty token list and is expanded here to
    /// "accept this arch's `~arch`" (portage's rule).
    pub(super) fn new(
        arch: &Arch,
        global: &[AcceptToken],
        per_package: Vec<(Dep, Vec<AcceptToken>)>,
    ) -> Self {
        let arch_key = Interned::intern(arch.as_str());
        let mut global_accept = ArchAccept::default();
        for &tok in global {
            global_accept.add(tok, arch_key);
        }
        // An empty global list means "stable only" (the historical fallback).
        if global.is_empty() {
            global_accept.stable = true;
        }
        let per_package = per_package
            .into_iter()
            .map(|(dep, mut toks)| {
                if toks.is_empty() {
                    // Bare atom ⇒ accept this arch's testing keyword.
                    toks.push(AcceptToken::Testing(arch_key));
                }
                (dep, toks)
            })
            .collect();
        Self {
            arch: arch_key,
            global: global_accept,
            per_package,
        }
    }

    /// Test-only constructor from a global token list (no per-package entries).
    #[cfg(test)]
    pub(super) fn from_global(arch: &Arch, global: &[&str]) -> Self {
        let toks: Vec<AcceptToken> = global
            .iter()
            .filter_map(|s| AcceptToken::parse(s))
            .collect();
        Self::new(arch, &toks, Vec::new())
    }

    /// The accept decision for one package version, folding any matching
    /// per-package overrides into the precomputed global decision. Returns the
    /// global decision unchanged (no work) in the common no-override case.
    fn decision(&self, cpv: &Cpv, slot: Option<Interned<DefaultInterner>>) -> ArchAccept {
        if self.per_package.is_empty() {
            return self.global;
        }
        let slot_str = slot.as_ref().map(|s| s.as_str());
        let mut acc = self.global;
        for (dep, toks) in &self.per_package {
            if dep.matches_cpv(cpv, slot_str) {
                for &t in toks {
                    acc.add(t, self.arch);
                }
            }
        }
        acc
    }

    /// Whether `keywords` is accepted for this version.
    pub(super) fn accepts(
        &self,
        keywords: &[Keyword],
        cpv: &Cpv,
        slot: Option<Interned<DefaultInterner>>,
    ) -> bool {
        self.decision(cpv, slot).accepts(keywords, self.arch)
    }

    /// Whether this version is merged on a *stable* basis (accepted, and not via
    /// a testing keyword) — gates the `use.stable.{force,mask}` sets.
    pub(super) fn is_stable(
        &self,
        keywords: &[Keyword],
        cpv: &Cpv,
        slot: Option<Interned<DefaultInterner>>,
    ) -> bool {
        let d = self.decision(cpv, slot);
        d.accepts(keywords, self.arch) && !d.testing
    }

    /// The `~arch` token an autounmask would need: the version is not accepted
    /// but carries a testing keyword for the host arch.
    pub(super) fn keyword_needed(
        &self,
        keywords: &[Keyword],
        cpv: &Cpv,
        slot: Option<Interned<DefaultInterner>>,
    ) -> Option<String> {
        if self.accepts(keywords, cpv, slot) {
            return None;
        }
        keywords
            .iter()
            .any(|kw| kw.arch == self.arch && kw.stability == Stability::Testing)
            .then(|| format!("~{}", self.arch.as_str()))
    }
}

/// Global `ACCEPT_LICENSE` plus per-package `package.license`.
///
/// The global list applies to every package; per-package entries extend it
/// (allow more, or `-deny`) for versions whose atom matches. The common
/// no-override path borrows the global decision — no allocation.
pub(super) struct AcceptLicenses {
    global: portage_repo::AcceptLicense,
    /// `(atom, overlay)` — each overlay is the per-line tokens parsed into an
    /// `AcceptLicense`, merged onto the global decision for matching versions.
    per_package: Vec<(Dep, portage_repo::AcceptLicense)>,
}

impl AcceptLicenses {
    pub(super) fn new(
        global: portage_repo::AcceptLicense,
        per_package: Vec<(Dep, portage_repo::AcceptLicense)>,
    ) -> Self {
        Self {
            global,
            per_package,
        }
    }

    /// The license-acceptance decision in effect for one package version.
    /// Returns the global decision *borrowed* when no per-package entry matches
    /// (the common case); otherwise a merged clone (global + matched overlays).
    fn effective_for(
        &self,
        cpv: &Cpv,
        slot: Option<Interned<DefaultInterner>>,
    ) -> Cow<'_, portage_repo::AcceptLicense> {
        if self.per_package.is_empty() {
            return Cow::Borrowed(&self.global);
        }
        let slot_str = slot.as_ref().map(|s| s.as_str());
        let mut merged: Option<portage_repo::AcceptLicense> = None;
        for (dep, overlay) in &self.per_package {
            if dep.matches_cpv(cpv, slot_str) {
                merged
                    .get_or_insert_with(|| self.global.clone())
                    .merge(overlay);
            }
        }
        merged.map_or(Cow::Borrowed(&self.global), Cow::Owned)
    }
}

/// Returns true if the license expression is fully covered by `accept`,
/// evaluating `use? ( … )` branches against `enabled` (a package's effective
/// USE). For an expression with no conditionals, `enabled` is never consulted.
pub(super) fn license_accepted(
    expr: &LicenseExpr,
    accept: &portage_repo::AcceptLicense,
    enabled: &dyn Fn(&str) -> bool,
) -> bool {
    accept.accepts_expr(expr, enabled)
}

/// Collects the license names NOT covered by `accept`, evaluating conditionals
/// against `enabled` (see [`license_accepted`]).
fn licenses_needed(
    expr: &LicenseExpr,
    accept: &portage_repo::AcceptLicense,
    enabled: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    accept.licenses_needed(expr, enabled)
}

/// True if a `LICENSE` expression contains any `use? ( … )` conditional. When
/// false, license acceptance is USE-independent and the effective USE need not
/// be computed (a hot-path shortcut for the common non-conditional case).
fn license_has_conditional(expr: &LicenseExpr) -> bool {
    match expr {
        LicenseExpr::License(_) => false,
        LicenseExpr::AnyOf(c) | LicenseExpr::All(c) => c.iter().any(license_has_conditional),
        LicenseExpr::UseConditional { .. } => true,
    }
}

/// Build the effective USE config for `cpv` from the resolved global USE,
/// per-version `package.use`, the ebuild's IUSE defaults and the profile
/// force/mask sets — the same layering `Adapter::desired_use` does, minus the
/// Level-C cede. Used to evaluate USE-conditional `LICENSE` expressions both in
/// the version filter and the autounmask reasons (which have no `Adapter`).
#[allow(clippy::too_many_arguments)]
fn effective_use_config(
    use_config: &portage_atom_pubgrub::UseConfig,
    package_use: &[(Dep, Vec<UseOverride>)],
    force_mask: &super::force_mask::ForceMask,
    accept_keywords: &AcceptKeywords,
    cpv: &Cpv,
    meta: &portage_metadata::EbuildMetadata,
    slot: Option<Interned<DefaultInterner>>,
) -> portage_atom_pubgrub::UseConfig {
    use portage_atom_pubgrub::apply_package_use;

    let mut cfg = apply_package_use(use_config, cpv, slot, package_use).into_owned();
    apply_iuse_defaults(&mut cfg, meta);
    if !force_mask.is_empty() {
        let stable = accept_keywords.is_stable(&meta.keywords, cpv, slot);
        force_mask.apply(&mut cfg, cpv, stable);
    }
    cfg
}

/// A USE-flag predicate (`enabled`) over an effective `UseConfig`.
fn use_predicate(cfg: &portage_atom_pubgrub::UseConfig) -> impl Fn(&str) -> bool + '_ {
    use portage_atom_pubgrub::UseFlagState;
    move |flag: &str| matches!(cfg.get(Interned::intern(flag)), UseFlagState::Enabled)
}

/// Whether `meta`'s `LICENSE` is accepted for version `cpv`, evaluating any
/// `use? ( … )` branch against the version's effective USE. Effective USE is
/// computed only when the expression has conditionals.
#[allow(clippy::too_many_arguments)]
fn license_ok_for(
    cpv: &Cpv,
    meta: &portage_metadata::EbuildMetadata,
    accept_licenses: &AcceptLicenses,
    use_config: &portage_atom_pubgrub::UseConfig,
    package_use: &[(Dep, Vec<UseOverride>)],
    force_mask: &super::force_mask::ForceMask,
    accept_keywords: &AcceptKeywords,
) -> bool {
    let Some(lic) = &meta.license else {
        return true;
    };
    let slot = Some(meta.slot.slot);
    let accept = accept_licenses.effective_for(cpv, slot);
    if !license_has_conditional(lic) {
        return license_accepted(lic, &accept, &|_| false);
    }
    let cfg = effective_use_config(
        use_config,
        package_use,
        force_mask,
        accept_keywords,
        cpv,
        meta,
        slot,
    );
    license_accepted(lic, &accept, &use_predicate(&cfg))
}

/// Check whether `mask_dep` matches the given `cpv` (version + CPN, no slot check).
/// Whether `cpv` (in `slot`) is masked: some mask atom matches and no unmask
/// atom does (`/etc/portage/package.unmask` cancels masks per package,
/// portage(5)).
pub(super) fn is_masked(
    masks: &[Dep],
    unmasks: &[Dep],
    cpv: &Cpv,
    slot: &portage_atom::Slot,
) -> bool {
    let hit = |m: &Dep| mask_matches(m, cpv) && mask_slot_matches(m, slot);
    masks.iter().any(hit) && !unmasks.iter().any(hit)
}

/// Whether a mask atom's `:slot[/subslot]` component (if any) matches the
/// candidate's slot. A versionless slot-scoped mask like
/// `dev-qt/qttranslations:5` must not mask other slots.
fn mask_slot_matches(mask_dep: &Dep, slot: &portage_atom::Slot) -> bool {
    match &mask_dep.slot_dep {
        Some(portage_atom::SlotDep::Slot { slot: Some(s), .. }) => {
            s.slot == slot.slot
                && match s.subslot {
                    Some(ms) => slot.subslot.is_some_and(|cs| cs == ms),
                    None => true,
                }
        }
        _ => true,
    }
}

pub(super) fn mask_matches(mask_dep: &Dep, cpv: &Cpv) -> bool {
    if mask_dep.cpn != cpv.cpn {
        return false;
    }
    let (Some(op), Some(mask_ver)) = (mask_dep.op, &mask_dep.version) else {
        return mask_dep.version.is_none();
    };
    let cand = &cpv.version;
    match op {
        Operator::Equal => {
            if mask_dep.glob {
                cand.glob_matches(mask_ver)
            } else {
                cand == mask_ver
            }
        }
        Operator::GreaterOrEqual => cand >= mask_ver,
        Operator::Greater => cand > mask_ver,
        Operator::LessOrEqual => cand <= mask_ver,
        Operator::Less => cand < mask_ver,
        Operator::Approximate => {
            let mut base_mask = mask_ver.clone();
            base_mask.revision = Default::default();
            let mut base_cand = cand.clone();
            base_cand.revision = Default::default();
            base_cand == base_mask
        }
    }
}

pub(super) struct RepoData {
    pub(super) cpns: Vec<Cpn>,
    pub(super) versions: HashMap<Cpn, Vec<(Cpv, CacheEntry)>>,
    /// The main repo's name; versions from overlays are recorded in `repo_of`.
    pub(super) repo_name: String,
    /// Source repo of overlay-provided versions (absent ⇒ the main repo).
    pub(super) repo_of: HashMap<Cpv, String>,
}

/// The repo a version comes from (for `::repo` display and constraints).
pub(super) fn repo_name_of<'a>(data: &'a RepoData, cpv: &Cpv) -> &'a str {
    data.repo_of
        .get(cpv)
        .map_or(data.repo_name.as_str(), String::as_str)
}

pub(super) struct Adapter<'a> {
    pub(super) data: &'a RepoData,
    pub(super) accept_keywords: &'a AcceptKeywords,
    pub(super) package_mask: &'a [Dep],
    pub(super) package_unmask: &'a [Dep],
    pub(super) accept_licenses: &'a AcceptLicenses,
    /// Global desired USE (profile + make.conf), folded with per-version
    /// `package.use` + IUSE defaults by `desired_use`.
    pub(super) use_config: &'a portage_atom_pubgrub::UseConfig,
    pub(super) package_use: &'a [(Dep, Vec<UseOverride>)],
    /// Profile USE force/mask policy: applied to each version's effective USE and
    /// consulted by the Level-C cede gate (pinned flags are never ceded).
    pub(super) force_mask: &'a super::force_mask::ForceMask,
    /// Exact installed cpvs. A version that is installed and staying installed
    /// never has its `REQUIRED_USE` flags ceded — its USE was decided at build
    /// time, and only packages being built get theirs auto-satisfied (emerge
    /// likewise leaves installed packages' constraints alone).
    pub(super) installed_cpvs: &'a std::collections::HashSet<Cpv>,
    /// Level-C: when set, cede each package's non-pinned `REQUIRED_USE` flags to
    /// the solver (`SolverDecided`) instead of fixing them. See
    /// `portage-atom-pubgrub/docs/required-use-level-c.md`.
    pub(super) autosolve_use: bool,
}

/// Apply the ebuild's IUSE defaults: every IUSE flag not already set by the
/// resolved config takes its `+`/`-` default.
fn apply_iuse_defaults(
    cfg: &mut portage_atom_pubgrub::UseConfig,
    m: &portage_metadata::EbuildMetadata,
) {
    use portage_atom_pubgrub::UseFlagState;
    for iu in &m.iuse {
        let flag = iu.interned();
        if cfg.get_opt(flag).is_none()
            && let Some(def) = iu.default
        {
            cfg.set(
                flag,
                match def {
                    portage_metadata::IUseDefault::Enabled => UseFlagState::Enabled,
                    portage_metadata::IUseDefault::Disabled => UseFlagState::Disabled,
                },
            );
        }
    }
}

impl Adapter<'_> {
    /// The shared version filter: keywords, package.mask/unmask, license.
    /// `versions_for` and `slots_for` must agree, or the slot map would carry
    /// phantom slots for versions the solver can never select.
    fn version_accepted(&self, cpv: &Cpv, cache: &portage_metadata::CacheEntry) -> bool {
        let meta = &cache.metadata;
        self.accept_keywords
            .accepts(&meta.keywords, cpv, Some(meta.slot.slot))
            && !is_masked(self.package_mask, self.package_unmask, cpv, &meta.slot)
            && self.license_ok(cpv, meta)
    }

    /// License acceptance for a version, evaluating any `use? ( … )` LICENSE
    /// branch against the version's effective USE (computed only when the
    /// expression actually has conditionals).
    fn license_ok(&self, cpv: &Cpv, meta: &portage_metadata::EbuildMetadata) -> bool {
        license_ok_for(
            cpv,
            meta,
            self.accept_licenses,
            self.use_config,
            self.package_use,
            self.force_mask,
            self.accept_keywords,
        )
    }

    /// Level-C cede: when `--autosolve-use` is on and the package's REQUIRED_USE
    /// is *violated* by the resolved config, hand its REQUIRED_USE flags to the
    /// solver as preferences (`solver_decide`) — a ceded flag keeps its resolved
    /// value as the preference (greedy keep-config) and the solver only flips it
    /// to satisfy REQUIRED_USE. Flags the user pinned via package.use, or the
    /// profile forced/masked, are left fixed (hard choices we must not override).
    ///
    /// Skips ceding entirely when the constraint already holds, so settled flags
    /// (e.g. a USE_EXPAND like LLVM_SLOT/PYTHON_TARGETS) are not re-decided and
    /// their conditional deps not gratuitously pulled — matching emerge, which
    /// acts only on violations.
    fn cede_required_use(
        &self,
        cfg: &mut portage_atom_pubgrub::UseConfig,
        m: &portage_metadata::EbuildMetadata,
        cpv: &Cpv,
        slot: Option<Interned<DefaultInterner>>,
        stable: bool,
    ) {
        use portage_atom_pubgrub::{UseConfig, UseFlagState, apply_package_use};

        if self.installed_cpvs.contains(cpv) {
            return;
        }
        let Some(ru) = &m.required_use else {
            return;
        };
        let enabled = |flag: &str| matches!(cfg.get(Interned::intern(flag)), UseFlagState::Enabled);
        if ru.unsatisfied(&enabled).is_empty() {
            return;
        }

        // Flags the user pinned via package.use: applying it to an empty base
        // leaves exactly those flags set.
        let empty = UseConfig::new();
        let pins = apply_package_use(&empty, cpv, slot, self.package_use);
        let iuse: std::collections::HashSet<&str> = m.iuse.iter().map(|iu| iu.name()).collect();
        // Flags pinned by use.force/use.mask (global, package-level and the stable
        // variants): hard profile decisions, never ceded.
        let forced_masked = self.force_mask.pins(cpv, stable);
        let mut names = std::collections::BTreeSet::new();
        collect_required_use_flags(ru, &mut names);
        for name in names {
            let flag = Interned::intern(&name);
            // Only cede real flags the user has not pinned or the profile has not
            // forced/masked.
            if !iuse.contains(name.as_str())
                || pins.get_opt(flag).is_some()
                || forced_masked.contains(&flag)
            {
                continue;
            }
            let prefer = matches!(cfg.get(flag), UseFlagState::Enabled);
            cfg.solver_decide(flag, prefer);
        }
    }
}

impl PackageRepository for Adapter<'_> {
    fn all_packages(&self) -> Vec<Cpn> {
        self.data.cpns.clone()
    }

    fn desired_use(&self, cpv: &Cpv) -> portage_atom_pubgrub::UseConfig {
        use portage_atom_pubgrub::{UseConfig, apply_package_use};

        let meta = self
            .data
            .versions
            .get(&cpv.cpn)
            .and_then(|entries| entries.iter().find(|(c, _)| c.version == cpv.version))
            .map(|(_, cache)| &cache.metadata);

        let slot = meta.map(|m| m.slot.slot);

        // Caller-resolved policy: package.use over global USE, then the ebuild's
        // IUSE defaults for anything still unset → the authoritative desired set.
        let mut cfg: UseConfig =
            apply_package_use(self.use_config, cpv, slot, self.package_use).into_owned();
        if let Some(m) = meta {
            apply_iuse_defaults(&mut cfg, m);
        }

        // Profile USE force/mask override package.use and the configured value
        // (Portage semantics). Global use.force/use.mask are already in the base
        // config; this layers the package-level sets plus the *.stable.* sets,
        // the latter only when this version is merged due to a stable keyword.
        // This is what makes crossdev's package.use.force/mask (multilib/cet/…)
        // take effect on cross-* packages.
        let stable = meta.is_some_and(|m| {
            self.accept_keywords
                .is_stable(&m.keywords, cpv, Some(m.slot.slot))
        });
        if !self.force_mask.is_empty() {
            self.force_mask.apply(&mut cfg, cpv, stable);
        }

        // Level-C: cede this package's REQUIRED_USE flags to the solver.
        if self.autosolve_use
            && let Some(m) = meta
        {
            self.cede_required_use(&mut cfg, m, cpv, slot, stable);
        }
        cfg
    }

    fn slots_for(&self, cpn: &Cpn) -> Vec<Interned<DefaultInterner>> {
        // Best (newest) version per slot, then ordered by version (ascending) so
        // the `SlotChoice` numbering ranks slots by version, not slot name — see
        // `portage_atom_pubgrub::rank_slots_by_version`. Mirrors portage's
        // version-descending `:*` selection (e.g. `app-shells/bash:0` (5.3)
        // beats the `:5.1` compat slot).
        let mut best: HashMap<Interned<DefaultInterner>, Version> = HashMap::new();
        if let Some(entries) = self.data.versions.get(cpn) {
            for (cpv, cache) in entries {
                if !self.version_accepted(cpv, cache) {
                    continue;
                }
                // SLOT is mandatory in md5-cache (`CacheEntry::parse` rejects a
                // missing/empty SLOT), so every stored version has a real slot.
                best.entry(cache.metadata.slot.slot)
                    .and_modify(|v| {
                        if cpv.version > *v {
                            *v = cpv.version.clone();
                        }
                    })
                    .or_insert_with(|| cpv.version.clone());
            }
        }
        portage_atom_pubgrub::rank_slots_by_version(best)
    }

    fn versions_for(&self, cpn: &Cpn) -> Vec<(Cpv, PackageVersions)> {
        self.data
            .versions
            .get(cpn)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|(cpv, cache)| self.version_accepted(cpv, cache))
                    .map(|(cpv, cache)| {
                        let meta = &cache.metadata;
                        let slot = Some(meta.slot.slot);
                        let subslot = meta.slot.subslot;
                        let repo = Some(Interned::<DefaultInterner>::intern(repo_name_of(
                            self.data, cpv,
                        )));
                        let iuse: Vec<Interned<DefaultInterner>> = meta
                            .iuse
                            .iter()
                            .map(|iu| Interned::intern(iu.name()))
                            .collect();
                        let iuse_defaults: HashMap<Interned<DefaultInterner>, IUseDefault> = meta
                            .iuse
                            .iter()
                            .filter_map(|iu| {
                                iu.default.map(|d| {
                                    let val = match d {
                                        portage_metadata::IUseDefault::Enabled => {
                                            IUseDefault::Enabled
                                        }
                                        portage_metadata::IUseDefault::Disabled => {
                                            IUseDefault::Disabled
                                        }
                                    };
                                    (Interned::intern(iu.name()), val)
                                })
                            })
                            .collect();
                        let deps = PackageDeps {
                            depend: meta.depend.clone(),
                            rdepend: meta.rdepend.clone(),
                            bdepend: meta.bdepend.clone(),
                            pdepend: meta.pdepend.clone(),
                            idepend: meta.idepend.clone(),
                        };
                        // Translate the parsed metadata grammar into the solver's
                        // interned-flag fact vocabulary (the crate stays free of
                        // portage-metadata). Dormant until Level-C consumes it.
                        let required_use = meta.required_use.as_ref().map(translate_required_use);
                        (
                            cpv.clone(),
                            PackageVersions {
                                slot,
                                subslot,
                                repo,
                                iuse,
                                iuse_defaults,
                                deps,
                                required_use,
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Translate `portage_metadata::RequiredUseExpr` (string flags) into the
/// solver's `RequiredUse` fact (interned flags).
///
/// This is the caller's adaptation step — it keeps `portage-atom-pubgrub`
/// decoupled from the md5-cache parser, mirroring how dep strings become
/// `DepEntry`. Pure structural translation; no policy.
fn translate_required_use(expr: &RequiredUseExpr) -> RequiredUse {
    let kids = |v: &[RequiredUseExpr]| v.iter().map(translate_required_use).collect();
    match expr {
        RequiredUseExpr::Flag { name, negated } => RequiredUse::Flag {
            name: Interned::intern(name),
            negated: *negated,
        },
        RequiredUseExpr::AnyOf(c) => RequiredUse::AnyOf(kids(c)),
        RequiredUseExpr::ExactlyOne(c) => RequiredUse::ExactlyOne(kids(c)),
        RequiredUseExpr::AtMostOne(c) => RequiredUse::AtMostOne(kids(c)),
        RequiredUseExpr::UseConditional {
            flag,
            negated,
            entries,
        } => RequiredUse::UseConditional {
            flag: Interned::intern(flag),
            negated: *negated,
            entries: kids(entries),
        },
        RequiredUseExpr::All(c) => RequiredUse::All(kids(c)),
    }
}

/// Collect every flag name mentioned in a `REQUIRED_USE` expression (guards and
/// operands, ignoring `!`), for deciding which flags to cede.
fn collect_required_use_flags(
    expr: &RequiredUseExpr,
    out: &mut std::collections::BTreeSet<String>,
) {
    match expr {
        RequiredUseExpr::Flag { name, .. } => {
            out.insert(name.clone());
        }
        RequiredUseExpr::AnyOf(c)
        | RequiredUseExpr::ExactlyOne(c)
        | RequiredUseExpr::AtMostOne(c)
        | RequiredUseExpr::All(c) => {
            for e in c {
                collect_required_use_flags(e, out);
            }
        }
        RequiredUseExpr::UseConditional { flag, entries, .. } => {
            out.insert(flag.clone());
            for e in entries {
                collect_required_use_flags(e, out);
            }
        }
    }
}

/// Load the main repo's md5-cache plus every overlay's metadata (sourcing
/// cache-less ebuilds — see `overlay::overlay_entries`). A cpv provided by
/// the main repo wins over an overlay copy; among overlays, earlier wins.
pub(super) async fn load_repos(
    repo: &Repository,
    overlays: &[(Repository, Vec<Repository>)],
) -> RepoData {
    let mut cpns_set: HashSet<Cpn> = HashSet::new();
    let mut versions: HashMap<Cpn, Vec<(Cpv, CacheEntry)>> = HashMap::new();
    let mut repo_of: HashMap<Cpv, String> = HashMap::new();
    let mut seen: HashSet<Cpv> = HashSet::new();

    let entries = cache_entries_parallel(
        std::slice::from_ref(repo),
        &CacheReadOpts::default(),
        |text| CacheEntry::parse(text).map_err(portage_repo::Error::from),
    )
    .await;

    for (cpv, entry) in entries {
        if let Ok(entry) = entry {
            let cpn = cpv.cpn;
            cpns_set.insert(cpn);
            seen.insert(cpv.clone());
            versions.entry(cpn).or_default().push((cpv, entry));
        }
    }

    for (overlay, masters) in overlays {
        for (cpv, entry) in super::overlay::overlay_entries(overlay, masters).await {
            if !seen.insert(cpv.clone()) {
                continue;
            }
            cpns_set.insert(cpv.cpn);
            repo_of.insert(cpv.clone(), overlay.name().to_string());
            versions.entry(cpv.cpn).or_default().push((cpv, entry));
        }
    }

    let mut cpns: Vec<Cpn> = cpns_set.into_iter().collect();
    // `Cpn: Ord` already compares category then package over the interned
    // strings — alphabetical, no per-comparison allocation.
    cpns.sort_unstable();

    RepoData {
        cpns,
        versions,
        repo_name: repo.name().to_string(),
        repo_of,
    }
}

/// Map a dep atom to a `PortagePackage` for the solver.
#[allow(clippy::too_many_arguments)]
pub(super) fn target_package(
    data: &RepoData,
    dep: &Dep,
    accept_keywords: &AcceptKeywords,
    package_mask: &[Dep],
    package_unmask: &[Dep],
    accept_licenses: &AcceptLicenses,
    use_config: &portage_atom_pubgrub::UseConfig,
    package_use: &[(Dep, Vec<UseOverride>)],
    force_mask: &super::force_mask::ForceMask,
) -> portage_atom_pubgrub::PortagePackage {
    let entries = match data.versions.get(&dep.cpn) {
        Some(e) => e,
        None => return portage_atom_pubgrub::PortagePackage::unslotted(dep.cpn),
    };

    // The target slot is the slot of the newest accepted version that satisfies
    // the atom's slot operator and version constraint. `matches_cpv` applies the
    // `:slot` dep and version op — a bare name (or `:*`) matches every accepted
    // version, giving the newest slot; `cat/pkg:3.13` / `=cat/pkg-3.13*` pin the
    // matching slot, as emerge does. The version-set is applied separately to the
    // resolved package by the caller, so here we only pin the slot identity.
    let target_slot = entries
        .iter()
        .filter(|(cpv, cache)| {
            let slot = cache.metadata.slot.slot;
            dep.matches_cpv(cpv, Some(slot.as_str()))
                && accept_keywords.accepts(&cache.metadata.keywords, cpv, Some(slot))
                && !is_masked(package_mask, package_unmask, cpv, &cache.metadata.slot)
                && license_ok_for(
                    cpv,
                    &cache.metadata,
                    accept_licenses,
                    use_config,
                    package_use,
                    force_mask,
                    accept_keywords,
                )
        })
        .max_by(|a, b| a.0.version.cmp(&b.0.version))
        .map(|(_, cache)| cache.metadata.slot.slot);

    match target_slot {
        Some(slot) => portage_atom_pubgrub::PortagePackage::slotted(dep.cpn, slot),
        // Nothing accepted satisfies the qualifier: hand the solver an unslotted
        // package so it fails to resolve (emerge: "no match") rather than
        // silently retargeting another slot.
        None => portage_atom_pubgrub::PortagePackage::unslotted(dep.cpn),
    }
}

/// Collect all dependency CPNs referenced in a package version's raw dep data.
///
/// Walks the full dep tree (across all dep classes, through all conditional and
/// group nodes) so that masked transitive deps are captured regardless of USE
/// flag state at detection time.
pub(super) fn cpns_for(data: &RepoData, cpn: &Cpn, ver: &Version) -> Vec<Cpn> {
    use portage_atom::DepEntry;

    fn walk(entries: &[DepEntry], out: &mut Vec<Cpn>) {
        for e in entries {
            match e {
                DepEntry::Atom(dep) if dep.blocker.is_none() => out.push(dep.cpn),
                DepEntry::UseConditional { children, .. }
                | DepEntry::AnyOf(children)
                | DepEntry::AllOf(children)
                | DepEntry::ExactlyOneOf(children)
                | DepEntry::AtMostOneOf(children) => walk(children, out),
                _ => {}
            }
        }
    }

    let Some(entries) = data.versions.get(cpn) else {
        return vec![];
    };
    let Some((_, cache)) = entries.iter().find(|(cpv, _)| &cpv.version == ver) else {
        return vec![];
    };
    let meta = &cache.metadata;
    let mut out = Vec::new();
    for deps in [
        &meta.depend,
        &meta.rdepend,
        &meta.bdepend,
        &meta.pdepend,
        &meta.idepend,
    ] {
        walk(deps, &mut out);
    }
    out
}

pub(super) fn find_cache<'a>(
    data: &'a RepoData,
    pkg: &portage_atom_pubgrub::PortagePackage,
    ver: &Version,
) -> Option<&'a CacheEntry> {
    data.versions
        .get(pkg.cpn())?
        .iter()
        .find(|(cpv, _)| &cpv.version == ver)
        .map(|(_, e)| e)
}

/// For each dropped dep, find versions in the unfiltered repo that match its
/// version range and determine why they were excluded.
#[allow(clippy::too_many_arguments)]
pub(super) fn find_autounmask_candidates(
    data: &RepoData,
    dropped: &[DroppedDep],
    accept_keywords: &AcceptKeywords,
    package_mask: &[Dep],
    package_unmask: &[Dep],
    accept_licenses: &AcceptLicenses,
    use_config: &portage_atom_pubgrub::UseConfig,
    package_use: &[(Dep, Vec<UseOverride>)],
    force_mask: &super::force_mask::ForceMask,
) -> Vec<AutounmaskCandidate> {
    let mut candidates = Vec::new();

    for dep in dropped {
        if dep.package.is_virtual() {
            continue;
        }
        // || group with available alternatives: the solver picked one of them,
        // no need to unmask/keyword the dropped branch.
        if !dep.alternatives.is_empty() {
            continue;
        }
        let cpn = dep.package.cpn();
        let Some(entries) = data.versions.get(cpn) else {
            continue;
        };

        for (cpv, cache) in entries {
            if !dep.version_set.contains(&cpv.version) {
                continue;
            }
            let meta = &cache.metadata;
            let slot = Some(meta.slot.slot);

            let mut reasons = Vec::new();

            if let Some(kw) = accept_keywords.keyword_needed(&meta.keywords, cpv, slot) {
                reasons.push(FilterReason::Keyword(kw));
            }
            if is_masked(package_mask, package_unmask, cpv, &meta.slot) {
                reasons.push(FilterReason::Masked);
            }
            if let Some(lic) = &meta.license {
                let accept = accept_licenses.effective_for(cpv, slot);
                // Evaluate `use? ( … )` branches against the version's effective
                // USE so a non-FREE license behind a disabled flag is not flagged.
                let needed = if license_has_conditional(lic) {
                    let cfg = effective_use_config(
                        use_config,
                        package_use,
                        force_mask,
                        accept_keywords,
                        cpv,
                        meta,
                        slot,
                    );
                    licenses_needed(lic, &accept, &use_predicate(&cfg))
                } else {
                    licenses_needed(lic, &accept, &|_| false)
                };
                if !needed.is_empty() {
                    reasons.push(FilterReason::License(needed));
                }
            }

            if !reasons.is_empty() {
                candidates.push(AutounmaskCandidate {
                    cpv: cpv.clone(),
                    slot,
                    reasons,
                });
            }
        }
    }

    // A CPV may appear from multiple DroppedDep entries; its reasons are
    // determined solely by its own metadata, so we keep the first occurrence.
    let mut seen: HashSet<String> = HashSet::new();
    candidates.retain(|c| seen.insert(c.cpv.to_string()));

    candidates
}

#[cfg(test)]
mod tests {
    use super::super::force_mask::{ForceMask, index_by_cpn};
    use super::*;
    use portage_atom_pubgrub::{PackageRepository, UseConfig, UseFlagState};
    use portage_repo::{AcceptLicense, LicenseGroupRegistry};

    fn accept_all_licenses() -> AcceptLicense {
        AcceptLicense::from_tokens(&["*".into()], &LicenseGroupRegistry::default())
    }

    fn dep(s: &str) -> Dep {
        Dep::parse(s).unwrap()
    }

    #[test]
    fn accept_keywords_per_package_override() {
        let arch = Arch::intern("arm64");
        let tok = |s: &str| AcceptToken::parse(s).unwrap();
        let kws = |s: &str| Keyword::parse_line(s).unwrap();
        let testing = kws("~arm64");
        let stable = kws("arm64");
        let foo = Cpv::parse("dev-libs/foo-1").unwrap();
        let bar = Cpv::parse("dev-libs/bar-1").unwrap();

        // Global stable-only config: ~arm64 is masked everywhere.
        let global = AcceptKeywords::from_global(&arch, &["arm64"]);
        assert!(global.accepts(&stable, &foo, None));
        assert!(!global.accepts(&testing, &foo, None));
        // Autounmask would suggest ~arm64 for the testing-only package.
        assert_eq!(
            global.keyword_needed(&testing, &foo, None).as_deref(),
            Some("~arm64")
        );

        // Per-package `dev-libs/foo ~arm64` unmasks foo but not bar.
        let per = AcceptKeywords::new(
            &arch,
            &[tok("arm64")],
            vec![(dep("dev-libs/foo"), vec![tok("~arm64")])],
        );
        assert!(per.accepts(&testing, &foo, None), "foo unmasked");
        assert!(!per.accepts(&testing, &bar, None), "bar still masked");
        // No longer suggested for foo (it is already accepted).
        assert_eq!(per.keyword_needed(&testing, &foo, None), None);

        // A bare atom (`dev-libs/foo`) means "accept this arch's ~arm64".
        let bare = AcceptKeywords::new(&arch, &[tok("arm64")], vec![(dep("dev-libs/foo"), vec![])]);
        assert!(bare.accepts(&testing, &foo, None));

        // A testing merge is never "stable" for use.stable.* gating.
        assert!(global.is_stable(&stable, &foo, None));
        assert!(!per.is_stable(&testing, &foo, None));
    }

    #[test]
    fn accept_license_per_package_override() {
        let groups = LicenseGroupRegistry::default();
        let global = AcceptLicense::from_tokens(&["MIT".into()], &groups);
        let foo = Cpv::parse("dev-libs/foo-1").unwrap();
        let bar = Cpv::parse("dev-libs/bar-1").unwrap();

        // package.license `dev-libs/foo GPL-2` accepts GPL-2 for foo only.
        let licenses = AcceptLicenses::new(
            global.clone(),
            vec![(
                dep("dev-libs/foo"),
                AcceptLicense::from_tokens(&["GPL-2".into()], &groups),
            )],
        );
        assert!(licenses.effective_for(&foo, None).accepts("GPL-2"));
        assert!(!licenses.effective_for(&bar, None).accepts("GPL-2"));
        // The global MIT acceptance still applies everywhere.
        assert!(licenses.effective_for(&bar, None).accepts("MIT"));

        // No per-package entries ⇒ the global decision is borrowed unchanged.
        let plain = AcceptLicenses::new(global, Vec::new());
        assert!(!plain.effective_for(&foo, None).accepts("GPL-2"));
        assert!(plain.effective_for(&foo, None).accepts("MIT"));
    }

    /// Build a one-package `RepoData` from md5-cache text.
    fn repo_with(cpv: &str, cache_text: &str) -> (RepoData, Cpv) {
        let cpv = Cpv::parse(cpv).unwrap();
        let entry = CacheEntry::parse(cache_text).unwrap();
        let mut versions = HashMap::new();
        versions.insert(cpv.cpn, vec![(cpv.clone(), entry)]);
        (
            RepoData {
                repo_of: Default::default(),
                cpns: vec![cpv.cpn],
                versions,
                repo_name: "test".into(),
            },
            cpv,
        )
    }

    /// Build a multi-version `RepoData` from `(cpv, cache_text)` pairs (one CPN).
    fn repo_with_many(entries: &[(&str, &str)]) -> RepoData {
        let mut versions: HashMap<Cpn, Vec<(Cpv, CacheEntry)>> = HashMap::new();
        let mut cpn = None;
        for (cpv_s, cache_text) in entries {
            let cpv = Cpv::parse(cpv_s).unwrap();
            cpn = Some(cpv.cpn);
            let entry = CacheEntry::parse(cache_text).unwrap();
            versions.entry(cpv.cpn).or_default().push((cpv, entry));
        }
        RepoData {
            repo_of: Default::default(),
            cpns: vec![cpn.unwrap()],
            versions,
            repo_name: "test".into(),
        }
    }

    // `target_package` resolves the slot identity from the atom's slot operator
    // and version constraint (emerge semantics): a bare name / `:*` picks the
    // newest slot, but `:slot` and `=…-ver*` pin the matching slot rather than
    // collapsing to the newest. Regression for the python:3.13 / =python-3.13*
    // gap (todo/target-derivation.md).
    #[test]
    fn target_package_honours_slot_and_version_qualifiers() {
        let cache = |slot: &str| format!("EAPI=8\nSLOT={slot}\nKEYWORDS=amd64\nDESCRIPTION=t\n");
        let data = repo_with_many(&[
            ("dev-lang/python-3.13.12", &cache("3.13")),
            ("dev-lang/python-3.13.14", &cache("3.13")),
            ("dev-lang/python-3.14.5", &cache("3.14")),
            ("dev-lang/python-3.14.6", &cache("3.14")),
        ]);
        let arch = Arch::intern("amd64");
        let ak = AcceptKeywords::from_global(&arch, &["amd64"]);
        let slot = |atom: &str| {
            target_package(
                &data,
                &dep(atom),
                &ak,
                &[],
                &[],
                &AcceptLicenses::new(accept_all_licenses(), Vec::new()),
                &UseConfig::new(),
                &[],
                &ForceMask::default(),
            )
            .slot()
            .map(|s| s.as_str().to_string())
        };

        // bare name and `:*` → newest slot
        assert_eq!(slot("dev-lang/python").as_deref(), Some("3.14"));
        assert_eq!(slot("dev-lang/python:*").as_deref(), Some("3.14"));
        // explicit slot honoured
        assert_eq!(slot("dev-lang/python:3.13").as_deref(), Some("3.13"));
        assert_eq!(slot("dev-lang/python:3.14").as_deref(), Some("3.14"));
        // version glob picks the matching slot, not the newest
        assert_eq!(slot("=dev-lang/python-3.13*").as_deref(), Some("3.13"));
        assert_eq!(slot("=dev-lang/python-3.14*").as_deref(), Some("3.14"));
    }

    // A flag forced by use.force must not be ceded to the solver even when it is
    // named in a violated REQUIRED_USE — only the non-forced flag is ceded, so
    // the solver can never produce a plan that flips a forced flag.
    #[test]
    fn forced_flag_is_not_ceded() {
        let (data, cpv) = repo_with(
            "cat/pkg-1.0",
            "EAPI=7\nSLOT=0\nIUSE=a b\nREQUIRED_USE=?? ( a b )\nKEYWORDS=amd64\nDESCRIPTION=t\n",
        );
        let arch = Arch::intern("amd64");
        let mut use_config = UseConfig::new();
        use_config.enable(Interned::intern("a"));
        use_config.enable(Interned::intern("b")); // both on ⇒ ?? ( a b ) violated

        let fm = ForceMask {
            use_force: vec![Interned::intern("a")],
            ..Default::default()
        };
        let ak = AcceptKeywords::from_global(&arch, &["amd64"]);
        let adapter = Adapter {
            data: &data,
            accept_keywords: &ak,
            package_mask: &[],
            package_unmask: &[],
            installed_cpvs: &std::collections::HashSet::new(),
            accept_licenses: &AcceptLicenses::new(accept_all_licenses(), Vec::new()),
            use_config: &use_config,
            package_use: &[],
            force_mask: &fm, // a is use.force'd
            autosolve_use: true,
        };

        let desired = adapter.desired_use(&cpv);
        assert!(
            matches!(desired.get(Interned::intern("a")), UseFlagState::Enabled),
            "forced flag a must stay fixed-enabled, not ceded"
        );
        assert!(
            matches!(
                desired.get(Interned::intern("b")),
                UseFlagState::SolverDecided { .. }
            ),
            "non-forced flag b should be ceded to satisfy the violated ?? ( a b )"
        );
    }

    // Without the force pin, both flags in the violated constraint are ceded.
    #[test]
    fn unforced_flags_are_ceded() {
        let (data, cpv) = repo_with(
            "cat/pkg-1.0",
            "EAPI=7\nSLOT=0\nIUSE=a b\nREQUIRED_USE=?? ( a b )\nKEYWORDS=amd64\nDESCRIPTION=t\n",
        );
        let arch = Arch::intern("amd64");
        let mut use_config = UseConfig::new();
        use_config.enable(Interned::intern("a"));
        use_config.enable(Interned::intern("b"));

        let fm = ForceMask::default();
        let ak = AcceptKeywords::from_global(&arch, &["amd64"]);
        let adapter = Adapter {
            data: &data,
            accept_keywords: &ak,
            package_mask: &[],
            package_unmask: &[],
            installed_cpvs: &std::collections::HashSet::new(),
            accept_licenses: &AcceptLicenses::new(accept_all_licenses(), Vec::new()),
            use_config: &use_config,
            package_use: &[],
            force_mask: &fm,
            autosolve_use: true,
        };

        let desired = adapter.desired_use(&cpv);
        for f in ["a", "b"] {
            assert!(
                matches!(
                    desired.get(Interned::intern(f)),
                    UseFlagState::SolverDecided { .. }
                ),
                "flag {f} should be ceded when nothing pins it"
            );
        }
    }

    // A satisfied REQUIRED_USE cedes nothing (cede gating).
    #[test]
    fn satisfied_constraint_cedes_nothing() {
        let (data, cpv) = repo_with(
            "cat/pkg-1.0",
            "EAPI=7\nSLOT=0\nIUSE=a b\nREQUIRED_USE=?? ( a b )\nKEYWORDS=amd64\nDESCRIPTION=t\n",
        );
        let arch = Arch::intern("amd64");
        let mut use_config = UseConfig::new();
        use_config.enable(Interned::intern("a")); // only a on ⇒ ?? satisfied

        let fm = ForceMask::default();
        let ak = AcceptKeywords::from_global(&arch, &["amd64"]);
        let adapter = Adapter {
            data: &data,
            accept_keywords: &ak,
            package_mask: &[],
            package_unmask: &[],
            installed_cpvs: &std::collections::HashSet::new(),
            accept_licenses: &AcceptLicenses::new(accept_all_licenses(), Vec::new()),
            use_config: &use_config,
            package_use: &[],
            force_mask: &fm,
            autosolve_use: true,
        };

        let desired = adapter.desired_use(&cpv);
        for f in ["a", "b"] {
            assert!(
                !matches!(
                    desired.get(Interned::intern(f)),
                    UseFlagState::SolverDecided { .. }
                ),
                "flag {f} must not be ceded when REQUIRED_USE already holds"
            );
        }
    }

    // package.use.force/mask now change a package's effective USE (not just the
    // cede gate). This is the crossdev case: force multilib on, mask cet off.
    #[test]
    fn package_force_mask_change_effective_use() {
        let (data, cpv) = repo_with(
            "cross-foo/gcc-13.2",
            "EAPI=8\nSLOT=0\nIUSE=multilib cet\nKEYWORDS=~amd64\nDESCRIPTION=t\n",
        );
        let arch = Arch::intern("amd64");
        let mut use_config = UseConfig::new();
        use_config.enable(Interned::intern("cet")); // user enabled a flag the profile masks

        let fm = ForceMask {
            pkg_force: index_by_cpn(vec![(dep("cross-foo/gcc"), vec!["multilib".to_string()])]),
            pkg_mask: index_by_cpn(vec![(dep("cross-foo/gcc"), vec!["cet".to_string()])]),
            ..Default::default()
        };
        let ak = AcceptKeywords::from_global(&arch, &["~amd64"]);
        let adapter = Adapter {
            data: &data,
            accept_keywords: &ak,
            package_mask: &[],
            package_unmask: &[],
            installed_cpvs: &std::collections::HashSet::new(),
            accept_licenses: &AcceptLicenses::new(accept_all_licenses(), Vec::new()),
            use_config: &use_config,
            package_use: &[],
            force_mask: &fm,
            autosolve_use: false,
        };

        let desired = adapter.desired_use(&cpv);
        assert_eq!(
            desired.get(Interned::intern("multilib")),
            UseFlagState::Enabled,
            "package.use.force must enable multilib"
        );
        assert_eq!(
            desired.get(Interned::intern("cet")),
            UseFlagState::Disabled,
            "package.use.mask must beat the user's enable of cet"
        );
    }
}
