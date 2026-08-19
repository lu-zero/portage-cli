//! USE flag policy vocabulary.
//!
//! These types describe the per-package USE *policy* a consumer resolves
//! (profile, `make.conf`, `package.use`, IUSE defaults) and hands to a solver.
//! They are solver-agnostic: every [`crate::Solver`] implementation consumes
//! the same [`UseConfig`]. The solver never resolves policy itself — see the
//! architecture doc's "USE/solver boundary" section.
//!
//! This is the canonical definition; `portage-atom-pubgrub` exposes an
//! identical type today and will re-export this one in a follow-up so the two
//! cannot drift.

use std::collections::HashMap;
use std::sync::Arc;

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpv, Dep, Operator, Revision, UseFlagLookup};

use crate::IUseDefault;

/// How a single USE flag should be evaluated during dependency conversion.
///
/// See [PMS 8.2](https://projects.gentoo.org/pms/9/pms.html#use-flag-dependent-dependencies).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UseFlagState {
    /// The flag is ON — `flag? ( deps )` includes deps, `!flag? ( deps )` skips.
    Enabled,
    /// The flag is OFF — `flag? ( deps )` skips deps, `!flag? ( deps )` includes.
    Disabled,
    /// The caller cedes this flag to the solver — a virtual decision node is
    /// created and the solver picks its value subject to constraints (Level-C
    /// `REQUIRED_USE`). `prefer` is the value the caller's policy would have
    /// produced; the solver biases toward it so a ceded flag only flips when a
    /// constraint forces it (greedy keep-configured).
    SolverDecided {
        /// Value the caller's policy would have produced; the solver biases
        /// toward it and only flips the flag when a constraint forces it.
        prefer: bool,
    },
}

/// Configuration for USE flag evaluation during dependency conversion.
///
/// Unset flags default to [`UseFlagState::Disabled`].
///
/// See [PMS 8.2](https://projects.gentoo.org/pms/9/pms.html#use-flag-dependent-dependencies).
///
/// A fully-resolved `UseConfig` (the output of [`resolve_effective_use`]) has
/// every flag already decided — no separate "fall back to the IUSE default"
/// step, because [`resolve_effective_use`] already folded the ebuild's own
/// `+`/`-` IUSE defaults in at their correct position in portage's real
/// USE-resolution order. See that function's doc for why a config built any
/// other way must not be treated as authoritative for a real package.
///
/// Internally this is a **shared profile base** ([`UseLayer`] fold, `Arc`) plus
/// a **small per-package overlay** (IUSE-only flags, `package.use`, env,
/// force/mask, ceded). Lookups check the overlay first, then the base — so a
/// hundred profile USE_EXPAND flags are not re-inserted into a fresh map on
/// every CPV.
#[derive(Debug, Clone, Default)]
pub struct UseConfig {
    /// Profile/`make.conf` fold shared across packages (`true` = enabled).
    base: Option<Arc<HashMap<Interned<DefaultInterner>, bool>>>,
    /// Package-local and higher-priority decisions (win over [`Self::base`]).
    overlay: HashMap<Interned<DefaultInterner>, UseFlagState>,
}

impl UseConfig {
    /// Create an empty config (all flags default to `Disabled`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Config whose only content is a shared frozen layer (e.g. env after `-*`).
    fn from_base_map(map: Arc<HashMap<Interned<DefaultInterner>, bool>>) -> Self {
        if map.is_empty() {
            Self::default()
        } else {
            Self {
                base: Some(map),
                overlay: HashMap::new(),
            }
        }
    }

    /// Set a flag's state.
    pub fn set(&mut self, flag: Interned<DefaultInterner>, state: UseFlagState) {
        self.overlay.insert(flag, state);
    }

    /// Enable a flag.
    pub fn enable(&mut self, flag: Interned<DefaultInterner>) {
        self.overlay.insert(flag, UseFlagState::Enabled);
    }

    /// Disable a flag.
    pub fn disable(&mut self, flag: Interned<DefaultInterner>) {
        self.overlay.insert(flag, UseFlagState::Disabled);
    }

    /// Mark a flag as solver-decided, with the caller's preferred value.
    pub fn solver_decide(&mut self, flag: Interned<DefaultInterner>, prefer: bool) {
        self.overlay
            .insert(flag, UseFlagState::SolverDecided { prefer });
    }

    /// Get the state of a flag.
    ///
    /// Unset flags default to `Disabled`.
    pub fn get(&self, flag: Interned<DefaultInterner>) -> UseFlagState {
        self.get_opt(flag).unwrap_or(UseFlagState::Disabled)
    }

    /// Return `Some(state)` if the flag is explicitly set, `None` if absent.
    pub fn get_opt(&self, flag: Interned<DefaultInterner>) -> Option<UseFlagState> {
        // Prefer overlay when present; skip the HashMap probe when empty.
        if !self.overlay.is_empty()
            && let Some(s) = self.overlay.get(&flag)
        {
            return Some(*s);
        }
        match &self.base {
            Some(b) => b.get(&flag).map(|en| {
                if *en {
                    UseFlagState::Enabled
                } else {
                    UseFlagState::Disabled
                }
            }),
            None => None,
        }
    }

    /// Returns all flags explicitly enabled in this config (sorted, for stable output).
    pub fn enabled_flags(&self) -> Vec<Interned<DefaultInterner>> {
        let mut v: Vec<Interned<DefaultInterner>> = Vec::new();
        if let Some(base) = &self.base {
            for (&f, &en) in base.iter() {
                if en && !self.overlay.contains_key(&f) {
                    v.push(f);
                }
            }
        }
        for (&f, s) in &self.overlay {
            if matches!(s, UseFlagState::Enabled) {
                v.push(f);
            }
        }
        v.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        v
    }

    /// Returns all flags marked `SolverDecided` (the ones ceded to the
    /// solver for Level-C `REQUIRED_USE` handling).
    ///
    /// Order is not guaranteed.
    pub fn solver_decided_flags(&self) -> Vec<Interned<DefaultInterner>> {
        self.overlay
            .iter()
            .filter(|(_, s)| matches!(s, UseFlagState::SolverDecided { .. }))
            .map(|(f, _)| *f)
            .collect()
    }
}

impl UseFlagLookup for UseConfig {
    fn use_flag_active(&self, flag: Interned<DefaultInterner>) -> bool {
        matches!(self.get(flag), UseFlagState::Enabled)
    }
}

/// A parsed `package.use` override: a USE flag and whether it is turned on.
///
/// Parsing (`+flag`/`flag` → on, `-flag` → off) and interning happen once at
/// config-read time (via [`UseOverride::parse`]) so the per-version
/// [`resolve_effective_use`] call does no string work. Cheap to copy (an
/// interned `u32` plus a bool).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UseOverride {
    /// The interned flag name, with any `+`/`-` prefix stripped.
    pub flag: Interned<DefaultInterner>,
    /// `true` enables the flag, `false` disables it.
    pub enable: bool,
}

impl UseOverride {
    /// Parse a single `package.use` token: `flag`/`+flag` enables, `-flag`
    /// disables.
    pub fn parse(token: &str) -> Self {
        let name = token.strip_prefix('+').unwrap_or(token);
        match name.strip_prefix('-') {
            Some(rest) => Self {
                flag: Interned::intern(rest),
                enable: false,
            },
            None => Self {
                flag: Interned::intern(name),
                enable: true,
            },
        }
    }
}

/// One token inside a [`UseLayer`]: clear-all or a signed flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LayerTok {
    /// `-*` — discard everything accumulated from lower layers so far.
    ClearAll,
    /// `flag` / `+flag` / `-flag` with the name already interned.
    Flag {
        flag: Interned<DefaultInterner>,
        enable: bool,
    },
}

/// A pre-tokenized USE layer (profile `pre_env` or process `env_use`).
///
/// Profile USE_EXPAND folding can put a hundred-plus flags into `pre_env`.
/// Parse that string **once** at config load ([`UseLayer::parse`]): intern
/// tokens and freeze the layer's standalone fold into an [`Arc`] map so every
/// per-package [`resolve_effective_use`] can share the profile base without
/// re-applying those flags into a fresh map.
#[derive(Debug, Clone)]
pub struct UseLayer {
    tokens: Vec<LayerTok>,
    /// Whether this layer contains any `-*` (clears lower layers when applied).
    has_clear_all: bool,
    /// This layer folded alone from empty — shared via [`Arc`].
    frozen: Arc<HashMap<Interned<DefaultInterner>, bool>>,
    /// Lowercased flag prefixes (`l10n_`, `video_cards_`, …) of the
    /// `USE_EXPAND` groups this layer *explicitly assigned*.
    ///
    /// Portage's `is_not_incremental` branch: assigning a `USE_EXPAND`
    /// variable at any non-`defaults` config layer wipes every accumulated
    /// flag with that group's prefix from lower layers before this layer's
    /// own values apply. The flat USE string gets that treatment already
    /// (`strip_expand_tokens`), but the ebuild's own `+`-defaulted IUSE is
    /// only known per package — so the group travels with the layer.
    group_clears: Arc<[String]>,
}

impl Default for UseLayer {
    fn default() -> Self {
        Self {
            tokens: Vec::new(),
            has_clear_all: false,
            frozen: Arc::new(HashMap::new()),
            group_clears: Arc::from([]),
        }
    }
}

impl PartialEq for UseLayer {
    fn eq(&self, other: &Self) -> bool {
        self.tokens == other.tokens && self.group_clears == other.group_clears
    }
}

impl Eq for UseLayer {}

impl UseLayer {
    /// Empty layer (no tokens).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Split and intern a whitespace-separated USE string once.
    ///
    /// Accepts the same tokens as a profile/`make.conf`/`USE=` fold:
    /// `flag`, `+flag`, `-flag`, and `-*`.
    pub fn parse(s: &str) -> Self {
        let mut tokens = Vec::new();
        for tok in s.split_whitespace() {
            if tok == "-*" {
                tokens.push(LayerTok::ClearAll);
                continue;
            }
            let name = tok.strip_prefix('+').unwrap_or(tok);
            match name.strip_prefix('-') {
                Some(rest) => tokens.push(LayerTok::Flag {
                    flag: Interned::intern(rest),
                    enable: false,
                }),
                None => tokens.push(LayerTok::Flag {
                    flag: Interned::intern(name),
                    enable: true,
                }),
            }
        }
        let (frozen, has_clear_all) = freeze_tokens(&tokens);
        Self {
            tokens,
            has_clear_all,
            frozen: Arc::new(frozen),
            group_clears: Arc::from([]),
        }
    }

    /// Record the `USE_EXPAND` groups this layer explicitly assigned, by
    /// variable name as declared (`L10N`, `VIDEO_CARDS`, …).
    ///
    /// See the `group_clears` field doc.
    pub fn with_group_clears(mut self, vars: impl IntoIterator<Item = String>) -> Self {
        self.group_clears = vars
            .into_iter()
            .map(|v| format!("{}_", v.to_lowercase()))
            .collect::<Vec<_>>()
            .into();
        self
    }

    /// Whether `flag` belongs to a `USE_EXPAND` group this layer explicitly
    /// assigned — i.e. whether the fold must drop `flag` when it came from a
    /// layer below this one.
    fn clears_group(&self, flag: Interned<DefaultInterner>) -> bool {
        if self.group_clears.is_empty() {
            // Hot path: skip resolving the interned name entirely.
            return false;
        }
        let name = flag.as_str();
        self.group_clears
            .iter()
            .any(|p| name.starts_with(p.as_str()))
    }

    /// Whether this layer contributes no tokens.
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// Number of tokens (including `-*`).
    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    /// Whether this layer contains `-*` (wipes everything accumulated below it).
    pub fn has_clear_all(&self) -> bool {
        self.has_clear_all
    }
}

/// Fold tokens from empty into a map; report whether any `-*` appeared.
fn freeze_tokens(tokens: &[LayerTok]) -> (HashMap<Interned<DefaultInterner>, bool>, bool) {
    let mut state = HashMap::new();
    let mut has_clear = false;
    for tok in tokens {
        match *tok {
            LayerTok::ClearAll => {
                state.clear();
                has_clear = true;
            }
            LayerTok::Flag { flag, enable } => {
                state.insert(flag, enable);
            }
        }
    }
    (state, has_clear)
}

/// Resolve a single package's effective USE.
///
/// This is the **only** place "is flag F on for package P" gets decided —
/// every consumer that used to re-derive its own "unset flag → check the
/// IUSE default" fallback must call this function instead; do not
/// reimplement any part of this fold elsewhere. See [USE stacking
/// precedence](../../docs/design/architecture.md) for the full fold order,
/// the `-*` group-clear semantics, and the live-verified vscode/L10N case.
///
/// Folds four token groups, in exactly portage's own USE-resolution order
/// (`pkginternal < defaults/conf < pkg < env`):
///
/// 1. `iuse_defaults` — the ebuild's own `+`/`-` IUSE defaults (`pkginternal`).
/// 2. `pre_env` — the profile/`make.conf` fold, already computed by
///    `portage_repo`'s `ResolvedUse::pre_env`, parsed into a [`UseLayer`].
/// 3. This package's matching `package_use` entries (`pkg`).
/// 4. `env_use` — the process-environment USE layer ([`UseLayer`]), unmerged.
#[allow(clippy::too_many_arguments)]
pub fn resolve_effective_use(
    iuse_defaults: &HashMap<Interned<DefaultInterner>, IUseDefault>,
    pre_env: &UseLayer,
    cpv: &Cpv,
    slot: Option<Interned<DefaultInterner>>,
    package_use: &[(Dep, Vec<UseOverride>)],
    env_use: &UseLayer,
    profile_package_use: &[(Dep, Vec<UseOverride>)],
) -> UseConfig {
    // Fast path for the common case (and most `-*` cases that only appear in
    // pre_env/env as whole-layer clears):
    //
    // Fold order is iuse < pre_env < package_use < env. When `env` contains
    // `-*`, everything below is wiped — result is env folded alone (already
    // frozen on the layer). When `pre_env` contains `-*`, iuse is wiped but
    // package_use/env still apply on top of the frozen pre_env map.
    //
    // Without those clears: share pre_env's Arc map as UseConfig.base; put
    // only IUSE flags *not* in that map, package_use, and env into a small
    // overlay. No per-CPV re-insertion of ~100 USE_EXPAND flags.

    // Env-level `-*` wipes package.use and pre_env — frozen env is the answer.
    if env_use.has_clear_all {
        return UseConfig::from_base_map(Arc::clone(&env_use.frozen));
    }

    let mut overlay: HashMap<Interned<DefaultInterner>, UseFlagState> = HashMap::new();

    // IUSE (pkginternal): only **enabled** defaults that pre_env does not already
    // set. Disabled defaults match `get()`'s unset→Disabled, so inserting them
    // only bloated the overlay on every CPV. `-*` in pre_env wipes iuse entirely.
    if !pre_env.has_clear_all {
        for (flag, def) in iuse_defaults {
            if matches!(def, IUseDefault::Enabled)
                && !pre_env.frozen.contains_key(flag)
                && !pre_env.clears_group(*flag)
            {
                overlay.insert(*flag, UseFlagState::Enabled);
            }
        }
    }

    // Profile `package.use` for this CPV — sits in portage's *defaults* layer
    // (`config.py`'s `configdict["defaults"]`, populated from `_pkgprofileuse`),
    // which is BELOW `conf` (make.conf). Since `pre_env` is the already-folded
    // `defaults < conf` state, a make.conf `USE=` decision lives in
    // `pre_env.frozen` and must win over a profile `package.use` token — so a
    // profile line like `media-libs/libwebp -tiff` does NOT override a global
    // `USE="tiff"` (unlike user `/etc/portage/package.use`, which does). Only
    // flags pre_env is silent on take the profile's value. `-*` in pre_env
    // wipes this layer along with IUSE (it cleared the whole defaults fold).
    if !pre_env.has_clear_all && !profile_package_use.is_empty() {
        for (dep, overrides) in profile_package_use {
            if dep.cpn != cpv.cpn {
                continue;
            }
            if !atom_matches_cpv(dep, cpv, slot) {
                continue;
            }
            for ov in overrides {
                // Same layer position as the IUSE defaults above, so a
                // make.conf USE_EXPAND assignment wipes these group tokens too.
                if !pre_env.frozen.contains_key(&ov.flag) && !pre_env.clears_group(ov.flag) {
                    overlay.insert(
                        ov.flag,
                        if ov.enable {
                            UseFlagState::Enabled
                        } else {
                            UseFlagState::Disabled
                        },
                    );
                }
            }
        }
    }

    // package.use / package.env for this CPV (above pre_env).
    // Cheap Cpn prefilter before full atom match (version/slot ops).
    if !package_use.is_empty() {
        for (dep, overrides) in package_use {
            if dep.cpn != cpv.cpn {
                continue;
            }
            if !atom_matches_cpv(dep, cpv, slot) {
                continue;
            }
            for ov in overrides {
                overlay.insert(
                    ov.flag,
                    if ov.enable {
                        UseFlagState::Enabled
                    } else {
                        UseFlagState::Disabled
                    },
                );
            }
        }
    }

    // An environment-level USE_EXPAND assignment wipes the group from *every*
    // lower layer — IUSE defaults, `pre_env` (make.conf included) and
    // `package.use` alike — before env's own tokens fold on top. Cleared flags
    // are written Disabled rather than removed: `pre_env.frozen` stays the
    // shared base map, so the overlay is what has to mask it, and an unset
    // flag already reads as Disabled.
    if !env_use.group_clears.is_empty() {
        for flag in pre_env.frozen.keys() {
            if env_use.clears_group(*flag) {
                overlay.insert(*flag, UseFlagState::Disabled);
            }
        }
        for (flag, state) in overlay.iter_mut() {
            if env_use.clears_group(*flag) {
                *state = UseFlagState::Disabled;
            }
        }
    }

    // env (no `-*` — that case returned above); overrides package.use
    if !env_use.frozen.is_empty() {
        for (&flag, &enable) in env_use.frozen.iter() {
            overlay.insert(
                flag,
                if enable {
                    UseFlagState::Enabled
                } else {
                    UseFlagState::Disabled
                },
            );
        }
    }

    let base = if pre_env.frozen.is_empty() {
        None
    } else {
        Some(Arc::clone(&pre_env.frozen))
    };

    UseConfig { base, overlay }
}

/// Whether a dependency atom matches a given `cpv` (+ optional slot).
///
/// Pure helper used by [`resolve_effective_use`]; mirrors the PMS
/// atom-matching operators (including `~` revision-stripping and `=*` glob)
/// without taking a solver dependency.
pub fn atom_matches_cpv(dep: &Dep, cpv: &Cpv, slot: Option<Interned<DefaultInterner>>) -> bool {
    use std::cmp::Ordering;
    if dep.cpn != cpv.cpn {
        return false;
    }
    if let Some(portage_atom::SlotDep::Slot { slot: Some(s), .. }) = &dep.slot_dep
        && slot != Some(s.slot)
    {
        return false;
    }
    match (dep.op, &dep.version) {
        (None, None) => true,
        (Some(op), Some(ver)) => {
            let cmp = cpv.version.cmp(ver);
            match op {
                Operator::Equal => {
                    if dep.glob {
                        cpv.version.glob_matches(ver)
                    } else {
                        cmp == Ordering::Equal
                    }
                }
                Operator::GreaterOrEqual => cmp != Ordering::Less,
                Operator::Greater => cmp == Ordering::Greater,
                Operator::LessOrEqual => cmp != Ordering::Greater,
                Operator::Less => cmp == Ordering::Less,
                Operator::Approximate => {
                    let mut base_target = ver.clone();
                    base_target.revision = Revision::default();
                    let mut base_candidate = cpv.version.clone();
                    base_candidate.revision = Revision::default();
                    base_candidate == base_target
                }
            }
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flag(s: &str) -> Interned<DefaultInterner> {
        Interned::intern(s)
    }

    #[test]
    fn unset_defaults_to_disabled() {
        let config = UseConfig::new();
        assert_eq!(config.get(flag("ssl")), UseFlagState::Disabled);
    }

    #[test]
    fn enable_disable() {
        let mut config = UseConfig::new();
        let f = flag("ssl");
        config.enable(f);
        assert_eq!(config.get(f), UseFlagState::Enabled);
        config.disable(f);
        assert_eq!(config.get(f), UseFlagState::Disabled);
    }

    #[test]
    fn solver_decided_flags_collected() {
        let mut config = UseConfig::new();
        config.enable(flag("ssl"));
        config.solver_decide(flag("debug"), false);
        config.solver_decide(flag("test"), true);
        let decided = config.solver_decided_flags();
        assert_eq!(decided.len(), 2);
    }

    #[test]
    fn set_method_roundtrip() {
        let mut config = UseConfig::new();
        let f = flag("ssl");
        config.set(f, UseFlagState::Enabled);
        assert_eq!(config.get(f), UseFlagState::Enabled);
        config.set(f, UseFlagState::SolverDecided { prefer: false });
        assert_eq!(config.get(f), UseFlagState::SolverDecided { prefer: false });
    }

    #[test]
    fn use_override_parse() {
        let on = UseOverride::parse("ssl");
        assert!(on.enable);
        assert_eq!(on.flag, flag("ssl"));
        // `+flag` enables like `flag`
        assert_eq!(UseOverride::parse("+ssl"), on);
        let off = UseOverride::parse("-ssl");
        assert!(!off.enable);
        assert_eq!(off.flag, flag("ssl"));
        // `-` wins over a leading `+`
        assert!(!UseOverride::parse("+-ssl").enable);
    }

    fn cpv() -> Cpv {
        Cpv::parse("dev-libs/openssl-3.0.0").unwrap()
    }

    fn iuse_defaults(
        pairs: &[(&str, IUseDefault)],
    ) -> HashMap<Interned<DefaultInterner>, IUseDefault> {
        pairs.iter().map(|(f, d)| (flag(f), *d)).collect()
    }

    fn pkg_use(atom: &str, overrides: &[&str]) -> Vec<(Dep, Vec<UseOverride>)> {
        vec![(
            Dep::parse(atom).unwrap(),
            overrides.iter().map(|o| UseOverride::parse(o)).collect(),
        )]
    }

    fn layer(s: &str) -> UseLayer {
        UseLayer::parse(s)
    }

    // No `L10N` assigned anywhere outside profile `make.defaults`: real
    // portage leaves every `+l10n_*` IUSE default enabled (the `defaults`
    // layer is exempt from the group replace). Verified 2026-08-19 against
    // `emerge -pv app-editors/vscode` on portage 3.0.81.2, which listed all
    // 55 locales positive.
    #[test]
    fn unassigned_use_expand_group_keeps_its_iuse_defaults() {
        let cfg = resolve_effective_use(
            &iuse_defaults(&[
                ("l10n_af", IUseDefault::Enabled),
                ("l10n_en-GB", IUseDefault::Enabled),
            ]),
            &layer(""),
            &cpv(),
            None,
            &[],
            &layer(""),
            &[],
        );
        assert_eq!(cfg.get(flag("l10n_af")), UseFlagState::Enabled);
        assert_eq!(cfg.get(flag("l10n_en-GB")), UseFlagState::Enabled);
    }

    // `L10N="en-GB"` in make.conf against an ebuild that `+`-defaults every
    // locale: portage's non-incremental conf-layer replace wipes the group's
    // IUSE defaults, so only the assigned value survives. Flags outside any
    // assigned group are untouched.
    #[test]
    fn conf_use_expand_assignment_wipes_group_iuse_defaults() {
        let cfg = resolve_effective_use(
            &iuse_defaults(&[
                ("l10n_af", IUseDefault::Enabled),
                ("l10n_en-GB", IUseDefault::Enabled),
                ("seccomp", IUseDefault::Enabled),
            ]),
            &layer("l10n_en-GB").with_group_clears(["L10N".to_string()]),
            &cpv(),
            None,
            &[],
            &layer(""),
            &[],
        );
        assert_eq!(cfg.get(flag("l10n_af")), UseFlagState::Disabled);
        assert_eq!(cfg.get(flag("l10n_en-GB")), UseFlagState::Enabled);
        assert_eq!(cfg.get(flag("seccomp")), UseFlagState::Enabled);
    }

    // Profile `package.use` sits in the `defaults` layer, below make.conf, so
    // a conf-level assignment wipes its group tokens; user `package.use` is
    // the `pkg` layer above conf and survives. Verified live: make.conf
    // `L10N="en-GB"` plus `app-editors/vscode l10n_fr` in
    // `/etc/portage/package.use` gives `L10N="en-GB fr"`.
    #[test]
    fn conf_group_clear_spares_user_package_use() {
        let cfg = resolve_effective_use(
            &iuse_defaults(&[("l10n_af", IUseDefault::Enabled)]),
            &layer("l10n_en-GB").with_group_clears(["L10N".to_string()]),
            &cpv(),
            None,
            &pkg_use("dev-libs/openssl", &["l10n_fr"]),
            &layer(""),
            &pkg_use("dev-libs/openssl", &["l10n_de"]),
        );
        assert_eq!(cfg.get(flag("l10n_en-GB")), UseFlagState::Enabled);
        assert_eq!(cfg.get(flag("l10n_af")), UseFlagState::Disabled);
        assert_eq!(
            cfg.get(flag("l10n_de")),
            UseFlagState::Disabled,
            "profile package.use is below conf's L10N assignment"
        );
        assert_eq!(
            cfg.get(flag("l10n_fr")),
            UseFlagState::Enabled,
            "user package.use is above conf and must survive"
        );
    }

    // `L10N=de em …`: the env layer's replace reaches every lower layer —
    // IUSE defaults, the make.conf value inside `pre_env`, and user
    // `package.use` alike. Verified live: the same vscode run yields exactly
    // `L10N="de"`.
    #[test]
    fn env_use_expand_assignment_wipes_group_from_all_lower_layers() {
        let cfg = resolve_effective_use(
            &iuse_defaults(&[("l10n_af", IUseDefault::Enabled)]),
            &layer("l10n_en-GB").with_group_clears(["L10N".to_string()]),
            &cpv(),
            None,
            &pkg_use("dev-libs/openssl", &["l10n_fr"]),
            &layer("l10n_de").with_group_clears(["L10N".to_string()]),
            &[],
        );
        assert_eq!(cfg.get(flag("l10n_de")), UseFlagState::Enabled);
        assert_eq!(cfg.get(flag("l10n_af")), UseFlagState::Disabled);
        assert_eq!(cfg.get(flag("l10n_en-GB")), UseFlagState::Disabled);
        assert_eq!(cfg.get(flag("l10n_fr")), UseFlagState::Disabled);
    }

    // `L10N=""` — an explicit empty assignment still clears the group, which
    // is how a user turns every locale off (`emerge -pv` shows the whole
    // group negative).
    #[test]
    fn empty_use_expand_assignment_still_clears_the_group() {
        let cfg = resolve_effective_use(
            &iuse_defaults(&[("l10n_af", IUseDefault::Enabled)]),
            &layer("").with_group_clears(["L10N".to_string()]),
            &cpv(),
            None,
            &[],
            &layer(""),
            &[],
        );
        assert_eq!(cfg.get(flag("l10n_af")), UseFlagState::Disabled);
    }

    #[test]
    fn resolve_effective_use_baseline_no_wildcard() {
        // No -* anywhere: package.use applies normally, matching real emerge's
        // baseline behaviour (m4 nls with no override).
        let cfg = resolve_effective_use(
            &iuse_defaults(&[]),
            &layer(""),
            &cpv(),
            None,
            &pkg_use("dev-libs/openssl", &["ssl"]),
            &layer(""),
            &[],
        );
        assert_eq!(cfg.get(flag("ssl")), UseFlagState::Enabled);
    }

    #[test]
    fn resolve_effective_use_package_use_survives_conf_level_wildcard() {
        // A `-*` in `pre_env` (i.e. from profile make.defaults or make.conf)
        // does NOT wipe package.use — confirmed against real emerge: adding
        // `USE="-* build"` to make.conf still let `package.use: sys-devel/m4
        // nls` apply.
        let cfg = resolve_effective_use(
            &iuse_defaults(&[]),
            &layer("-* build"),
            &cpv(),
            None,
            &pkg_use("dev-libs/openssl", &["ssl"]),
            &layer(""),
            &[],
        );
        assert_eq!(cfg.get(flag("ssl")), UseFlagState::Enabled);
        assert_eq!(cfg.get(flag("build")), UseFlagState::Enabled);
    }

    #[test]
    fn resolve_effective_use_package_use_wiped_by_env_level_wildcard() {
        // A `-*` in `env_use` (the raw process environment) DOES wipe
        // package.use — confirmed against real emerge: `USE="-* build"` at
        // invocation left `package.use: sys-devel/m4 nls` with zero effect.
        let cfg = resolve_effective_use(
            &iuse_defaults(&[]),
            &layer(""),
            &cpv(),
            None,
            &pkg_use("dev-libs/openssl", &["ssl"]),
            &layer("-* build"),
            &[],
        );
        assert_eq!(cfg.get(flag("ssl")), UseFlagState::Disabled);
        assert_eq!(cfg.get(flag("build")), UseFlagState::Enabled);
    }

    #[test]
    fn resolve_effective_use_env_presence_without_wildcard_does_not_suppress_package_use() {
        // `USE="build"` (env, no `-*`) must NOT suppress package.use —
        // confirmed against real emerge.
        let cfg = resolve_effective_use(
            &iuse_defaults(&[]),
            &layer(""),
            &cpv(),
            None,
            &pkg_use("dev-libs/openssl", &["ssl"]),
            &layer("build"),
            &[],
        );
        assert_eq!(cfg.get(flag("ssl")), UseFlagState::Enabled);
        assert_eq!(cfg.get(flag("build")), UseFlagState::Enabled);
    }

    #[test]
    fn resolve_effective_use_iuse_default_suppressed_by_conf_level_wildcard() {
        // pkginternal sits *below* both conf and env, so a `-*` in `pre_env`
        // wipes a `+`-defaulted IUSE flag too — confirmed against real
        // emerge's app-alternatives/awk `+gawk` default.
        let cfg = resolve_effective_use(
            &iuse_defaults(&[("quic", IUseDefault::Enabled)]),
            &layer("-* build"),
            &cpv(),
            None,
            &[],
            &layer(""),
            &[],
        );
        assert_eq!(cfg.get(flag("quic")), UseFlagState::Disabled);
    }

    #[test]
    fn resolve_effective_use_iuse_default_suppressed_by_env_level_wildcard() {
        let cfg = resolve_effective_use(
            &iuse_defaults(&[("quic", IUseDefault::Enabled)]),
            &layer(""),
            &cpv(),
            None,
            &[],
            &layer("-* build"),
            &[],
        );
        assert_eq!(cfg.get(flag("quic")), UseFlagState::Disabled);
    }

    #[test]
    fn resolve_effective_use_iuse_default_kept_without_any_wildcard() {
        let cfg = resolve_effective_use(
            &iuse_defaults(&[("quic", IUseDefault::Enabled)]),
            &layer(""),
            &cpv(),
            None,
            &[],
            &layer(""),
            &[],
        );
        assert_eq!(cfg.get(flag("quic")), UseFlagState::Enabled);
    }

    #[test]
    fn resolve_effective_use_explicit_config_beats_iuse_default() {
        // pre_env explicitly disabling a flag must survive even though the
        // ebuild's own IUSE default is `+` (portage's USE-over-IUSE-default
        // precedence) — pkginternal is folded first, so a later explicit
        // -flag in pre_env/pkg/env always overrides it.
        let cfg = resolve_effective_use(
            &iuse_defaults(&[("ssl", IUseDefault::Enabled)]),
            &layer("-ssl"),
            &cpv(),
            None,
            &[],
            &layer(""),
            &[],
        );
        assert_eq!(cfg.get(flag("ssl")), UseFlagState::Disabled);
    }

    #[test]
    fn resolve_effective_use_package_use_only_applies_to_matching_atom() {
        let cfg = resolve_effective_use(
            &iuse_defaults(&[]),
            &layer(""),
            &cpv(),
            None,
            &pkg_use("dev-libs/other", &["ssl"]),
            &layer(""),
            &[],
        );
        assert_eq!(cfg.get(flag("ssl")), UseFlagState::Disabled);
    }

    #[test]
    fn resolve_effective_use_package_use_disable_overrides_pre_env_enable() {
        let cfg = resolve_effective_use(
            &iuse_defaults(&[]),
            &layer("ssl"),
            &cpv(),
            None,
            &pkg_use("dev-libs/openssl", &["-ssl"]),
            &layer(""),
            &[],
        );
        assert_eq!(cfg.get(flag("ssl")), UseFlagState::Disabled);
    }

    // Profile `package.use` sits in portage's *defaults* layer, BELOW
    // make.conf (`conf`): a `USE=` set in make.conf (the `pre_env` layer
    // here) wins over a profile `package.use -flag`. Contrast
    // `resolve_effective_use_package_use_disable_overrides_pre_env_enable`
    // above — that's USER `/etc/portage/package.use`, which sits in the
    // `pkg` layer above `conf` and DOES override it. Regression for the
    // `media-libs/libwebp -tiff` divergence from real emerge (Nathan's
    // report, 2026-08-11): `targets/desktop/package.use`'s `-tiff` was
    // incorrectly overriding a global `USE="tiff"`.
    #[test]
    fn resolve_effective_use_profile_package_use_does_not_override_make_conf() {
        let cfg = resolve_effective_use(
            &iuse_defaults(&[]),
            &layer("ssl"),
            &cpv(),
            None,
            &pkg_use("dev-libs/openssl", &[]),
            &layer(""),
            &pkg_use("dev-libs/openssl", &["-ssl"]),
        );
        assert_eq!(
            cfg.get(flag("ssl")),
            UseFlagState::Enabled,
            "profile package.use -ssl must NOT override a make.conf USE=ssl"
        );
    }

    // The flip side: when make.conf is silent on a flag, profile
    // `package.use` DOES set it (that's its purpose — e.g. a profile
    // enabling `pulseaudio` for `media-sound/alsa-plugins` when global
    // USE doesn't mention it).
    #[test]
    fn resolve_effective_use_profile_package_use_applies_when_make_conf_silent() {
        let cfg = resolve_effective_use(
            &iuse_defaults(&[]),
            &layer(""),
            &cpv(),
            None,
            &pkg_use("dev-libs/openssl", &[]),
            &layer(""),
            &pkg_use("dev-libs/openssl", &["-ssl"]),
        );
        assert_eq!(
            cfg.get(flag("ssl")),
            UseFlagState::Disabled,
            "profile package.use -ssl must apply when make.conf is silent on ssl"
        );
    }

    #[test]
    fn atom_matches_cpv_version_operators() {
        let cpv = Cpv::parse("dev-libs/openssl-3.0.0").unwrap();
        assert!(atom_matches_cpv(
            &Dep::parse(">=dev-libs/openssl-3.0.0").unwrap(),
            &cpv,
            None
        ));
        assert!(!atom_matches_cpv(
            &Dep::parse(">dev-libs/openssl-3.0.0").unwrap(),
            &cpv,
            None
        ));
        assert!(!atom_matches_cpv(
            &Dep::parse("dev-lang/rust").unwrap(),
            &cpv,
            None
        ));
    }
}
