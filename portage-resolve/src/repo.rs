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
use portage_repo::UseExpand;

/// A reason a package version was excluded from the solver.
#[derive(Debug, Clone)]
pub enum FilterReason {
    /// Needs a keyword the system doesn't accept (e.g. `~arm64`).
    Keyword(String),
    /// Masked by the profile or user `package.mask`.
    Masked,
    /// One or more licenses not in ACCEPT_LICENSE.
    License(Vec<String>),
    /// One or more `PROPERTIES` tokens not in ACCEPT_PROPERTIES.
    Properties(Vec<String>),
    /// One or more `RESTRICT` tokens not in ACCEPT_RESTRICT.
    Restrict(Vec<String>),
}

/// A package version that was excluded and could resolve a dropped dep.
#[derive(Debug, Clone)]
pub struct AutounmaskCandidate {
    /// The excluded version.
    pub cpv: Cpv,
    /// Its slot, if known.
    pub slot: Option<Interned<DefaultInterner>>,
    /// Every reason this version was excluded.
    pub reasons: Vec<FilterReason>,
}

/// One parsed `ACCEPT_KEYWORDS` / `package.accept_keywords` token.
///
/// # Spec split (PMS vs Portage)
///
/// - **Package-side** keywords (`KEYWORDS="amd64 ~arm64 -mips"`) are PMS
///   vocabulary — [PMS 7.3.3](https://projects.gentoo.org/pms/9/pms.html#keywords)
///   (stable / testing / disabled / `-*`). Parsed into
///   [`portage_metadata::Keyword`].
/// - **User/profile-side** acceptance (`ACCEPT_KEYWORDS`,
///   `/etc/portage/package.accept_keywords`) is **Portage policy**, not PMS:
///   - [`make.conf(5)`](https://dev.gentoo.org/~zmedico/portage/doc/man/make.conf.5.html)
///     — `ACCEPT_KEYWORDS` is an *incremental* variable (profile → globals →
///     make.conf → env); defaults to `$ARCH`; users typically add `~arch`.
///   - [`portage(5)`](https://dev.gentoo.org/~zmedico/portage/doc/man/portage.5.html)
///     — `package.accept_keywords` *augments* that set per atom; bare atom ⇒
///     unstable host arch; special tokens `*` / `~*` / `**`; documented pin
///     idiom `media-video/mplayer -~x86`.
///
/// Visibility is exact membership of a package `KEYWORDS` token in the
/// folded accept set (Portage `KeywordsManager._getMissingKeywords`), not
/// “host arch bits only”. Wildcards on the *accept* side: `*` = any stable
/// package keyword, `~*` = any testing package keyword, `**` = always.
///
/// # Fold rules (why this shape)
///
/// Tokens are interned at config-read time so the solver never sees a raw
/// keyword string. Each token sets or clears **exactly its own** grant —
/// `KeywordAccept::add` is set insert/remove, never a lattice join.
///
/// Real Portage keeps `pgroups` as a flat set of *literal strings*
/// (`arm64`, `~arm64`, `riscv`, `*`, `**`, …). Match is `token in set` (plus
/// wildcards). That implies:
///
/// 1. **`-arch` and `-~arch` are independent** — `portage(5)`'s
///    `media-video/mplayer -~x86` must leave stable `x86` accepted while
///    withdrawing only `~x86`. Folding “testing ⇒ stable” at add-time made
///    both negations clear both bits (bug fixed in the Opus review pass).
/// 2. **Foreign arches are first-class** — crossdev’s usual line
///    `cross-…/pkg riscv ~riscv -arm64 -~arm64` must *grant* target tokens
///    and *withdraw* host ones. A host-only bitfield drops `riscv`/`~riscv`
///    as no-ops and still applies `-arm64`/`-~arm64`, emptying the set and
///    masking every version (live canary:
///    `cross-riscv64-unknown-linux-gnu/linux-headers`).
/// 3. **`~arch` does not accept stable `arch`** (and vice versa) at match
///    time — profile baseline is typically `ARCH` plus make.conf `~ARCH`.
/// 4. **`-*` clear-all** resets the accept set (incremental rebuild), same
///    family as make.conf(5) incremental clear-all for `USE`/`ACCEPT_*`.
///
/// # Failure modes we already hit
///
/// | Shortcut | What broke |
/// |----------|------------|
/// | Host-arch bools only | Crossdev foreign-arch grants ignored |
/// | Join testing⇒stable at fold | `mplayer -~x86` fully masked |
/// | Treat `-~arch` as `-arch` | Same pin idiom fully masked |
#[derive(Clone, Copy)]
pub enum AcceptToken {
    /// `arch` — accept the stable package keyword for this arch (any arch name).
    Stable(Interned<DefaultInterner>),
    /// `~arch` — accept the testing package keyword for this arch.
    Testing(Interned<DefaultInterner>),
    /// `*` — package visible if stable on any architecture (`portage(5)`).
    AnyStable,
    /// `~*` — package visible if testing on any architecture (`portage(5)`).
    AnyTesting,
    /// `**` — always visible; package `KEYWORDS` ignored (`portage(5)`).
    Any,
    /// `-*` — incremental clear-all of the accept set so later tokens rebuild
    /// it (make.conf(5) incremental variables; same idea as ebuild `KEYWORDS=-*`).
    ClearAll,
    /// `-arch` — withdraw only the stable grant for this arch (literal token
    /// discard). Does not touch `~arch` / `*` / `~*` / `**`.
    Negate(Interned<DefaultInterner>),
    /// `-~arch` — withdraw only the testing grant (portage(5) pin-to-stable
    /// idiom `media-video/mplayer -~x86`). Must stay distinct from [`Self::Negate`].
    NegateTesting(Interned<DefaultInterner>),
    /// `-~*` — withdraw the `~*` grant specifically.
    NegateAnyTesting,
    /// `-**` — withdraw the `**` grant specifically.
    NegateAny,
}

impl AcceptToken {
    /// Parse one token.
    pub fn parse(tok: &str) -> Option<Self> {
        match tok {
            "**" => Some(Self::Any),
            "~*" => Some(Self::AnyTesting),
            "*" => Some(Self::AnyStable),
            "-*" => Some(Self::ClearAll),
            "-**" => Some(Self::NegateAny),
            "-~*" => Some(Self::NegateAnyTesting),
            _ if tok.starts_with('-') => match tok[1..].strip_prefix('~') {
                Some(arch) if !arch.is_empty() => Some(Self::NegateTesting(Interned::intern(arch))),
                Some(_) => None, // "-~" alone: malformed
                None if !tok[1..].is_empty() => Some(Self::Negate(Interned::intern(&tok[1..]))),
                None => None, // bare "-": malformed
            },
            _ => match tok.strip_prefix('~') {
                Some(arch) => Some(Self::Testing(Interned::intern(arch))),
                None => Some(Self::Stable(Interned::intern(tok))),
            },
        }
    }
}

/// Folded accept set: multi-arch literal tokens plus wildcards.
///
/// This is Portage’s `pgroups` set after incremental stacking
/// (`KeywordsManager._getEgroups` for env/package lines,
/// `_getMissingKeywords` for the match against ebuild `KEYWORDS`).
///
/// ## Why not “host arch + five bools”?
///
/// A host-only model is tempting (`stable`/`testing` bits for `$ARCH` only)
/// because bare `ACCEPT_KEYWORDS="arm64 ~arm64"` never mentions another arch.
/// It is **wrong** for `package.accept_keywords`, which routinely lists
/// *other* arches (crossdev target keywords, binary packages on a foreign
/// KEYWORDS line, `*`/`~*`/`**`). Those tokens are ordinary set members in
/// Portage; they are not “host-relative modifiers”.
///
/// ## Match algorithm (must stay Portage-shaped)
///
/// For each positive package keyword `gp` in `KEYWORDS` (PMS 7.3.3 tokens):
/// - exact membership: `gp`’s arch is in [`Self::stable`] or [`Self::testing`]
///   according to stability (stable `arm64` ≠ testing `~arm64`);
/// - else wildcards on the accept side: `*` if the package has any stable
///   keyword, `~*` if any testing, `**` always.
///
/// Package-side `-arch` / `-*` are “disabled” entries and do not accept;
/// Portage strips `-*` from the ebuild keyword list before matching.
///
/// Fold never joins tokens: `-arch` removes only that stable arch and does
/// **not** retract a separate `*` grant (literal-token semantics; locked by
/// `negate_arch_does_not_retract_a_wildcard_grant`).
#[derive(Clone, Default)]
struct KeywordAccept {
    /// Arches granted via a bare `arch` token (any architecture name).
    stable: HashSet<Interned<DefaultInterner>>,
    /// Arches granted via a `~arch` token.
    testing: HashSet<Interned<DefaultInterner>>,
    /// `*` is granted.
    any_stable: bool,
    /// `~*` is granted.
    any_testing: bool,
    /// `**` — accept even with no matching keyword.
    any: bool,
}

impl KeywordAccept {
    /// Fold one token — plain insert/remove of the named grant, never a join.
    ///
    /// Order matters the same way as Portage incremental lists: a later grant
    /// after a veto re-adds; `-*` clears everything so subsequent tokens rebuild.
    fn add(&mut self, tok: AcceptToken) {
        match tok {
            AcceptToken::ClearAll => *self = Self::default(),
            AcceptToken::Any => self.any = true,
            AcceptToken::AnyTesting => self.any_testing = true,
            AcceptToken::AnyStable => self.any_stable = true,
            AcceptToken::Testing(a) => {
                self.testing.insert(a);
            }
            AcceptToken::Stable(a) => {
                self.stable.insert(a);
            }
            AcceptToken::Negate(a) => {
                self.stable.remove(&a);
            }
            AcceptToken::NegateTesting(a) => {
                self.testing.remove(&a);
            }
            AcceptToken::NegateAnyTesting => self.any_testing = false,
            AcceptToken::NegateAny => self.any = false,
        }
    }

    /// Portage `_getMissingKeywords` match: accept when some package keyword’s
    /// exact token is in the set, or a covering wildcard is granted.
    ///
    /// `~arch` on the accept side does **not** satisfy a stable package
    /// `arch` keyword (verified against Portage: missing list stays `['arm64']`
    /// when only `~arm64` is accepted).
    ///
    /// `downgrade` re-checks with every `Stable` package keyword treated as
    /// `Testing` first — Portage's `isStable` recheck (see [`Self::is_stable_for`]).
    fn accepts_inner(&self, keywords: &[Keyword], downgrade: bool) -> bool {
        if self.any {
            return true;
        }
        let mut has_stable = false;
        let mut has_testing = false;
        for kw in keywords {
            let stability = if downgrade && kw.stability == Stability::Stable {
                Stability::Testing
            } else {
                kw.stability
            };
            match stability {
                Stability::Stable => {
                    has_stable = true;
                    if self.stable.contains(&kw.arch) {
                        return true;
                    }
                }
                Stability::Testing => {
                    has_testing = true;
                    if self.testing.contains(&kw.arch) {
                        return true;
                    }
                }
                _ => {}
            }
        }
        (has_stable && self.any_stable) || (has_testing && self.any_testing)
    }

    fn accepts(&self, keywords: &[Keyword]) -> bool {
        self.accepts_inner(keywords, false)
    }

    /// Whether the package is accepted on a *stable* keyword path (gates
    /// profile `use.stable.{force,mask}`).
    ///
    /// Portage's `isStable` (`KeywordsManager.py`): accepted, AND *still*
    /// accepted after downgrading every stable package keyword to testing.
    /// If the downgraded set is still accepted, the accept set's own testing
    /// grant is what's really pulling the package in — nothing is being
    /// "forced" by stability, so it isn't stable for `use.stable.*` purposes.
    /// Concretely: `ACCEPT_KEYWORDS="arm64 ~arm64"` with `KEYWORDS="arm64"`
    /// is **not** stable (the `~arm64` grant alone already accepts it) — the
    /// opposite of what an earlier version of this comment claimed.
    fn is_stable_for(&self, keywords: &[Keyword]) -> bool {
        self.accepts_inner(keywords, false) && !self.accepts_inner(keywords, true)
    }
}

/// Global `ACCEPT_KEYWORDS` plus per-package `package.accept_keywords`.
///
/// # Layering (Portage)
///
/// 1. Global incremental `ACCEPT_KEYWORDS` (profile `make.defaults` usually
///    contributes `$ARCH`; make.conf often adds `~$ARCH`) — see make.conf(5).
/// 2. Per-package lines from profile + `/etc/portage/package.accept_keywords`
///    (+ legacy `package.keywords`) — portage(5): each line *augments* the
///    accept set for matching atoms; a bare atom expands to host `~arch`.
///
/// Effective set for a version = fold (1) then all matching lines of (2) in
/// file order (Portage also applies atom specificity ordering; we fold every
/// match, which is correct when lines do not contradict each other for the
/// same atom — the crossdev files on this project are one line per CP).
///
/// Host `arch` is **not** the accept universe: it is only used for
/// bare-atom expansion and autounmask `~arch` suggestions. All arch names in
/// tokens are stored in `KeywordAccept`.
pub struct AcceptKeywords {
    /// Host arch, interned — bare-atom expansion and autounmask `~arch` only.
    arch: Interned<DefaultInterner>,
    /// Precomputed decision from the global `ACCEPT_KEYWORDS` tokens.
    global: KeywordAccept,
    /// Per-package overrides: `(atom, [tokens])`. A bare atom is pre-expanded to
    /// `~arch` (portage(5)). Empty ⇒ no per-package work in the hot path.
    per_package: Vec<(Dep, Vec<AcceptToken>)>,
}

impl AcceptKeywords {
    /// Build from pre-parsed tokens: the global `ACCEPT_KEYWORDS` list and the
    /// per-package `(atom, tokens)` entries from `package.accept_keywords`.
    /// Tokens are already interned (parsed at config-read time); a bare
    /// per-package atom arrives as an empty token list and is expanded here to
    /// "accept this arch's `~arch`" (portage's rule).
    pub fn new(
        arch: &Arch,
        global: &[AcceptToken],
        per_package: Vec<(Dep, Vec<AcceptToken>)>,
    ) -> Self {
        let arch_key = Interned::intern(arch.as_str());
        let mut global_accept = KeywordAccept::default();
        for &tok in global {
            global_accept.add(tok);
        }
        // An empty global list means "stable only" (the historical fallback).
        if global.is_empty() {
            global_accept.stable.insert(arch_key);
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
    /// `#[doc(hidden)] pub`, not `#[cfg(test)]`: this is also called from
    /// `portage-cli`'s own tests (`c7.rs`, `host_copies.rs`), and `#[cfg(test)]`
    /// doesn't survive a crate boundary — it's only `true` while
    /// `portage-resolve` itself is under test, not for a downstream crate's
    /// tests that merely depend on it normally.
    #[doc(hidden)]
    pub fn from_global(arch: &Arch, global: &[&str]) -> Self {
        let toks: Vec<AcceptToken> = global
            .iter()
            .filter_map(|s| AcceptToken::parse(s))
            .collect();
        Self::new(arch, &toks, Vec::new())
    }

    /// The accept decision for one package version, folding any matching
    /// per-package overrides into the precomputed global decision. Borrows the
    /// global set when nothing matches (the common path).
    fn decision(
        &self,
        cpv: &Cpv,
        slot: Option<Interned<DefaultInterner>>,
    ) -> Cow<'_, KeywordAccept> {
        if self.per_package.is_empty() {
            return Cow::Borrowed(&self.global);
        }
        let slot = slot.map(portage_atom::Slot::from_name);
        let slot_str = slot.as_ref();
        let mut acc: Option<KeywordAccept> = None;
        for (dep, toks) in &self.per_package {
            if dep.matches_cpv(cpv, slot_str) {
                let set = acc.get_or_insert_with(|| self.global.clone());
                for &t in toks {
                    set.add(t);
                }
            }
        }
        match acc {
            Some(owned) => Cow::Owned(owned),
            None => Cow::Borrowed(&self.global),
        }
    }

    /// Whether `keywords` is accepted for this version.
    pub fn accepts(
        &self,
        keywords: &[Keyword],
        cpv: &Cpv,
        slot: Option<Interned<DefaultInterner>>,
    ) -> bool {
        self.decision(cpv, slot).accepts(keywords)
    }

    /// Whether this version is merged on a *stable* keyword path — gates the
    /// `use.stable.{force,mask}` sets. Depends on whether a testing grant is
    /// also present in the accept set: see `KeywordAccept::is_stable_for`
    /// (private, in this module).
    pub fn is_stable(
        &self,
        keywords: &[Keyword],
        cpv: &Cpv,
        slot: Option<Interned<DefaultInterner>>,
    ) -> bool {
        self.decision(cpv, slot).is_stable_for(keywords)
    }

    /// The keyword token an autounmask would need: the version is not accepted.
    /// Returns `~arch` when a testing keyword for the host arch is present,
    /// otherwise `**` for fully unkeyworded / foreign-arch-only versions so
    /// autounmask always has a concrete suggestion for a dropped hard dep.
    pub fn keyword_needed(
        &self,
        keywords: &[Keyword],
        cpv: &Cpv,
        slot: Option<Interned<DefaultInterner>>,
    ) -> Option<String> {
        if self.accepts(keywords, cpv, slot) {
            return None;
        }
        if keywords
            .iter()
            .any(|kw| kw.arch == self.arch && kw.stability == Stability::Testing)
        {
            Some(format!("~{}", self.arch.as_str()))
        } else {
            Some("**".to_string())
        }
    }
}

/// Global `ACCEPT_LICENSE` plus per-package `package.license`.
///
/// The global list applies to every package; per-package entries extend it
/// (allow more, or `-deny`) for versions whose atom matches. The common
/// no-override path borrows the global decision — no allocation.
pub struct AcceptOverlay {
    global: portage_repo::AcceptSet,
    /// `(atom, overlay)` — each overlay is the per-line tokens parsed into an
    /// `AcceptSet`, merged onto the global decision for matching versions.
    per_package: Vec<(Dep, portage_repo::AcceptSet)>,
}

impl AcceptOverlay {
    /// Build from the global `ACCEPT_LICENSE` decision plus per-package
    /// `package.license` overlays.
    pub fn new(
        global: portage_repo::AcceptSet,
        per_package: Vec<(Dep, portage_repo::AcceptSet)>,
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
    ) -> Cow<'_, portage_repo::AcceptSet> {
        if self.per_package.is_empty() {
            return Cow::Borrowed(&self.global);
        }
        let slot = slot.map(portage_atom::Slot::from_name);
        let slot_str = slot.as_ref();
        let mut merged: Option<portage_repo::AcceptSet> = None;
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

/// `ACCEPT_LICENSE` + `package.license` — the license-family name for
/// [`AcceptOverlay`] (three equally-named aliases of the same generic
/// engine; see [`AcceptProperties`]/[`AcceptRestrict`]).
pub type AcceptLicenses = AcceptOverlay;
/// `ACCEPT_PROPERTIES` + `package.properties`. Same shape as [`AcceptOverlay`],
/// reused verbatim rather than a hand-rolled third manager: both are "global
/// accept-list + per-package atom-matched overlay" over the identical
/// [`portage_repo::AcceptSet`] engine, constructed group-free via
/// [`portage_repo::AcceptSet::from_tokens_plain`] (`PROPERTIES`/`RESTRICT`
/// have no `@GROUP` concept).
pub type AcceptProperties = AcceptOverlay;
/// `ACCEPT_RESTRICT` + `package.accept_restrict` — see [`AcceptProperties`].
pub type AcceptRestrict = AcceptOverlay;

/// Returns true if the license expression is fully covered by `accept`,
/// evaluating `use? ( … )` branches against `enabled` (a package's effective
/// USE). For an expression with no conditionals, `enabled` is never consulted.
pub fn license_accepted(
    expr: &LicenseExpr,
    accept: &portage_repo::AcceptSet,
    enabled: &dyn Fn(&str) -> bool,
) -> bool {
    accept.accepts_expr(expr, enabled)
}

/// Collects the license names NOT covered by `accept`, evaluating conditionals
/// against `enabled` (see [`license_accepted`]).
fn licenses_needed(
    expr: &LicenseExpr,
    accept: &portage_repo::AcceptSet,
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

/// Build the `iuse_defaults` map `resolve_effective_use`/`PackageVersions`
/// need from an ebuild's parsed `IUSE` list — shared by every call site that
/// resolves a package's effective USE, so the `+`/`-` default conversion
/// lives in exactly one place.
///
/// Only **`+flag` / enabled** defaults are collected: disabled defaults match
/// `UseConfig::get`'s unset→Disabled, and `resolve_effective_use` only
/// overlays enabled IUSE flags not already set by `pre_env`.
///
/// A flag matching an active `USE_EXPAND` group (`l10n_af`, `video_cards_*`,
/// …) is excluded even when `+`-defaulted: PMS's USE_EXPAND rule makes
/// membership in the group's own variable authoritative, overriding the
/// ebuild's own IUSE default entirely. `chromium-2.eclass`'s
/// `_chromium_set_l10n_IUSE` sets `IUSE="+l10n_${lang}"` for every locale it
/// bundles, deliberately relying on this: "Default to enabled since we
/// bundle them anyway. USE-expansion will take care of disabling the langs
/// the user has not selected via L10N." Before this exclusion, `em` enabled
/// every locale the package shipped except the user's actual selection.
fn iuse_defaults_map(
    meta: &portage_metadata::EbuildMetadata,
    use_expand: &portage_repo::UseExpand,
) -> HashMap<Interned<DefaultInterner>, IUseDefault> {
    meta.iuse
        .iter()
        .filter_map(|iu| match iu.default {
            Some(portage_metadata::IUseDefault::Enabled)
                if use_expand.split(iu.name()).0 == "global" =>
            {
                Some((Interned::from(iu), IUseDefault::Enabled))
            }
            _ => None,
        })
        .collect()
}

/// Build the effective USE config for `cpv` from the resolved global USE,
/// per-version `package.use`, the ebuild's IUSE defaults and the profile
/// force/mask sets — the same layering `Adapter::desired_use` does, minus the
/// Level-C cede. Used to evaluate USE-conditional `LICENSE` expressions both in
/// the version filter and the autounmask reasons (which have no `Adapter`).
fn effective_use_config(
    policy: &ResolvePolicy,
    cpv: &Cpv,
    meta: &portage_metadata::EbuildMetadata,
    slot: Option<Interned<DefaultInterner>>,
) -> portage_atom_pubgrub::UseConfig {
    use portage_atom_pubgrub::resolve_effective_use;

    let iuse_defaults = iuse_defaults_map(meta, policy.use_expand);
    let mut cfg = resolve_effective_use(
        &iuse_defaults,
        policy.pre_env,
        cpv,
        slot,
        policy.package_use,
        policy.env_use,
        policy.profile_package_use,
    );
    if !policy.force_mask.is_empty() {
        let stable = policy.accept_keywords.is_stable(&meta.keywords, cpv, slot);
        let iuse: std::collections::HashSet<Interned<DefaultInterner>> =
            meta.iuse.iter().map(Interned::from).collect();
        let slot_dep = slot.map(portage_atom::Slot::from_name);
        policy
            .force_mask
            .apply(&mut cfg, cpv, slot_dep.as_ref(), stable, &iuse);
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
fn license_ok_for(
    cpv: &Cpv,
    meta: &portage_metadata::EbuildMetadata,
    policy: &ResolvePolicy,
) -> bool {
    let Some(lic) = &meta.license else {
        return true;
    };
    let slot = Some(meta.slot.slot);
    let accept = policy.accept_licenses.effective_for(cpv, slot);
    if !license_has_conditional(lic) {
        return license_accepted(lic, &accept, &|_| false);
    }
    let cfg = effective_use_config(policy, cpv, meta, slot);
    license_accepted(lic, &accept, &use_predicate(&cfg))
}

/// True if a `RESTRICT`/`PROPERTIES` value contains any `flag? ( … )`
/// conditional. Mirrors [`license_has_conditional`] for [`RestrictExpr`]'s
/// flatter shape (top level is an implicit AND-list, no `||` group).
fn restrict_has_conditional(entries: &[portage_metadata::RestrictExpr]) -> bool {
    use portage_metadata::RestrictExpr;
    entries.iter().any(|e| match e {
        RestrictExpr::Token(_) => false,
        RestrictExpr::UseConditional { .. } => true,
    })
}

/// Whether every entry of a `RESTRICT`/`PROPERTIES` value is accepted under
/// `accept`, evaluating conditionals against the version's effective USE only
/// when the value actually has any (same hot-path shortcut as `license_ok_for`).
fn restrict_family_ok_for(
    entries: &[portage_metadata::RestrictExpr],
    accept: &AcceptOverlay,
    cpv: &Cpv,
    meta: &portage_metadata::EbuildMetadata,
    policy: &ResolvePolicy,
) -> bool {
    if entries.is_empty() {
        return true;
    }
    let slot = Some(meta.slot.slot);
    let a = accept.effective_for(cpv, slot);
    if !restrict_has_conditional(entries) {
        return a.accepts_restrict(entries, &|_| false);
    }
    let cfg = effective_use_config(policy, cpv, meta, slot);
    a.accepts_restrict(entries, &use_predicate(&cfg))
}

/// Whether `meta`'s `PROPERTIES` is accepted for version `cpv` — see
/// [`restrict_family_ok_for`].
fn properties_ok_for(
    cpv: &Cpv,
    meta: &portage_metadata::EbuildMetadata,
    policy: &ResolvePolicy,
) -> bool {
    restrict_family_ok_for(
        &meta.properties,
        policy.accept_properties,
        cpv,
        meta,
        policy,
    )
}

/// Whether `meta`'s `RESTRICT` is accepted for version `cpv` — see
/// [`restrict_family_ok_for`].
fn restrict_ok_for(
    cpv: &Cpv,
    meta: &portage_metadata::EbuildMetadata,
    policy: &ResolvePolicy,
) -> bool {
    restrict_family_ok_for(&meta.restrict, policy.accept_restrict, cpv, meta, policy)
}

/// Check whether `mask_dep` matches the given `cpv` (version + CPN, no slot check).
/// Whether `cpv` (in `slot`) is masked: some mask atom matches and no unmask
/// atom does (`/etc/portage/package.unmask` cancels masks per package,
/// portage(5)).
pub fn is_masked(masks: &[Dep], unmasks: &[Dep], cpv: &Cpv, slot: &portage_atom::Slot) -> bool {
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

/// Whether `mask_dep`'s version constraint matches `cpv` (CPN + version op;
/// no slot check — see [`is_masked`] for the slot-aware caller).
pub fn mask_matches(mask_dep: &Dep, cpv: &Cpv) -> bool {
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

/// Every loaded package fact: the main repo's md5-cache plus overlay/alias
/// entries, ready for the solver bridge's [`Adapter`] to read.
pub struct RepoData {
    /// Every known `Cpn`, sorted.
    pub cpns: Vec<Cpn>,
    /// Every known version per `Cpn`, with its parsed cache entry.
    pub versions: HashMap<Cpn, Vec<(Cpv, CacheEntry)>>,
    /// The main repo's name; versions from overlays are recorded in `repo_of`.
    pub repo_name: String,
    /// Source repo of overlay-provided versions (absent ⇒ the main repo).
    pub repo_of: HashMap<Cpv, String>,
    /// Cross-derivation reverse map: `cross-<tuple>/<pkg>` → real `<cat>/<pkg>`.
    /// Populated by `load_repos` from `Location::Alias` entries; empty for
    /// non-cross solves. Used by `PlannedMerge.ebuild_path` construction
    /// (`mod.rs`) to find the real on-disk file for a derived cross cpv — but
    /// that's only half of the merge-path decoupling. `Ebuild::from_path`
    /// still re-derives CATEGORY from that real path's text, which loses the
    /// cross category on an actual merge — this should be resolved before
    /// wiring a real producer of `Location::Alias` entries.
    pub real_cpn_of: HashMap<Cpn, Cpn>,
}

/// The repo a version comes from (for `::repo` display and constraints).
pub fn repo_name_of<'a>(data: &'a RepoData, cpv: &Cpv) -> &'a str {
    data.repo_of
        .get(cpv)
        .map_or(data.repo_name.as_str(), String::as_str)
}

/// The resolved keyword/mask/license/USE policy, shared by every version
/// filter and USE-config computation (the `effective_use_config` function,
/// the `license_ok_for` function, `target_package`, `find_autounmask_candidates`,
/// and `download_size::compute`) so each takes one reference instead of
/// re-listing the same fields. The solver-specific remainder of [`Adapter`]
/// (`data`, `installed_cpvs`, `autosolve_use`) isn't part of this — those
/// three vary by call site in ways this shared policy doesn't. `Copy` so a
/// caller can build one base value and override just `package_use` per call
/// site (it's the one field that legitimately varies mid-resolution, e.g.
/// across a Level-C co-solve fixpoint) via `ResolvePolicy { package_use: X,
/// ..base }`.
#[derive(Clone, Copy)]
pub struct ResolvePolicy<'a> {
    /// Resolved `ACCEPT_KEYWORDS`/`package.accept_keywords` decision.
    pub accept_keywords: &'a AcceptKeywords,
    /// `package.mask` atoms.
    pub package_mask: &'a [Dep],
    /// `package.unmask` atoms (cancel a mask per package).
    pub package_unmask: &'a [Dep],
    /// Resolved `ACCEPT_LICENSE`/`package.license` decision.
    pub accept_licenses: &'a AcceptOverlay,
    /// Resolved `ACCEPT_PROPERTIES`/`package.properties` decision.
    pub accept_properties: &'a AcceptProperties,
    /// Resolved `ACCEPT_RESTRICT`/`package.accept_restrict` decision.
    pub accept_restrict: &'a AcceptRestrict,
    /// USE folded up through `make.conf` — see [`Adapter::pre_env`].
    pub pre_env: &'a portage_atom_pubgrub::UseLayer,
    /// Process-environment USE layer — see [`Adapter::env_use`].
    pub env_use: &'a portage_atom_pubgrub::UseLayer,
    /// Per-version `package.use`/`package.env`-style overrides from
    /// `/etc/portage` (the `pkg` layer, above `conf`/make.conf).
    pub package_use: &'a [(Dep, Vec<UseOverride>)],
    /// Profile-chain `package.use` — portage's *defaults* layer, BELOW
    /// `conf` (make.conf): a make.conf `USE=` decision wins over it (see
    /// `portage_atom_pubgrub::resolve_effective_use`). Contrast
    /// [`Self::package_use`].
    pub profile_package_use: &'a [(Dep, Vec<UseOverride>)],
    /// Profile USE force/mask policy — see [`Adapter::force_mask`].
    pub force_mask: &'a crate::force_mask::ForceMask,
    /// The profile's declared `USE_EXPAND` group names (`L10N`,
    /// `VIDEO_CARDS`, …) — see [`Adapter::use_expand`].
    pub use_expand: &'a UseExpand,
}

/// The [`portage_atom_pubgrub::PackageRepository`] impl the solver bridge
/// reads from — a borrowed view over [`RepoData`] plus every resolved policy
/// input (accept keywords/mask/license, USE layers, force/mask, Level-C).
pub struct Adapter<'a> {
    /// The loaded repository facts.
    pub data: &'a RepoData,
    /// Resolved `ACCEPT_KEYWORDS`/`package.accept_keywords` decision.
    pub accept_keywords: &'a AcceptKeywords,
    /// `package.mask` atoms.
    pub package_mask: &'a [Dep],
    /// `package.unmask` atoms (cancel a mask per package).
    pub package_unmask: &'a [Dep],
    /// Resolved `ACCEPT_LICENSE`/`package.license` decision.
    pub accept_licenses: &'a AcceptOverlay,
    /// Resolved `ACCEPT_PROPERTIES`/`package.properties` decision.
    pub accept_properties: &'a AcceptProperties,
    /// Resolved `ACCEPT_RESTRICT`/`package.accept_restrict` decision.
    pub accept_restrict: &'a AcceptRestrict,
    /// USE folded up through `make.conf` (profile make.defaults + extra
    /// confs) — everything below the `package.use`/`env` layers in portage's
    /// real fold order. Pre-parsed [`portage_atom_pubgrub::UseLayer`]; combined with `env_use` and
    /// per-version `package.use` + IUSE defaults by `desired_use`.
    pub pre_env: &'a portage_atom_pubgrub::UseLayer,
    /// Process-environment USE layer — highest priority, applied after
    /// `package.use` (see `resolve_effective_use`). Pre-parsed once.
    pub env_use: &'a portage_atom_pubgrub::UseLayer,
    /// Per-version `package.use`/`package.env`-style overrides from
    /// `/etc/portage` (the `pkg` layer, above `conf`/make.conf).
    pub package_use: &'a [(Dep, Vec<UseOverride>)],
    /// Profile-chain `package.use` — portage's *defaults* layer, below
    /// `conf`/make.conf (see [`ResolvePolicy::profile_package_use`]).
    pub profile_package_use: &'a [(Dep, Vec<UseOverride>)],
    /// Profile USE force/mask policy: applied to each version's effective USE and
    /// consulted by the Level-C cede gate (pinned flags are never ceded).
    pub force_mask: &'a crate::force_mask::ForceMask,
    /// The profile's declared `USE_EXPAND` group names (`L10N`,
    /// `VIDEO_CARDS`, …). A flag matching one of these prefixes is governed
    /// by its group's own variable, not by the ebuild's `+`/`-` IUSE
    /// default — see [`iuse_defaults_map`]'s doc for why.
    pub use_expand: &'a UseExpand,
    /// Exact installed cpvs. A version that is installed and staying installed
    /// never has its `REQUIRED_USE` flags ceded — its USE was decided at build
    /// time, and only packages being built get theirs auto-satisfied (emerge
    /// likewise leaves installed packages' constraints alone). See
    /// [`Self::rebuilding_cpvs`] for the exception: an installed cpv that is
    /// nonetheless being rebuilt this run.
    pub installed_cpvs: &'a std::collections::HashSet<Cpv>,
    /// Installed cpvs this run rebuilds anyway — non-selective explicit root
    /// targets (reinstalled `[R]` at their installed version, e.g. `em stages
    /// --stage1`'s `packages.build` atoms) and `-N`/`-U` USE-drift rebuilds.
    /// Level-C treats these as "being built": their `REQUIRED_USE` is
    /// re-decided for the new build's USE context, unlike a package that is
    /// installed and staying installed (see `installed_cpvs`'s doc comment,
    /// and commit `b919014` for why that distinction matters — config drift
    /// on a genuinely untouched installed package must not invent USE
    /// decisions). Mid-solve USE-dep-forced reinstalls and post-solve subslot
    /// rebuilds aren't known yet when this set is built, so those stay
    /// Level-A advisory only — a documented limitation, not a bug.
    pub rebuilding_cpvs: &'a std::collections::HashSet<Cpv>,
    /// Level-C: when set, cede each package's non-pinned `REQUIRED_USE` flags to
    /// the solver (`SolverDecided`) instead of fixing them. See
    /// `portage-atom-pubgrub/docs/required-use-level-c.md`.
    pub autosolve_use: bool,
}

impl Adapter<'_> {
    /// The shared version filter: keywords, package.mask/unmask, license.
    /// `versions_for` and `slots_for` must agree, or the slot map would carry
    /// phantom slots for versions the solver can never select.
    pub fn version_accepted(&self, cpv: &Cpv, cache: &portage_metadata::CacheEntry) -> bool {
        let meta = &cache.metadata;
        self.accept_keywords
            .accepts(&meta.keywords, cpv, Some(meta.slot.slot))
            && !is_masked(self.package_mask, self.package_unmask, cpv, &meta.slot)
            && self.license_ok(cpv, meta)
            && properties_ok_for(cpv, meta, &self.policy())
            && restrict_ok_for(cpv, meta, &self.policy())
    }

    /// The newest keyword/mask/license-accepted version of `cpn`, with its
    /// cache entry. `None` when the CPN is absent from the repo or has no
    /// accepted version. Centralizes the "newest accepted version" pick so a
    /// caller can't drift from `version_accepted` the way `host_copies`'s own
    /// inline copy once did (the `dev-vcs/git-9999` selection bug).
    pub fn newest_accepted(&self, cpn: Cpn) -> Option<(&Cpv, &CacheEntry)> {
        self.data
            .versions
            .get(&cpn)?
            .iter()
            .filter(|(cpv, cache)| self.version_accepted(cpv, cache))
            .max_by(|a, b| a.0.version.cmp(&b.0.version))
            .map(|(cpv, cache)| (cpv, cache))
    }

    /// License acceptance for a version, evaluating any `use? ( … )` LICENSE
    /// branch against the version's effective USE (computed only when the
    /// expression actually has conditionals).
    fn license_ok(&self, cpv: &Cpv, meta: &portage_metadata::EbuildMetadata) -> bool {
        license_ok_for(cpv, meta, &self.policy())
    }

    /// Level-C cede: when `--autosolve-use` is on and the package's REQUIRED_USE
    /// is *violated* by the resolved config, hand its REQUIRED_USE flags to the
    /// solver as preferences (`solver_decide`) — a ceded flag keeps its resolved
    /// value as the preference (greedy keep-config) and the solver only flips it
    /// to satisfy REQUIRED_USE. Flags the user pinned via package.use, or the
    /// profile forced/masked, are left fixed (hard choices we must not override).
    ///
    /// When the resolved config has the flag **off**, but the ebuild's IUSE
    /// carries a `+` default (wiped by conf-level `USE=-*`, as stage1 does),
    /// prefer **on** instead of "keep off". That biases exclusive groups
    /// (`^^`, `app-alternatives.eclass`) toward the eclass default provider
    /// rather than an arbitrary member when every option was cleared.
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
        use portage_atom_pubgrub::{UseFlagState, resolve_effective_use};

        if self.installed_cpvs.contains(cpv) && !self.rebuilding_cpvs.contains(cpv) {
            return;
        }
        let Some(ru) = &m.required_use else {
            return;
        };
        let enabled = |flag: &str| matches!(cfg.get(Interned::intern(flag)), UseFlagState::Enabled);
        let unsatisfied = ru.unsatisfied(&enabled);
        if unsatisfied.is_empty() {
            return;
        }

        // Flags the user pinned via package.use: folding it against an empty
        // base (no IUSE defaults, no pre_env, no env_use) leaves exactly
        // those flags set.
        let empty = portage_atom_pubgrub::UseLayer::default();
        let pins = resolve_effective_use(
            &HashMap::new(),
            &empty,
            cpv,
            slot,
            self.package_use,
            &empty,
            self.profile_package_use,
        );
        let iuse: std::collections::HashSet<&str> = m.iuse.iter().map(|iu| iu.name()).collect();
        let iuse_flags: std::collections::HashSet<Interned<DefaultInterner>> =
            m.iuse.iter().map(Interned::from).collect();
        // Flags pinned by use.force/use.mask (global, package-level and the stable
        // variants): hard profile decisions, never ceded.
        let slot_dep = slot.map(portage_atom::Slot::from_name);
        let forced_masked = self
            .force_mask
            .pins(cpv, slot_dep.as_ref(), stable, &iuse_flags);
        // Only flags mentioned in the *violated* clause(s), not the whole
        // REQUIRED_USE tree: a package can have several independent top-level
        // clauses (e.g. util-linux's `python? ( ... ) su? ( pam )`), and one
        // failing must not cede flags belonging to an unrelated, independently
        // satisfied clause — that gratuitously turns an already-decided flag
        // into a solver-owned virtual choice node, which can fabricate a
        // dependency edge that doesn't reflect the real (settled) USE state.
        // `python` too, spuriously wiring it to dev-lang/python and creating a
        // fake install-order cycle.
        let mut names = std::collections::BTreeSet::new();
        for clause in &unsatisfied {
            collect_required_use_flags(clause, &mut names);
        }
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
            // Keep-config when already on; if off, still prefer on when the
            // ebuild's IUSE default is `+flag` (stage1 `-*` wipe recovery).
            let prefer = matches!(cfg.get(flag), UseFlagState::Enabled)
                || m.iuse
                    .iter()
                    .any(|iu| iu.name() == name.as_str() && iu.is_enabled_default());
            cfg.solver_decide(flag, prefer);
        }
    }
}

impl<'a> Adapter<'a> {
    /// Borrow out the policy fields, for the free functions that don't need
    /// the rest of `Adapter` (`data`/`installed_cpvs`/`autosolve_use`).
    pub fn policy(&self) -> ResolvePolicy<'a> {
        ResolvePolicy {
            accept_keywords: self.accept_keywords,
            package_mask: self.package_mask,
            package_unmask: self.package_unmask,
            accept_licenses: self.accept_licenses,
            accept_properties: self.accept_properties,
            accept_restrict: self.accept_restrict,
            pre_env: self.pre_env,
            env_use: self.env_use,
            use_expand: self.use_expand,
            package_use: self.package_use,
            profile_package_use: self.profile_package_use,
            force_mask: self.force_mask,
        }
    }
}

/// `EbuildMetadata`'s five dependency classes, straight into `PackageDeps`.
///
/// Each field on both sides is a `DepList` (`Arc`-wrapped), so this is five
/// refcount bumps, not a deep clone — cannot be a `From` impl on either
/// side: `portage-atom-pubgrub` deliberately stays free of
/// `portage-metadata` (see the `required_use` translation below), and
/// `portage-cli` owns neither type, so an orphan-rule-legal trait impl
/// isn't available here — a plain function is the direct route.
fn package_deps_from_metadata(meta: &portage_metadata::EbuildMetadata) -> PackageDeps {
    PackageDeps::new(
        meta.depend.clone(),
        meta.rdepend.clone(),
        meta.bdepend.clone(),
        meta.pdepend.clone(),
        meta.idepend.clone(),
    )
}

impl PackageRepository for Adapter<'_> {
    fn all_packages(&self) -> Vec<Cpn> {
        self.data.cpns.clone()
    }

    fn desired_use(&self, cpv: &Cpv) -> portage_atom_pubgrub::UseConfig {
        use portage_atom_pubgrub::resolve_effective_use;

        let meta = self
            .data
            .versions
            .get(&cpv.cpn)
            .and_then(|entries| entries.iter().find(|(c, _)| c.version == cpv.version))
            .map(|(_, cache)| &cache.metadata);

        let slot = meta.map(|m| m.slot.slot);

        // Caller-resolved policy: the full ordered fold (IUSE defaults, then
        // pre_env, then package.use, then env_use) → the authoritative
        // desired set.
        let iuse_defaults = meta
            .map(|m| iuse_defaults_map(m, self.use_expand))
            .unwrap_or_default();
        let mut cfg = resolve_effective_use(
            &iuse_defaults,
            self.pre_env,
            cpv,
            slot,
            self.package_use,
            self.env_use,
            self.profile_package_use,
        );

        // Profile USE force/mask override package.use and the configured value
        // (Portage semantics), applied as the final post-fold step, matching
        // real portage's `use.force`/`use.mask` (outside the layer stack).
        // This layers the package-level sets plus the *.stable.* sets, the
        // latter only when this version is merged due to a stable keyword.
        // This is what makes crossdev's package.use.force/mask (multilib/cet/…)
        // take effect on cross-* packages.
        let stable = meta.is_some_and(|m| {
            self.accept_keywords
                .is_stable(&m.keywords, cpv, Some(m.slot.slot))
        });
        if !self.force_mask.is_empty() {
            let iuse: std::collections::HashSet<Interned<DefaultInterner>> = meta
                .map(|m| m.iuse.iter().map(Interned::from).collect())
                .unwrap_or_default();
            let slot_dep = slot.map(portage_atom::Slot::from_name);
            self.force_mask
                .apply(&mut cfg, cpv, slot_dep.as_ref(), stable, &iuse);
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
                        // `Interned::from(iu)` wraps the key `CacheEntry::parse`
                        // already produced (same `DefaultInterner`) — zero-cost.
                        // `Interned::intern(iu.name())` would instead resolve that
                        // key back to a `&str` just to look the same string up
                        // again, paying a full interner resolve+get_or_intern
                        // round trip (each pinning an epoch handle on the
                        // lock-free backend) for a result identical to the key
                        // already in hand.
                        let iuse: Vec<Interned<DefaultInterner>> =
                            meta.iuse.iter().map(Interned::from).collect();
                        let iuse_defaults = iuse_defaults_map(meta, self.use_expand);
                        let deps = package_deps_from_metadata(meta);
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

/// Load every repo's metadata (sourcing cache-less ebuilds — see
/// `portage_repo::repo_entries`), merged in `set`'s priority order: higher
/// priority wins a duplicate cpv, matching real portage (`porttree.py`'s
/// `findname2`/`xmatch` `reversed(...)` over the ascending `(priority,
/// name)` list; `man 5 portage`: "those with higher priority are
/// preferred"). The main repo defaults to priority `-1000` when unset
/// (`ReposConf::load_from`), so by default *any* overlay shadows it — the
/// ordinary "drop an ebuild in a local overlay to override ::gentoo"
/// workflow, no repos.conf configuration required.
///
/// Overlay secondary caches must already be configured on each
/// [`portage_repo::Repository`] (via [`portage_repo::RepositoryBuilder`]).
pub async fn load_repos(set: &portage_repo::RepoSet) -> RepoData {
    let mut cpns_set: HashSet<Cpn> = HashSet::new();
    let mut versions: HashMap<Cpn, Vec<(Cpv, CacheEntry)>> = HashMap::new();
    let mut repo_of: HashMap<Cpv, String> = HashMap::new();
    let mut seen: HashSet<Cpv> = HashSet::new();
    let mut real_cpn_of: HashMap<Cpn, Cpn> = HashMap::new();

    for (i, source_repo) in set.iter().enumerate() {
        for (cpv, entry) in portage_repo::repo_entries(source_repo).await {
            if !seen.insert(cpv.clone()) {
                continue;
            }
            cpns_set.insert(cpv.cpn);
            // No `repo_of` insert for the main repo: absence means "the
            // main repo", per `RepoData::repo_of`'s documented convention.
            if i != set.main_index() {
                repo_of.insert(cpv.clone(), source_repo.name().to_string());
            }
            versions.entry(cpv.cpn).or_default().push((cpv, entry));
        }
    }

    // Inject alias (virtual) repos: for each Location::Alias, clone the source
    // repo's versions for each aliased package under the destination category.
    // This is the in-memory equivalent of crossdev's symlink overlay — no
    // on-disk tree needed.
    // Collect first, then inject, to avoid borrowing `versions` while mutating.
    type CrossInject = (String, Cpn, Vec<(Cpv, CacheEntry)>);
    let mut cross_inject: Vec<CrossInject> = Vec::new();
    for entry in set.aliases() {
        let portage_repo::Location::Alias { source, aliases } = &entry.location else {
            continue;
        };
        // Only the main repo is a supported alias source today — `versions`
        // doesn't track per-cpn origin for main-repo entries (only overlays
        // get a `repo_of` entry), so there's no way to disambiguate a
        // same-named cpn coming from elsewhere.
        if source != set.main().name() {
            continue;
        }
        for (dest_cat, source_cpns) in aliases {
            let dest_cat_interned = Interned::<DefaultInterner>::intern(dest_cat.as_str());
            for source_cpn in source_cpns {
                let Some(real_entries) = versions.get(source_cpn) else {
                    continue; // source package not in the loaded repos
                };
                let cross_cpn = Cpn::new(dest_cat_interned, source_cpn.package);
                real_cpn_of.insert(cross_cpn, *source_cpn);
                cpns_set.insert(cross_cpn);
                let copies: Vec<(Cpv, CacheEntry)> = real_entries
                    .iter()
                    .map(|(cpv, cache)| (Cpv::new(cross_cpn, cpv.version.clone()), cache.clone()))
                    .collect();
                cross_inject.push((entry.name.clone(), cross_cpn, copies));
            }
        }
    }
    for (repo_name, cross_cpn, copies) in cross_inject {
        for (cross_cpv, cache) in copies {
            if !seen.insert(cross_cpv.clone()) {
                continue;
            }
            repo_of.insert(cross_cpv.clone(), repo_name.clone());
            versions
                .entry(cross_cpn)
                .or_default()
                .push((cross_cpv, cache));
        }
    }

    let mut cpns: Vec<Cpn> = cpns_set.into_iter().collect();
    // `Cpn: Ord` already compares category then package over the interned
    // strings — alphabetical, no per-comparison allocation.
    cpns.sort_unstable();

    RepoData {
        cpns,
        versions,
        repo_name: set.main().name().to_string(),
        repo_of,
        real_cpn_of,
    }
}

/// Map a dep atom to a `PortagePackage` for the solver.
pub fn target_package(
    data: &RepoData,
    dep: &Dep,
    policy: &ResolvePolicy,
) -> portage_atom_pubgrub::PortagePackage {
    let Some(entries) = data.versions.get(&dep.cpn) else {
        return portage_atom_pubgrub::PortagePackage::unslotted(dep.cpn);
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
            dep.matches_cpv(cpv, Some(&cache.metadata.slot))
                && policy.accept_keywords.accepts(
                    &cache.metadata.keywords,
                    cpv,
                    Some(cache.metadata.slot.slot),
                )
                && !is_masked(
                    policy.package_mask,
                    policy.package_unmask,
                    cpv,
                    &cache.metadata.slot,
                )
                && license_ok_for(cpv, &cache.metadata, policy)
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
pub fn cpns_for(data: &RepoData, cpn: &Cpn, ver: &Version) -> Vec<Cpn> {
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

/// Look up `(pkg, ver)`'s parsed cache entry in `data`, if present.
pub fn find_cache<'a>(
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

/// Why a version was filtered, as displayed text — one phrase per reason.
pub fn filter_reason_text(reasons: &[FilterReason]) -> String {
    reasons
        .iter()
        .map(|r| match r {
            // `keyword_needed` reports `**` when the arch has no keyword at all.
            FilterReason::Keyword(kw) if kw == "**" => "missing keyword".to_string(),
            FilterReason::Keyword(kw) => format!("{kw} keyword"),
            FilterReason::Masked => "package.mask".to_string(),
            FilterReason::License(l) => format!("{} license(s)", l.join(" ")),
            FilterReason::Properties(p) => format!("{} propert(y/ies)", p.join(" ")),
            FilterReason::Restrict(r) => format!("{} restriction(s)", r.join(" ")),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// [`filter_reasons_for`] narrowed to the versions `dep` itself matches.
///
/// `filter_reasons_for` applies the version range only, so a slot-qualified
/// atom like `cat/pkg:6.18.12` would otherwise report unrelated slots' versions
/// as its own rejected candidates. An empty result means nothing the atom
/// matches exists at all — the caller's "no ebuilds" case, as against "every
/// candidate was filtered".
pub fn filter_reasons_for_atom(
    data: &RepoData,
    dep: &Dep,
    version_set: &portage_atom_pubgrub::PortageVersionSet,
    policy: &ResolvePolicy,
) -> Vec<AutounmaskCandidate> {
    filter_reasons_for(data, &dep.cpn, version_set, policy)
        .into_iter()
        .filter(|c| {
            let slot = c.slot.map(portage_atom::Slot::from_name);
            dep.matches_cpv(&c.cpv, slot.as_ref())
        })
        .collect()
}

/// Every version of `cpn` within `version_set` that the policy excludes, with
/// the reasons (keyword / mask / license). Versions that pass every filter are
/// absent, so an empty result means the atom is satisfiable.
pub fn filter_reasons_for(
    data: &RepoData,
    cpn: &Cpn,
    version_set: &portage_atom_pubgrub::PortageVersionSet,
    policy: &ResolvePolicy,
) -> Vec<AutounmaskCandidate> {
    let Some(entries) = data.versions.get(cpn) else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    for (cpv, cache) in entries {
        if !version_set.contains(&cpv.version) {
            continue;
        }
        let meta = &cache.metadata;
        let slot = Some(meta.slot.slot);

        let mut reasons = Vec::new();

        if let Some(kw) = policy
            .accept_keywords
            .keyword_needed(&meta.keywords, cpv, slot)
        {
            reasons.push(FilterReason::Keyword(kw));
        }
        if is_masked(policy.package_mask, policy.package_unmask, cpv, &meta.slot) {
            reasons.push(FilterReason::Masked);
        }
        if let Some(lic) = &meta.license {
            let accept = policy.accept_licenses.effective_for(cpv, slot);
            // Evaluate `use? ( … )` branches against the version's effective
            // USE so a non-FREE license behind a disabled flag is not flagged.
            let needed = if license_has_conditional(lic) {
                let cfg = effective_use_config(policy, cpv, meta, slot);
                licenses_needed(lic, &accept, &use_predicate(&cfg))
            } else {
                licenses_needed(lic, &accept, &|_| false)
            };
            if !needed.is_empty() {
                reasons.push(FilterReason::License(needed));
            }
        }
        if !meta.properties.is_empty() {
            let accept = policy.accept_properties.effective_for(cpv, slot);
            let needed = if restrict_has_conditional(&meta.properties) {
                let cfg = effective_use_config(policy, cpv, meta, slot);
                accept.restrict_needed(&meta.properties, &use_predicate(&cfg))
            } else {
                accept.restrict_needed(&meta.properties, &|_| false)
            };
            if !needed.is_empty() {
                reasons.push(FilterReason::Properties(needed));
            }
        }
        if !meta.restrict.is_empty() {
            let accept = policy.accept_restrict.effective_for(cpv, slot);
            let needed = if restrict_has_conditional(&meta.restrict) {
                let cfg = effective_use_config(policy, cpv, meta, slot);
                accept.restrict_needed(&meta.restrict, &use_predicate(&cfg))
            } else {
                accept.restrict_needed(&meta.restrict, &|_| false)
            };
            if !needed.is_empty() {
                reasons.push(FilterReason::Restrict(needed));
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
    candidates
}

/// For each dropped dep, find versions in the unfiltered repo that match its
/// version range and determine why they were excluded.
pub fn find_autounmask_candidates(
    data: &RepoData,
    dropped: &[DroppedDep],
    policy: &ResolvePolicy,
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
        candidates.extend(filter_reasons_for(
            data,
            dep.package.cpn(),
            &dep.version_set,
            policy,
        ));
    }

    // A CPV may appear from multiple DroppedDep entries; its reasons are
    // determined solely by its own metadata, so we keep the first occurrence.
    let mut seen: HashSet<String> = HashSet::new();
    candidates.retain(|c| seen.insert(c.cpv.to_string()));

    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::force_mask::{ForceMask, index_by_cpn};
    use portage_atom_pubgrub::{PackageRepository, UseFlagState};
    use portage_repo::{AcceptSet, LicenseGroupRegistry, Repository};

    fn accept_all_licenses() -> AcceptSet {
        AcceptSet::from_tokens(&["*".into()], &LicenseGroupRegistry::default())
    }

    fn dep(s: &str) -> Dep {
        Dep::parse(s).unwrap()
    }

    /// vscode-shaped (`chromium-2.eclass`'s `_chromium_set_l10n_IUSE`):
    /// `IUSE="+l10n_af +normal"`, `l10n_af` deliberately `+`-defaulted per
    /// the eclass's own comment ("Default to enabled since we bundle them
    /// anyway. USE-expansion will take care of disabling the langs the
    /// user has not selected via L10N"). Once `L10N` is an active
    /// USE_EXPAND group, PMS makes membership in its own expansion
    /// authoritative for every `l10n_*` flag — the ebuild's own `+`
    /// default must not auto-enable it. An ordinary (non-USE_EXPAND)
    /// `+`-defaulted flag is unaffected.
    #[test]
    fn iuse_defaults_map_excludes_use_expand_governed_enabled_defaults() {
        let entry =
            CacheEntry::parse("EAPI=8\nDESCRIPTION=t\nSLOT=0\nIUSE=+l10n_af +normal\n").unwrap();
        let use_expand = portage_repo::UseExpand::new(["l10n"]);
        let defaults = iuse_defaults_map(&entry.metadata, &use_expand);
        assert!(
            !defaults.contains_key(&Interned::intern("l10n_af")),
            "a USE_EXPAND-governed +default must not auto-enable, got {defaults:?}"
        );
        assert_eq!(
            defaults.get(&Interned::intern("normal")),
            Some(&IUseDefault::Enabled),
            "an ordinary +default flag must still enable"
        );
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

        // use.stable.* gates on whether the package is *only* accepted via a
        // stable keyword — Portage's `isStable`: accepted, and no longer
        // accepted once every stable keyword is downgraded to testing.
        assert!(global.is_stable(&stable, &foo, None));
        assert!(!per.is_stable(&testing, &foo, None));
        // Host accepts both arm64 and ~arm64 (the common `ACCEPT_KEYWORDS=
        // "arch ~arch"` stack): the ~arm64 grant alone would already accept a
        // stable-keyworded package, so it is NOT "stable" for use.stable.*
        // purposes — matches Portage, not the naive "package has a granted
        // stable keyword" reading an earlier version of this test asserted.
        let both = AcceptKeywords::from_global(&arch, &["arm64", "~arm64"]);
        assert!(!both.is_stable(&stable, &foo, None));
        assert!(!both.is_stable(&testing, &foo, None));

        // Wildcards: Portage's downgrade recheck is never defeated by `**`
        // (accepts everything either way) or `~*` (downgraded-to-testing set
        // is still covered) — only a *specific* stable grant with no
        // matching testing/wildcard rescue makes a package "stable".
        let any = AcceptKeywords::from_global(&arch, &["**"]);
        assert!(!any.is_stable(&stable, &foo, None));
        // "arm64" alone accepts+downgrade-rescues nothing; adding "~*" makes
        // the downgraded (all-testing) recheck accepted too, so no longer stable.
        let any_testing = AcceptKeywords::from_global(&arch, &["arm64", "~*"]);
        assert!(any_testing.accepts(&stable, &foo, None));
        assert!(!any_testing.is_stable(&stable, &foo, None));
        let any_stable = AcceptKeywords::from_global(&arch, &["*"]);
        assert!(any_stable.is_stable(&stable, &foo, None));
    }

    /// Crossdev's usual `package.accept_keywords` shape: grant the *target*
    /// arch and withdraw the host arch so a package keyworded `~riscv` (and
    /// often also `~arm64` from the mirrored ebuild) is accepted via riscv,
    /// not rejected after `-arm64 -~arm64` clears the host grants.
    ///
    /// Regression for the host-arch-only `ArchAccept` bitfield, which dropped
    /// foreign-arch tokens as no-ops and left only the host vetoes.
    #[test]
    fn accept_keywords_foreign_arch_crossdev_idiom() {
        let host = Arch::intern("arm64");
        let tok = |s: &str| AcceptToken::parse(s).unwrap();
        let kws = |s: &str| Keyword::parse_line(s).unwrap();
        // Real linux-headers KEYWORDS shape (abbreviated): testing on many
        // arches including host and target.
        let keywords = kws("~arm64 ~riscv ~amd64");
        let cpv = Cpv::parse("cross-riscv64-unknown-linux-gnu/linux-headers-7.1").unwrap();

        let ak = AcceptKeywords::new(
            &host,
            &[tok("arm64"), tok("~arm64")],
            vec![(
                dep("cross-riscv64-unknown-linux-gnu/linux-headers"),
                vec![tok("riscv"), tok("~riscv"), tok("-arm64"), tok("-~arm64")],
            )],
        );
        assert!(
            ak.accepts(&keywords, &cpv, None),
            "must accept via ~riscv after host arm64/~arm64 are withdrawn"
        );
        // Without the foreign-arch grant, the same vetoes fully mask it.
        let veto_only = AcceptKeywords::new(
            &host,
            &[tok("arm64"), tok("~arm64")],
            vec![(
                dep("cross-riscv64-unknown-linux-gnu/linux-headers"),
                vec![tok("-arm64"), tok("-~arm64")],
            )],
        );
        assert!(
            !veto_only.accepts(&keywords, &cpv, None),
            "host vetoes alone must mask when only ~arm64/~riscv remain"
        );
        // Unrelated package keeps the global ~arm64 accept.
        let other = Cpv::parse("sys-fs/btrfs-progs-7.1").unwrap();
        assert!(ak.accepts(&kws("~arm64"), &other, None));
    }

    /// `is_stable`'s downgrade recheck must run over the *whole* accept set,
    /// not per-token — a crossdev target line granting both `riscv` and
    /// `~riscv` isn't stable (the `~riscv` grant alone would already accept
    /// a downgraded package), but a stable-only target grant is.
    #[test]
    fn accept_keywords_stable_gate_respects_crossdev_target_grants() {
        let host = Arch::intern("arm64");
        let tok = |s: &str| AcceptToken::parse(s).unwrap();
        let kws = |s: &str| Keyword::parse_line(s).unwrap();
        let cpv = Cpv::parse("cross-riscv64-unknown-linux-gnu/linux-headers-7.1").unwrap();
        // A package keyworded stable on the target arch, testing on host.
        let keywords = kws("~arm64 riscv");

        let both_grants = AcceptKeywords::new(
            &host,
            &[tok("arm64"), tok("~arm64")],
            vec![(
                dep("cross-riscv64-unknown-linux-gnu/linux-headers"),
                vec![tok("riscv"), tok("~riscv"), tok("-arm64"), tok("-~arm64")],
            )],
        );
        assert!(both_grants.accepts(&keywords, &cpv, None));
        assert!(!both_grants.is_stable(&keywords, &cpv, None));

        let stable_only_grant = AcceptKeywords::new(
            &host,
            &[tok("arm64"), tok("~arm64")],
            vec![(
                dep("cross-riscv64-unknown-linux-gnu/linux-headers"),
                vec![tok("riscv"), tok("-arm64"), tok("-~arm64")],
            )],
        );
        assert!(stable_only_grant.accepts(&keywords, &cpv, None));
        assert!(stable_only_grant.is_stable(&keywords, &cpv, None));
    }

    #[test]
    fn accept_keywords_dash_star_clears_global() {
        let arch = Arch::intern("arm64");
        let tok = |s: &str| AcceptToken::parse(s).unwrap();
        let kws = |s: &str| Keyword::parse_line(s).unwrap();
        let testing = kws("~arm64");
        let offarch = kws("x86"); // wrong arch ⇒ only accepted via `**`/`*`
        let foo = Cpv::parse("dev-libs/foo-1").unwrap();

        // `**` alone accepts even a wrong-arch package. `-*` clears that, so the
        // trailing `~arm64` rebuilds testing+stable for arm64 only — the wrong
        // arch is no longer accepted.
        let star_star = AcceptKeywords::from_global(&arch, &["**"]);
        assert!(
            star_star.accepts(&offarch, &foo, None),
            "** accepts any keyword"
        );
        let acc = AcceptKeywords::from_global(&arch, &["**", "-*", "~arm64"]);
        assert!(
            acc.accepts(&testing, &foo, None),
            "~arm64 re-added after -*"
        );
        assert!(
            !acc.accepts(&offarch, &foo, None),
            "** was cleared by -*, so a wrong-arch keyword is rejected again"
        );
        assert!(AcceptToken::parse("-*").is_some(), "-* parses, not dropped");

        // Per-package `-*` resets the global decision for that package only.
        let bar = Cpv::parse("dev-libs/bar-1").unwrap();
        let per = AcceptKeywords::new(
            &arch,
            &[tok("**")],
            vec![(dep("dev-libs/foo"), vec![tok("-*")])],
        );
        assert!(
            !per.accepts(&offarch, &foo, None),
            "foo: global ** cleared by the per-package -*"
        );
        assert!(
            per.accepts(&offarch, &bar, None),
            "bar keeps the global ** accept"
        );
    }

    #[test]
    fn accept_keywords_negate_arch_withdraws_a_specific_grant() {
        let arch = Arch::intern("riscv");
        let kws = |s: &str| Keyword::parse_line(s).unwrap();
        let stable = kws("riscv");
        let testing = kws("~riscv");
        let foo = Cpv::parse("dev-libs/foo-1").unwrap();

        // `arch -arch`: the veto comes after the grant in the same stack (the
        // shape a downstream profile uses to withdraw an ancestor's `arch`).
        let vetoed = AcceptKeywords::from_global(&arch, &["riscv", "-riscv"]);
        assert!(!vetoed.accepts(&stable, &foo, None), "the veto must win");

        // Order matters, same as `-*`: a grant *after* the veto still applies.
        let regranted = AcceptKeywords::from_global(&arch, &["riscv", "-riscv", "~riscv"]);
        assert!(
            regranted.accepts(&testing, &foo, None),
            "a later token re-grants after an earlier veto"
        );

        // `-riscv` must not affect a different arch.
        let other_arch = AcceptKeywords::from_global(&Arch::intern("amd64"), &["-riscv", "amd64"]);
        assert!(other_arch.accepts(&kws("amd64"), &foo, None));

        assert!(
            AcceptToken::parse("-riscv").is_some(),
            "-arch now parses instead of being silently dropped"
        );
    }

    /// Answers the open question left after the `-arch`/`-~arch` fix: does
    /// `-arch` retract a *wildcard* grant (`*`/`**`) for that one arch, or
    /// only a specific bare `arch`/`~arch` token? Real Portage's matcher
    /// works over a flat set of literal strings — `-riscv` only ever
    /// discards a literal `riscv` token if present, and is inert against a
    /// separate `*`/`**` token, which stays granted. `KeywordAccept::add`
    /// already matches this exactly (each token clears only its own set
    /// membership — `Negate` removes a stable arch, never `any_stable`/
    /// `any`), so this was already correct once the fold stopped joining
    /// tokens together; this test just makes that answer explicit and locks
    /// it in.
    #[test]
    fn negate_arch_does_not_retract_a_wildcard_grant() {
        let arch = Arch::intern("riscv");
        let kws = |s: &str| Keyword::parse_line(s).unwrap();
        let stable = kws("riscv");
        let testing = kws("~riscv");
        let foo = Cpv::parse("dev-libs/foo-1").unwrap();

        // `* -riscv`: real Portage still accepts riscv here, since `-riscv`
        // discards the literal `riscv` token, not the separate `*` token.
        let star_then_veto = AcceptKeywords::from_global(&arch, &["*", "-riscv"]);
        assert!(
            star_then_veto.accepts(&stable, &foo, None),
            "-arch must not retract a `*` grant for the same arch"
        );

        // `** -riscv`: same reasoning, `**` is untouched by `-riscv`.
        let any_then_veto = AcceptKeywords::from_global(&arch, &["**", "-riscv"]);
        assert!(
            any_then_veto.accepts(&testing, &foo, None),
            "-arch must not retract a `**` grant for the same arch"
        );

        // The only way to actually exclude one arch from an otherwise
        // wildcard-accepting config is `-*` (clear-all) followed by
        // re-granting everything except it — there is no narrower "retract
        // just this arch from the wildcard" token, matching upstream.
        let star_reset_without_riscv = AcceptKeywords::from_global(&arch, &["**", "-*", "amd64"]);
        assert!(!star_reset_without_riscv.accepts(&stable, &foo, None));
    }

    /// Regression:
    /// `package.accept_keywords`'s own documented "pin to stable" idiom
    /// (`portage(5)`: `media-video/mplayer -~x86`) needs `-~arch` to withdraw
    /// *only* the testing grant, leaving the stable one intact. An earlier
    /// version of `AcceptToken::parse` stripped the `~` and treated `-~x86`
    /// identically to `-x86`, so this idiom fully masked the package instead
    /// of pinning it to stable.
    #[test]
    fn negate_testing_pins_a_package_to_stable_without_masking_it() {
        let arch = Arch::intern("x86");
        let kws = |s: &str| Keyword::parse_line(s).unwrap();
        let stable = kws("x86");
        let testing = kws("~x86");
        let foo = Cpv::parse("media-video/mplayer-1").unwrap();

        // System-wide: testing enabled. Per-package: pin mplayer to stable.
        let ak = AcceptKeywords::new(
            &arch,
            &[
                AcceptToken::parse("x86").unwrap(),
                AcceptToken::parse("~x86").unwrap(),
            ],
            vec![(
                dep("media-video/mplayer"),
                vec![AcceptToken::parse("-~x86").unwrap()],
            )],
        );
        assert!(
            ak.accepts(&stable, &foo, None),
            "stable mplayer must still be accepted"
        );
        assert!(
            !ak.accepts(&testing, &foo, None),
            "-~x86 must withdraw the testing grant for mplayer specifically"
        );

        // Confirm the system-wide default (no override) still accepts testing —
        // the veto must be scoped to mplayer, not global.
        let bar = Cpv::parse("dev-libs/bar-1").unwrap();
        assert!(ak.accepts(&testing, &bar, None), "bar keeps testing");
    }

    /// `-~*` and `-**` are their own tokens, not swallowed by the generic
    /// `-arch` branch (which previously interned "*"/"**' as bogus arch names,
    /// making them permanently inert since no real arch is ever named "*").
    #[test]
    fn negate_any_testing_and_negate_any_are_not_inert() {
        let arch = Arch::intern("amd64");
        let kws = |s: &str| Keyword::parse_line(s).unwrap();
        let offarch_stable = kws("x86");
        let offarch_testing = kws("~x86");
        let foo = Cpv::parse("dev-libs/foo-1").unwrap();

        let any_testing_then_negated = AcceptKeywords::from_global(&arch, &["~*", "-~*", "amd64"]);
        assert!(
            !any_testing_then_negated.accepts(&offarch_testing, &foo, None),
            "-~* must withdraw the ~* grant"
        );

        let any_then_negated = AcceptKeywords::from_global(&arch, &["**", "-**", "amd64"]);
        assert!(
            !any_then_negated.accepts(&offarch_stable, &foo, None),
            "-** must withdraw the ** grant"
        );
        assert!(
            any_then_negated.accepts(&kws("amd64"), &foo, None),
            "the trailing bare amd64 grant still applies"
        );

        assert!(AcceptToken::parse("-~*").is_some());
        assert!(AcceptToken::parse("-**").is_some());
    }

    #[test]
    fn accept_license_per_package_override() {
        let groups = LicenseGroupRegistry::default();
        let global = AcceptSet::from_tokens(&["MIT".into()], &groups);
        let foo = Cpv::parse("dev-libs/foo-1").unwrap();
        let bar = Cpv::parse("dev-libs/bar-1").unwrap();

        // package.license `dev-libs/foo GPL-2` accepts GPL-2 for foo only.
        let licenses = AcceptOverlay::new(
            global.clone(),
            vec![(
                dep("dev-libs/foo"),
                AcceptSet::from_tokens(&["GPL-2".into()], &groups),
            )],
        );
        assert!(licenses.effective_for(&foo, None).accepts("GPL-2"));
        assert!(!licenses.effective_for(&bar, None).accepts("GPL-2"));
        // The global MIT acceptance still applies everywhere.
        assert!(licenses.effective_for(&bar, None).accepts("MIT"));

        // No per-package entries ⇒ the global decision is borrowed unchanged.
        let plain = AcceptOverlay::new(global, Vec::new());
        assert!(!plain.effective_for(&foo, None).accepts("GPL-2"));
        assert!(plain.effective_for(&foo, None).accepts("MIT"));
    }

    /// `AcceptProperties`/`AcceptRestrict` are the same engine as
    /// `AcceptOverlay` (type aliases) — group-free construction, same
    /// per-package overlay fold, `RestrictExpr`'s USE-conditional evaluation.
    #[test]
    fn accept_properties_and_restrict_reuse_the_license_engine() {
        // Global accepts only `mirror`; per-package `dev-libs/foo interactive`
        // adds `interactive` for foo only (no interaction with a `-deny`, which
        // `AcceptSet::merge`'s union-only `denied` set cannot be
        // re-allowed by a per-package overlay short of `-*` — the same rule
        // `accept_license_per_package_override` relies on).
        let global = AcceptSet::from_tokens_plain(&["mirror".into()]);
        let foo = Cpv::parse("dev-libs/foo-1").unwrap();
        let bar = Cpv::parse("dev-libs/bar-1").unwrap();

        let accept_properties = AcceptProperties::new(
            global.clone(),
            vec![(
                dep("dev-libs/foo"),
                AcceptSet::from_tokens_plain(&["interactive".into()]),
            )],
        );
        let interactive = portage_metadata::RestrictExpr::parse("interactive").unwrap();
        assert!(
            accept_properties
                .effective_for(&foo, None)
                .accepts_restrict(&interactive, &|_| false)
        );
        assert!(
            !accept_properties
                .effective_for(&bar, None)
                .accepts_restrict(&interactive, &|_| false)
        );

        // A USE-conditional entry only contributes when its flag is active.
        let accept_restrict = AcceptRestrict::new(
            AcceptSet::from_tokens_plain(&["*".into(), "-bindist".into()]),
            Vec::new(),
        );
        let conditional = portage_metadata::RestrictExpr::parse("bindist? ( bindist )").unwrap();
        let accept = accept_restrict.effective_for(&foo, None);
        assert!(
            accept.accepts_restrict(&conditional, &|f| f != "bindist"),
            "inactive flag: the bindist restriction never applies"
        );
        assert!(
            !accept.accepts_restrict(&conditional, &|f| f == "bindist"),
            "active flag: bindist is required and not accepted"
        );
        assert_eq!(
            accept.restrict_needed(&conditional, &|f| f == "bindist"),
            vec!["bindist".to_string()]
        );
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
                real_cpn_of: Default::default(),
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
            real_cpn_of: Default::default(),
        }
    }

    /// Build a minimal on-disk repo with one ebuild's md5-cache entry, so
    /// `load_repos` can be exercised against a real `Repository`.
    fn disk_repo(cpv: &str, cache_text: &str) -> (tempfile::TempDir, Repository) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("metadata")).unwrap();
        std::fs::write(dir.path().join("metadata").join("layout.conf"), "").unwrap();
        std::fs::create_dir_all(dir.path().join("profiles")).unwrap();

        let cpv = Cpv::parse(cpv).unwrap();
        let cache_dir = dir
            .path()
            .join("metadata")
            .join("md5-cache")
            .join(cpv.cpn.category.as_ref());
        std::fs::create_dir_all(&cache_dir).unwrap();
        std::fs::write(
            cache_dir.join(format!("{}-{}", cpv.cpn.package, cpv.version)),
            cache_text,
        )
        .unwrap();

        let repo = Repository::builder()
            .in_memory_cache()
            .open(dir.path())
            .unwrap();
        (dir, repo)
    }

    /// Like [`disk_repo`], but also writes a real `.ebuild` file with a
    /// matching `_md5_` in the cache entry. `disk_repo`'s bare cache entry
    /// is trusted as-is only because it has no on-disk ebuild to contradict
    /// it (`repo_entries`'s suspect walk has nothing to flag); a test whose
    /// repo *does* carry a real ebuild tree needs the digest to actually
    /// match, or the entry is dropped and re-sourced instead.
    fn disk_repo_with_ebuild(cpv: &str, description: &str) -> (tempfile::TempDir, Repository) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("metadata")).unwrap();
        std::fs::write(dir.path().join("metadata").join("layout.conf"), "").unwrap();
        std::fs::create_dir_all(dir.path().join("profiles")).unwrap();

        let cpv = Cpv::parse(cpv).unwrap();
        // `ebuilds` (unlike the cache-only bulk read) scans only the
        // categories listed here -- an empty/missing file means it finds
        // nothing at all, silently.
        std::fs::write(
            dir.path().join("profiles").join("categories"),
            cpv.cpn.category.as_ref(),
        )
        .unwrap();
        let pf = format!("{}-{}", cpv.cpn.package, cpv.version);
        let pkg_dir = dir
            .path()
            .join(cpv.cpn.category.as_ref())
            .join(cpv.cpn.package.as_ref());
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let ebuild_text =
            format!("EAPI=8\nDESCRIPTION=\"{description}\"\nSLOT=\"0\"\nKEYWORDS=\"~amd64\"\n");
        std::fs::write(pkg_dir.join(format!("{pf}.ebuild")), &ebuild_text).unwrap();
        let digest = format!("{:x}", md5::compute(ebuild_text.as_bytes()));

        let cache_dir = dir
            .path()
            .join("metadata")
            .join("md5-cache")
            .join(cpv.cpn.category.as_ref());
        std::fs::create_dir_all(&cache_dir).unwrap();
        std::fs::write(
            cache_dir.join(&pf),
            format!("EAPI=8\nDESCRIPTION={description}\nSLOT=0\n_md5_={digest}\n"),
        )
        .unwrap();

        let repo = Repository::builder()
            .in_memory_cache()
            .open(dir.path())
            .unwrap();
        (dir, repo)
    }

    /// By default (main listed after the overlay in `sources`, matching
    /// main's real `-1000` default priority losing to an overlay's unset-
    /// priority `0`) an overlay's cpv wins over an identical cpv from main
    /// -- live-verified against real portage 3.0.81.2 (`portdbapi.xmatch`,
    /// same setup: main priority -1000, overlay priority 0, `bestmatch-
    /// visible` picks the overlay's ebuild). This is the ordinary "drop an
    /// ebuild in a local overlay to override ::gentoo" workflow, no
    /// repos.conf configuration required.
    #[tokio::test]
    async fn load_repos_overlay_wins_over_main_for_duplicate_cpv_by_default() {
        let (_main_dir, main) =
            disk_repo("dev-libs/foo-1", "EAPI=8\nDESCRIPTION=from main\nSLOT=0\n");
        let (_overlay_dir, overlay) = disk_repo_with_ebuild("dev-libs/foo-1", "from overlay");
        let overlay_name = overlay.name().to_string();

        let set = portage_repo::RepoSet::from_ordered(
            vec![std::sync::Arc::new(overlay), std::sync::Arc::new(main)],
            1,
            Vec::new(),
        );
        let data = load_repos(&set).await;

        let cpv = Cpv::parse("dev-libs/foo-1").unwrap();
        let entries = data.versions.get(&cpv.cpn).unwrap();
        assert_eq!(
            entries.len(),
            1,
            "the duplicate must be deduped, not both kept"
        );
        assert_eq!(entries[0].1.metadata.description, "from overlay");
        assert_eq!(
            data.repo_of.get(&cpv).map(String::as_str),
            Some(overlay_name.as_str()),
            "repo_of must attribute the win to the overlay, not be absent (which means main)"
        );
    }

    /// The inverse: when main is listed *before* the overlay in `sources`
    /// (an overlay with an explicit priority lower than main's -1000 --
    /// exotic, but real portage supports it, `man 5 portage`'s own
    /// `priority = 9999`-style examples for the opposite direction), main
    /// wins instead, and `repo_of` correctly has no entry for that cpv.
    #[tokio::test]
    async fn load_repos_main_wins_when_ranked_above_the_overlay() {
        let (_main_dir, main) =
            disk_repo("dev-libs/foo-1", "EAPI=8\nDESCRIPTION=from main\nSLOT=0\n");
        let (_overlay_dir, overlay) = disk_repo_with_ebuild("dev-libs/foo-1", "from overlay");

        let set = portage_repo::RepoSet::from_ordered(
            vec![std::sync::Arc::new(main), std::sync::Arc::new(overlay)],
            0,
            Vec::new(),
        );
        let data = load_repos(&set).await;

        let cpv = Cpv::parse("dev-libs/foo-1").unwrap();
        let entries = data.versions.get(&cpv.cpn).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1.metadata.description, "from main");
        assert_eq!(data.repo_of.get(&cpv), None, "absence means main");
    }

    /// `load_repos` injects `Location::Alias` entries as in-memory `cross-<tuple>/<pkg>`
    /// packages cloned from the source repo, with `real_cpn_of` recording the
    /// derivation — the in-memory equivalent of crossdev's symlink overlay.
    #[tokio::test]
    async fn load_repos_injects_alias_cross_packages() {
        let (_dir, repo) = disk_repo("sys-devel/gcc-15.2.1", "EAPI=8\nDESCRIPTION=t\nSLOT=0\n");

        let real_cpn = Cpn::parse("sys-devel/gcc").unwrap();
        let cross_cat = "cross-riscv64-unknown-linux-gnu";
        let mut aliases = HashMap::new();
        aliases.insert(cross_cat.to_string(), [real_cpn].into_iter().collect());
        let alias_entry = portage_repo::RepoEntry {
            name: "crossdev".into(),
            location: portage_repo::Location::Alias {
                source: repo.name().to_string(),
                aliases,
            },
            masters: None,
            sync_type: None,
            sync_uri: None,
            auto_sync: false,
            volatile: None,
            priority: None,
        };

        let mut set = portage_repo::RepoSet::single(repo);
        set.set_aliases(vec![alias_entry]);
        let data = load_repos(&set).await;

        let cross_cpn = Cpn::new(cross_cat, "gcc");
        assert!(data.cpns.contains(&cross_cpn), "cross cpn injected");
        assert!(data.cpns.contains(&real_cpn), "real cpn still present");
        let cross_versions = data
            .versions
            .get(&cross_cpn)
            .expect("cross versions present");
        assert_eq!(cross_versions.len(), 1);
        assert_eq!(cross_versions[0].0.version.to_string(), "15.2.1");
        assert_eq!(data.real_cpn_of.get(&cross_cpn), Some(&real_cpn));
    }

    /// An alias entry whose declared `source` doesn't match the repo being
    /// loaded is skipped rather than silently pulling from the wrong repo.
    #[tokio::test]
    async fn load_repos_alias_from_unknown_source_is_ignored() {
        let (_dir, repo) = disk_repo("sys-devel/gcc-15.2.1", "EAPI=8\nDESCRIPTION=t\nSLOT=0\n");

        let real_cpn = Cpn::parse("sys-devel/gcc").unwrap();
        let cross_cat = "cross-riscv64-unknown-linux-gnu";
        let mut aliases = HashMap::new();
        aliases.insert(cross_cat.to_string(), [real_cpn].into_iter().collect());
        let alias_entry = portage_repo::RepoEntry {
            name: "crossdev".into(),
            location: portage_repo::Location::Alias {
                source: "some-other-repo".into(),
                aliases,
            },
            masters: None,
            sync_type: None,
            sync_uri: None,
            auto_sync: false,
            volatile: None,
            priority: None,
        };

        let mut set = portage_repo::RepoSet::single(repo);
        set.set_aliases(vec![alias_entry]);
        let data = load_repos(&set).await;

        let cross_cpn = Cpn::new(cross_cat, "gcc");
        assert!(!data.cpns.contains(&cross_cpn));
        assert!(data.real_cpn_of.is_empty());
    }

    // `target_package` resolves the slot identity from the atom's slot operator
    // and version constraint (emerge semantics): a bare name / `:*` picks the
    // newest slot, but `:slot` and `=…-ver*` pin the matching slot rather than
    // collapsing to the newest. Regression for the python:3.13 / =python-3.13*
    // gap.

    fn empty_layer() -> &'static portage_atom_pubgrub::UseLayer {
        use std::sync::OnceLock;
        static E: OnceLock<portage_atom_pubgrub::UseLayer> = OnceLock::new();
        E.get_or_init(portage_atom_pubgrub::UseLayer::default)
    }

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
                &ResolvePolicy {
                    accept_keywords: &ak,
                    package_mask: &[],
                    package_unmask: &[],
                    accept_licenses: &AcceptOverlay::new(accept_all_licenses(), Vec::new()),
                    accept_properties: &AcceptProperties::new(accept_all_licenses(), Vec::new()),
                    accept_restrict: &AcceptRestrict::new(accept_all_licenses(), Vec::new()),
                    pre_env: empty_layer(),
                    env_use: empty_layer(),
                    package_use: &[],
                    profile_package_use: &[],
                    force_mask: &ForceMask::default(),
                    use_expand: UseExpand::empty(),
                },
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

    #[test]
    fn filter_reasons_report_every_excluded_candidate() {
        let data = repo_with_many(&[
            (
                "app-misc/thing-1.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\n",
            ),
            (
                "app-misc/thing-2.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=~arm64\nDESCRIPTION=t\n",
            ),
            (
                "app-misc/thing-3.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=arm64\nDESCRIPTION=t\n",
            ),
        ]);
        let arch = Arch::intern("arm64");
        let ak = AcceptKeywords::from_global(&arch, &["arm64"]);
        let cpn = Cpn::try_new("app-misc/thing").expect("cpn parses");
        let mask = vec![dep("=app-misc/thing-3.0")];
        let policy = ResolvePolicy {
            accept_keywords: &ak,
            package_mask: &mask,
            package_unmask: &[],
            accept_licenses: &AcceptOverlay::new(accept_all_licenses(), Vec::new()),
            accept_properties: &AcceptProperties::new(accept_all_licenses(), Vec::new()),
            accept_restrict: &AcceptRestrict::new(accept_all_licenses(), Vec::new()),
            pre_env: empty_layer(),
            env_use: empty_layer(),
            package_use: &[],
            profile_package_use: &[],
            force_mask: &ForceMask::default(),
            use_expand: UseExpand::empty(),
        };
        let found = filter_reasons_for(
            &data,
            &cpn,
            &portage_atom_pubgrub::PortageVersionSet::any(),
            &policy,
        );
        let reason_for = |v: &str| {
            found
                .iter()
                .find(|c| c.cpv.version.to_string() == v)
                .map(|c| c.reasons.clone())
        };
        // No arm64 keyword at all ⇒ `**`; testing-only ⇒ `~arm64`.
        assert!(
            matches!(reason_for("1.0").as_deref(), Some([FilterReason::Keyword(k)]) if k == "**")
        );
        assert!(
            matches!(reason_for("2.0").as_deref(), Some([FilterReason::Keyword(k)]) if k == "~arm64")
        );
        assert!(matches!(
            reason_for("3.0").as_deref(),
            Some([FilterReason::Masked])
        ));
    }

    #[test]
    fn filter_reasons_are_empty_when_a_candidate_is_accepted() {
        let data = repo_with_many(&[(
            "app-misc/thing-1.0",
            "EAPI=8\nSLOT=0\nKEYWORDS=arm64\nDESCRIPTION=t\n",
        )]);
        let arch = Arch::intern("arm64");
        let ak = AcceptKeywords::from_global(&arch, &["arm64"]);
        let policy = ResolvePolicy {
            accept_keywords: &ak,
            package_mask: &[],
            package_unmask: &[],
            accept_licenses: &AcceptOverlay::new(accept_all_licenses(), Vec::new()),
            accept_properties: &AcceptProperties::new(accept_all_licenses(), Vec::new()),
            accept_restrict: &AcceptRestrict::new(accept_all_licenses(), Vec::new()),
            pre_env: empty_layer(),
            env_use: empty_layer(),
            package_use: &[],
            profile_package_use: &[],
            force_mask: &ForceMask::default(),
            use_expand: UseExpand::empty(),
        };
        let vs = portage_atom_pubgrub::PortageVersionSet::any();
        let cpn = Cpn::try_new("app-misc/thing").expect("cpn parses");
        assert!(filter_reasons_for(&data, &cpn, &vs, &policy).is_empty());
        // An unknown CPN has nothing to report either.
        let absent = Cpn::try_new("app-misc/absent").expect("cpn parses");
        assert!(filter_reasons_for(&data, &absent, &vs, &policy).is_empty());
    }

    /// End-to-end: an ebuild's `RESTRICT` token not in `ACCEPT_RESTRICT` filters
    /// it out with `FilterReason::Restrict`, and `version_accepted` agrees.
    #[test]
    fn restrict_not_accepted_filters_the_version() {
        let (data, cpv) = repo_with(
            "net-misc/thing-1.0",
            "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nRESTRICT=bindist\nDESCRIPTION=t\n",
        );
        let arch = Arch::intern("amd64");
        let ak = AcceptKeywords::from_global(&arch, &["amd64"]);
        let deny_bindist = AcceptRestrict::new(
            AcceptSet::from_tokens_plain(&["*".into(), "-bindist".into()]),
            Vec::new(),
        );
        let policy = ResolvePolicy {
            accept_keywords: &ak,
            package_mask: &[],
            package_unmask: &[],
            accept_licenses: &AcceptOverlay::new(accept_all_licenses(), Vec::new()),
            accept_properties: &AcceptProperties::new(accept_all_licenses(), Vec::new()),
            accept_restrict: &deny_bindist,
            pre_env: empty_layer(),
            env_use: empty_layer(),
            package_use: &[],
            profile_package_use: &[],
            force_mask: &ForceMask::default(),
            use_expand: UseExpand::empty(),
        };
        let cpn = Cpn::try_new("net-misc/thing").expect("cpn parses");
        let vs = portage_atom_pubgrub::PortageVersionSet::any();
        let found = filter_reasons_for(&data, &cpn, &vs, &policy);
        assert!(matches!(
            found.as_slice(),
            [c] if matches!(c.reasons.as_slice(), [FilterReason::Restrict(r)] if r == &["bindist".to_string()])
        ));

        let adapter = Adapter {
            data: &data,
            accept_keywords: &ak,
            package_mask: &[],
            package_unmask: &[],
            installed_cpvs: &std::collections::HashSet::new(),
            rebuilding_cpvs: &std::collections::HashSet::new(),
            accept_licenses: &AcceptOverlay::new(accept_all_licenses(), Vec::new()),
            accept_properties: &AcceptProperties::new(accept_all_licenses(), Vec::new()),
            accept_restrict: &deny_bindist,
            pre_env: empty_layer(),
            env_use: empty_layer(),
            package_use: &[],
            profile_package_use: &[],
            force_mask: &ForceMask::default(),
            use_expand: UseExpand::empty(),
            autosolve_use: true,
        };
        let entry = &data.versions[&cpn][0].1;
        assert!(!adapter.version_accepted(&cpv, entry));

        // Accepting bindist makes the same version pass.
        let accept_all = AcceptRestrict::new(accept_all_licenses(), Vec::new());
        let adapter_allow = Adapter {
            accept_restrict: &accept_all,
            ..adapter
        };
        assert!(adapter_allow.version_accepted(&cpv, entry));
    }

    /// `target_package`'s unslotted return is the caller's signal that nothing
    /// acceptable satisfies the atom — for a filtered-out candidate and for a
    /// CPN absent from the tree alike.
    #[test]
    fn target_package_is_unslotted_when_nothing_is_accepted() {
        let data = repo_with_many(&[(
            "app-misc/thing-1.0",
            "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\n",
        )]);
        let arch = Arch::intern("arm64");
        let ak = AcceptKeywords::from_global(&arch, &["arm64"]);
        let policy = ResolvePolicy {
            accept_keywords: &ak,
            package_mask: &[],
            package_unmask: &[],
            accept_licenses: &AcceptOverlay::new(accept_all_licenses(), Vec::new()),
            accept_properties: &AcceptProperties::new(accept_all_licenses(), Vec::new()),
            accept_restrict: &AcceptRestrict::new(accept_all_licenses(), Vec::new()),
            pre_env: empty_layer(),
            env_use: empty_layer(),
            package_use: &[],
            profile_package_use: &[],
            force_mask: &ForceMask::default(),
            use_expand: UseExpand::empty(),
        };
        assert!(
            target_package(&data, &dep("app-misc/thing"), &policy)
                .slot()
                .is_none(),
            "keyword-filtered candidate"
        );
        assert!(
            target_package(&data, &dep("app-misc/absent"), &policy)
                .slot()
                .is_none(),
            "CPN absent from the tree"
        );
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
        let pre_env = portage_atom_pubgrub::UseLayer::parse("a b"); // both on ⇒ ?? ( a b ) violated

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
            rebuilding_cpvs: &std::collections::HashSet::new(),
            accept_licenses: &AcceptOverlay::new(accept_all_licenses(), Vec::new()),
            accept_properties: &AcceptProperties::new(accept_all_licenses(), Vec::new()),
            accept_restrict: &AcceptRestrict::new(accept_all_licenses(), Vec::new()),
            pre_env: &pre_env,
            env_use: empty_layer(),
            package_use: &[],
            profile_package_use: &[],
            force_mask: &fm, // a is use.force'd
            use_expand: UseExpand::empty(),
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
        let pre_env = portage_atom_pubgrub::UseLayer::parse("a b");

        let fm = ForceMask::default();
        let ak = AcceptKeywords::from_global(&arch, &["amd64"]);
        let adapter = Adapter {
            data: &data,
            accept_keywords: &ak,
            package_mask: &[],
            package_unmask: &[],
            installed_cpvs: &std::collections::HashSet::new(),
            rebuilding_cpvs: &std::collections::HashSet::new(),
            accept_licenses: &AcceptOverlay::new(accept_all_licenses(), Vec::new()),
            accept_properties: &AcceptProperties::new(accept_all_licenses(), Vec::new()),
            accept_restrict: &AcceptRestrict::new(accept_all_licenses(), Vec::new()),
            pre_env: &pre_env,
            env_use: empty_layer(),
            package_use: &[],
            profile_package_use: &[],
            force_mask: &fm,
            use_expand: UseExpand::empty(),
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

    /// Stage1 `USE=-*` wipes IUSE `+` defaults so `^^ ( gawk busybox … )` has
    /// every option off. Cede must prefer the ebuild's `+` default (`gawk`) so
    /// the solver restores the app-alternatives.eclass first provider, not an
    /// arbitrary member of the exclusive group.
    #[test]
    fn cede_prefers_iuse_plus_default_when_config_has_flag_off() {
        let (data, cpv) = repo_with(
            "app-alternatives/awk-4",
            "EAPI=8\nSLOT=0\nIUSE=+gawk busybox mawk nawk\n\
             REQUIRED_USE=^^ ( gawk busybox mawk nawk )\n\
             KEYWORDS=amd64\nDESCRIPTION=t\n",
        );
        let arch = Arch::intern("amd64");
        // Conf-layer -* equivalent: no flags on (all providers off).
        let pre_env = portage_atom_pubgrub::UseLayer::parse("-* build");

        let fm = ForceMask::default();
        let ak = AcceptKeywords::from_global(&arch, &["amd64"]);
        let adapter = Adapter {
            data: &data,
            accept_keywords: &ak,
            package_mask: &[],
            package_unmask: &[],
            installed_cpvs: &std::collections::HashSet::new(),
            rebuilding_cpvs: &std::collections::HashSet::new(),
            accept_licenses: &AcceptOverlay::new(accept_all_licenses(), Vec::new()),
            accept_properties: &AcceptProperties::new(accept_all_licenses(), Vec::new()),
            accept_restrict: &AcceptRestrict::new(accept_all_licenses(), Vec::new()),
            pre_env: &pre_env,
            env_use: empty_layer(),
            package_use: &[],
            profile_package_use: &[],
            force_mask: &fm,
            use_expand: UseExpand::empty(),
            autosolve_use: true,
        };

        let desired = adapter.desired_use(&cpv);
        match desired.get(Interned::intern("gawk")) {
            UseFlagState::SolverDecided { prefer: true } => {}
            other => panic!("gawk (+ IUSE default) must prefer on when wiped by -*; got {other:?}"),
        }
        for f in ["busybox", "mawk", "nawk"] {
            match desired.get(Interned::intern(f)) {
                UseFlagState::SolverDecided { prefer: false } => {}
                other => {
                    panic!("{f} (no +default) must prefer off when wiped by -*; got {other:?}")
                }
            }
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
        let pre_env = portage_atom_pubgrub::UseLayer::parse("a"); // only a on ⇒ ?? satisfied

        let fm = ForceMask::default();
        let ak = AcceptKeywords::from_global(&arch, &["amd64"]);
        let adapter = Adapter {
            data: &data,
            accept_keywords: &ak,
            package_mask: &[],
            package_unmask: &[],
            installed_cpvs: &std::collections::HashSet::new(),
            rebuilding_cpvs: &std::collections::HashSet::new(),
            accept_licenses: &AcceptOverlay::new(accept_all_licenses(), Vec::new()),
            accept_properties: &AcceptProperties::new(accept_all_licenses(), Vec::new()),
            accept_restrict: &AcceptRestrict::new(accept_all_licenses(), Vec::new()),
            pre_env: &pre_env,
            env_use: empty_layer(),
            package_use: &[],
            profile_package_use: &[],
            force_mask: &fm,
            use_expand: UseExpand::empty(),
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

    // Fixes the `em stages --stage1 --prefix <dir>` finding:
    // `cede_required_use`'s old
    // `installed_cpvs.contains(cpv)` early return fired for *any* cpv already
    // in the "installed" view, even when that exact cpv is the one currently
    // being resolved with a REQUIRED_USE-violating config (e.g. stage1's
    // `USE="-* build"` suppressing app-alternatives.eclass's `+`-default
    // flag on `app-alternatives/awk-4`, itself a literal `packages.build`
    // root target) — so autosolve-use never got a chance to cede/re-decide
    // it, and the plan carried a genuine REQUIRED_USE violation into the
    // build, which then died at pkg_setup/src_install. Fixed (Fable-planned)
    // by adding `rebuilding_cpvs`: an installed cpv also in that set is
    // being rebuilt this run (non-selective explicit root target, or
    // -N/-U USE-drift), so its REQUIRED_USE is ceded like any other build
    // target — see `installed_and_untouched_package_required_use_drift_is_not_ceded`
    // below for why `installed_cpvs` alone still gates everything else.
    #[test]
    fn installed_but_rebuilding_cpv_with_violated_required_use_is_ceded() {
        let (data, cpv) = repo_with(
            "cat/pkg-1.0",
            "EAPI=7\nSLOT=0\nIUSE=a b\nREQUIRED_USE=?? ( a b )\nKEYWORDS=amd64\nDESCRIPTION=t\n",
        );
        let arch = Arch::intern("amd64");
        // Both flags on ⇒ ?? ( a b ) violated — identical config shape to
        // `unforced_flags_are_ceded`, the only difference is `installed_cpvs`.
        let pre_env = portage_atom_pubgrub::UseLayer::parse("a b");

        let fm = ForceMask::default();
        let ak = AcceptKeywords::from_global(&arch, &["amd64"]);
        let mut installed = std::collections::HashSet::new();
        installed.insert(cpv.clone());
        let mut rebuilding = std::collections::HashSet::new();
        rebuilding.insert(cpv.clone());
        let adapter = Adapter {
            data: &data,
            accept_keywords: &ak,
            package_mask: &[],
            package_unmask: &[],
            installed_cpvs: &installed,
            rebuilding_cpvs: &rebuilding,
            accept_licenses: &AcceptOverlay::new(accept_all_licenses(), Vec::new()),
            accept_properties: &AcceptProperties::new(accept_all_licenses(), Vec::new()),
            accept_restrict: &AcceptRestrict::new(accept_all_licenses(), Vec::new()),
            pre_env: &pre_env,
            env_use: empty_layer(),
            package_use: &[],
            profile_package_use: &[],
            force_mask: &fm,
            use_expand: UseExpand::empty(),
            autosolve_use: true,
        };

        let desired = adapter.desired_use(&cpv);
        for f in ["a", "b"] {
            assert!(
                matches!(
                    desired.get(Interned::intern(f)),
                    UseFlagState::SolverDecided { .. }
                ),
                "flag {f} should be ceded for an installed cpv that is also \
                 being rebuilt this run (the awk-4/stage1 fix)"
            );
        }
    }

    // Why `installed_cpvs` alone still gates everything else (commit
    // b919014, 2026-06-10): an installed package that is genuinely staying
    // installed (not in `rebuilding_cpvs`) must NOT have its `REQUIRED_USE`
    // flags ceded just because profile/config drift now violates them —
    // only packages being *built* get their constraints auto-satisfied,
    // matching real emerge (installed-package violations are Level-A
    // advisories only). Without this guard, config drift on an untouched
    // package invents a USE decision that can cascade into a spurious
    // rebuild of something else entirely (the original bug: installed
    // systemd-utils' `^^` drifted against a moved PYTHON_SINGLE_TARGET,
    // autosolve ceded it, and a C7 co-solve forced a spurious jinja2 rebuild
    // for a target nothing needs). `rebuilding_cpvs` is empty here — this
    // cpv is installed but not a rebuild target — so the guard must still
    // hold exactly as before the fix above.
    #[test]
    fn installed_and_untouched_package_required_use_drift_is_not_ceded() {
        let (data, cpv) = repo_with(
            "cat/pkg-1.0",
            "EAPI=7\nSLOT=0\nIUSE=a b\nREQUIRED_USE=?? ( a b )\nKEYWORDS=amd64\nDESCRIPTION=t\n",
        );
        let arch = Arch::intern("amd64");
        let pre_env = portage_atom_pubgrub::UseLayer::parse("a b"); // drifted: both on now

        let fm = ForceMask::default();
        let ak = AcceptKeywords::from_global(&arch, &["amd64"]);
        let mut installed = std::collections::HashSet::new();
        installed.insert(cpv.clone());
        let adapter = Adapter {
            data: &data,
            accept_keywords: &ak,
            package_mask: &[],
            package_unmask: &[],
            installed_cpvs: &installed,
            rebuilding_cpvs: &std::collections::HashSet::new(),
            accept_licenses: &AcceptOverlay::new(accept_all_licenses(), Vec::new()),
            accept_properties: &AcceptProperties::new(accept_all_licenses(), Vec::new()),
            accept_restrict: &AcceptRestrict::new(accept_all_licenses(), Vec::new()),
            pre_env: &pre_env,
            env_use: empty_layer(),
            package_use: &[],
            profile_package_use: &[],
            force_mask: &fm,
            use_expand: UseExpand::empty(),
            autosolve_use: true,
        };

        let desired = adapter.desired_use(&cpv);
        for f in ["a", "b"] {
            assert!(
                !matches!(
                    desired.get(Interned::intern(f)),
                    UseFlagState::SolverDecided { .. }
                ),
                "flag {f} must not be ceded for an installed, untouched package \
                 whose REQUIRED_USE merely drifted — see commit b919014"
            );
        }
    }

    // A rebuilding cpv whose REQUIRED_USE is already satisfied must not be
    // gratuitously ceded — the `unsatisfied.is_empty()` check (present
    // regardless of `rebuilding_cpvs`) still short-circuits correctly, same
    // guarantee `satisfied_constraint_cedes_nothing` proves for a
    // never-installed package.
    #[test]
    fn rebuilding_cpv_with_satisfied_required_use_cedes_nothing() {
        let (data, cpv) = repo_with(
            "cat/pkg-1.0",
            "EAPI=7\nSLOT=0\nIUSE=a b\nREQUIRED_USE=?? ( a b )\nKEYWORDS=amd64\nDESCRIPTION=t\n",
        );
        let arch = Arch::intern("amd64");
        let pre_env = portage_atom_pubgrub::UseLayer::parse("a"); // only a on ⇒ ?? satisfied

        let fm = ForceMask::default();
        let ak = AcceptKeywords::from_global(&arch, &["amd64"]);
        let mut installed = std::collections::HashSet::new();
        installed.insert(cpv.clone());
        let mut rebuilding = std::collections::HashSet::new();
        rebuilding.insert(cpv.clone());
        let adapter = Adapter {
            data: &data,
            accept_keywords: &ak,
            package_mask: &[],
            package_unmask: &[],
            installed_cpvs: &installed,
            rebuilding_cpvs: &rebuilding,
            accept_licenses: &AcceptOverlay::new(accept_all_licenses(), Vec::new()),
            accept_properties: &AcceptProperties::new(accept_all_licenses(), Vec::new()),
            accept_restrict: &AcceptRestrict::new(accept_all_licenses(), Vec::new()),
            pre_env: &pre_env,
            env_use: empty_layer(),
            package_use: &[],
            profile_package_use: &[],
            force_mask: &fm,
            use_expand: UseExpand::empty(),
            autosolve_use: true,
        };

        let desired = adapter.desired_use(&cpv);
        for f in ["a", "b"] {
            assert!(
                !matches!(
                    desired.get(Interned::intern(f)),
                    UseFlagState::SolverDecided { .. }
                ),
                "flag {f} must not be ceded when REQUIRED_USE already holds, \
                 rebuilding or not"
            );
        }
    }

    // `rebuilding_cpvs` containing a cpv that ISN'T installed must not change
    // anything: the not-installed path already ceded correctly before this
    // field existed (`unforced_flags_are_ceded`), and the new condition
    // (`installed_cpvs.contains(cpv) && !rebuilding_cpvs.contains(cpv)`) must
    // not accidentally degrade to `!rebuilding_cpvs.contains(cpv)` alone.
    #[test]
    fn rebuilding_cpvs_membership_alone_does_not_gate_a_never_installed_package() {
        let (data, cpv) = repo_with(
            "cat/pkg-1.0",
            "EAPI=7\nSLOT=0\nIUSE=a b\nREQUIRED_USE=?? ( a b )\nKEYWORDS=amd64\nDESCRIPTION=t\n",
        );
        let arch = Arch::intern("amd64");
        let pre_env = portage_atom_pubgrub::UseLayer::parse("a b");

        let fm = ForceMask::default();
        let ak = AcceptKeywords::from_global(&arch, &["amd64"]);
        // cpv is in rebuilding_cpvs but NOT in installed_cpvs — should not
        // matter; the not-installed path always ceded.
        let mut rebuilding = std::collections::HashSet::new();
        rebuilding.insert(cpv.clone());
        let adapter = Adapter {
            data: &data,
            accept_keywords: &ak,
            package_mask: &[],
            package_unmask: &[],
            installed_cpvs: &std::collections::HashSet::new(),
            rebuilding_cpvs: &rebuilding,
            accept_licenses: &AcceptOverlay::new(accept_all_licenses(), Vec::new()),
            accept_properties: &AcceptProperties::new(accept_all_licenses(), Vec::new()),
            accept_restrict: &AcceptRestrict::new(accept_all_licenses(), Vec::new()),
            pre_env: &pre_env,
            env_use: empty_layer(),
            package_use: &[],
            profile_package_use: &[],
            force_mask: &fm,
            use_expand: UseExpand::empty(),
            autosolve_use: true,
        };

        let desired = adapter.desired_use(&cpv);
        for f in ["a", "b"] {
            assert!(
                matches!(
                    desired.get(Interned::intern(f)),
                    UseFlagState::SolverDecided { .. }
                ),
                "flag {f} should be ceded for a never-installed package regardless of rebuilding_cpvs"
            );
        }
    }

    // Regression for the em stages --stage1 --target riscv64 finding:
    // util-linux's
    // REQUIRED_USE="python? ( foo ) su? ( pam )" has two independent
    // top-level clauses. Only `su? ( pam )` is violated (su on, pam off);
    // `python? ( foo )` is independently satisfied (python off, vacuous).
    // Ceding must be scoped to the violated clause only — `python`/`foo`
    // must stay fixed-Disabled, not become a solver-owned virtual choice
    // (which previously fabricated a spurious dependency edge and a fake
    // install-order cycle).
    #[test]
    fn independently_satisfied_clause_is_not_ceded_by_an_unrelated_violation() {
        let (data, cpv) = repo_with(
            "cat/pkg-1.0",
            "EAPI=7\nSLOT=0\nIUSE=python foo su pam\n\
             REQUIRED_USE=python? ( foo ) su? ( pam )\nKEYWORDS=amd64\nDESCRIPTION=t\n",
        );
        let arch = Arch::intern("amd64");
        let pre_env = portage_atom_pubgrub::UseLayer::parse("su"); // su on, pam off ⇒ su?(pam) violated
        // python left unset ⇒ Disabled ⇒ python?(foo) vacuously satisfied

        let fm = ForceMask::default();
        let ak = AcceptKeywords::from_global(&arch, &["amd64"]);
        let adapter = Adapter {
            data: &data,
            accept_keywords: &ak,
            package_mask: &[],
            package_unmask: &[],
            installed_cpvs: &std::collections::HashSet::new(),
            rebuilding_cpvs: &std::collections::HashSet::new(),
            accept_licenses: &AcceptOverlay::new(accept_all_licenses(), Vec::new()),
            accept_properties: &AcceptProperties::new(accept_all_licenses(), Vec::new()),
            accept_restrict: &AcceptRestrict::new(accept_all_licenses(), Vec::new()),
            pre_env: &pre_env,
            env_use: empty_layer(),
            package_use: &[],
            profile_package_use: &[],
            force_mask: &fm,
            use_expand: UseExpand::empty(),
            autosolve_use: true,
        };

        let desired = adapter.desired_use(&cpv);
        for f in ["su", "pam"] {
            assert!(
                matches!(
                    desired.get(Interned::intern(f)),
                    UseFlagState::SolverDecided { .. }
                ),
                "flag {f} (in the violated clause) should be ceded"
            );
        }
        for f in ["python", "foo"] {
            assert!(
                !matches!(
                    desired.get(Interned::intern(f)),
                    UseFlagState::SolverDecided { .. }
                ),
                "flag {f} (in the independently-satisfied clause) must stay fixed, \
                 not be ceded just because an unrelated clause failed"
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
        let pre_env = portage_atom_pubgrub::UseLayer::parse("cet"); // user enabled a flag the profile masks

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
            rebuilding_cpvs: &std::collections::HashSet::new(),
            accept_licenses: &AcceptOverlay::new(accept_all_licenses(), Vec::new()),
            accept_properties: &AcceptProperties::new(accept_all_licenses(), Vec::new()),
            accept_restrict: &AcceptRestrict::new(accept_all_licenses(), Vec::new()),
            pre_env: &pre_env,
            env_use: empty_layer(),
            package_use: &[],
            profile_package_use: &[],
            force_mask: &fm,
            use_expand: UseExpand::empty(),
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
