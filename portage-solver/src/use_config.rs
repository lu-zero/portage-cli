//! USE flag policy vocabulary
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

/// How a single USE flag should be evaluated during dependency conversion
///
/// See [PMS 8.2](https://projects.gentoo.org/pms/9/pms.html#use-flag-dependent-dependencies).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UseFlagState {
    /// The flag is ON — `flag? ( deps )` includes deps, `!flag? ( deps )` skips
    Enabled,
    /// The flag is OFF — `flag? ( deps )` skips deps, `!flag? ( deps )` includes
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

/// Configuration for USE flag evaluation during dependency conversion
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
    /// Profile/`make.conf` fold shared across packages (`true` = enabled)
    base: Option<Arc<HashMap<Interned<DefaultInterner>, bool>>>,
    /// Package-local and higher-priority decisions (win over [`Self::base`])
    overlay: HashMap<Interned<DefaultInterner>, UseFlagState>,
}

impl UseConfig {
    /// Create an empty config (all flags default to `Disabled`)
    pub fn new() -> Self {
        Self::default()
    }

    /// Config whose only content is a shared frozen layer (e.g. env after `-*`)
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

    /// Set a flag's state
    pub fn set(&mut self, flag: Interned<DefaultInterner>, state: UseFlagState) {
        self.overlay.insert(flag, state);
    }

    /// Enable a flag
    pub fn enable(&mut self, flag: Interned<DefaultInterner>) {
        self.overlay.insert(flag, UseFlagState::Enabled);
    }

    /// Disable a flag
    pub fn disable(&mut self, flag: Interned<DefaultInterner>) {
        self.overlay.insert(flag, UseFlagState::Disabled);
    }

    /// Mark a flag as solver-decided, with the caller's preferred value
    pub fn solver_decide(&mut self, flag: Interned<DefaultInterner>, prefer: bool) {
        self.overlay
            .insert(flag, UseFlagState::SolverDecided { prefer });
    }

    /// Get the state of a flag
    ///
    /// Unset flags default to `Disabled`.
    pub fn get(&self, flag: Interned<DefaultInterner>) -> UseFlagState {
        self.get_opt(flag).unwrap_or(UseFlagState::Disabled)
    }

    /// Return `Some(state)` if the flag is explicitly set, `None` if absent
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

    /// Returns all flags explicitly enabled in this config (sorted, for stable output)
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

/// A parsed `package.use` override: a USE flag and whether it is turned on
///
/// Parsing (`+flag`/`flag` → on, `-flag` → off) and interning happen once at
/// config-read time (via [`UseOverride::parse`]) so the per-version
/// [`resolve_effective_use`] call does no string work. Cheap to copy (an
/// interned `u32` plus a bool).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UseOverride {
    /// The interned flag name, with any `+`/`-` prefix stripped
    pub flag: Interned<DefaultInterner>,
    /// `true` enables the flag, `false` disables it
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

/// One token inside a [`UseLayer`]: clear-all or a signed flag
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LayerTok {
    /// `-*` — discard everything accumulated from lower layers so far
    ClearAll,
    /// `flag` / `+flag` / `-flag` with the name already interned
    Flag {
        flag: Interned<DefaultInterner>,
        enable: bool,
    },
}

/// A pre-tokenized USE layer (profile `pre_env` or process `env_use`)
///
/// Profile USE_EXPAND folding can put a hundred-plus flags into `pre_env`.
/// Parse that string **once** at config load ([`UseLayer::parse`]): intern
/// tokens and freeze the layer's standalone fold into an [`Arc`] map so every
/// per-package [`resolve_effective_use`] can share the profile base without
/// re-applying those flags into a fresh map.
#[derive(Debug, Clone)]
pub struct UseLayer {
    tokens: Vec<LayerTok>,
    /// Whether this layer contains any `-*` (clears lower layers when applied)
    has_clear_all: bool,
    /// This layer folded alone from empty — shared via [`Arc`]
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
    /// Empty layer (no tokens)
    pub fn empty() -> Self {
        Self::default()
    }

    /// Split and intern a whitespace-separated USE string once
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

    /// Whether this layer contributes no tokens
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// Number of tokens (including `-*`)
    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    /// Whether this layer contains `-*` (wipes everything accumulated below it)
    pub fn has_clear_all(&self) -> bool {
        self.has_clear_all
    }

    /// Whether this layer assigned `flag` (enabled or disabled).
    pub fn mentions(&self, flag: Interned<DefaultInterner>) -> bool {
        self.frozen.contains_key(&flag)
    }
}

/// One profile-chain node in Portage's `defaults` layer (`setcpv` interleave).
///
/// Walked parent-first: this node's `make.defaults` then matching `package.use`.
/// [`resolve_effective_use`] keeps folded make.defaults as the shared Arc base
/// and only overlays `package_use`; `defaults` is the per-node delta used to
/// skip a parent `package.use` token a later node's make.defaults restates.
#[derive(Debug, Clone, Default)]
pub struct ProfileUseNode {
    /// This node's translated `make.defaults` USE (empty if the file is absent).
    pub defaults: UseLayer,
    /// This node's `package.use` lines.
    pub package_use: Vec<(Dep, Vec<UseOverride>)>,
    /// A matching line in [`Self::package_use`] contained `-*`.
    pub puse_clear_all: bool,
}

/// Fold tokens from empty into a map; report whether any `-*` appeared
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

/// Resolve a single package's effective USE
///
/// This is the **only** place "is flag F on for package P" gets decided —
/// every consumer that used to re-derive its own "unset flag → check the
/// IUSE default" fallback must call this function instead; do not
/// reimplement any part of this fold elsewhere. See [USE stacking
/// precedence](../../docs/design/architecture.md) for the full fold order,
/// the `-*` group-clear semantics, and the live-verified vscode/L10N case.
///
/// Folds Portage's `USE_ORDER` stack (`pkginternal < defaults < conf < pkg < env`):
///
/// 1. `iuse_defaults` — the ebuild's own `+`/`-` IUSE defaults (`pkginternal`).
/// 2. `defaults` — folded profile `make.defaults` (shared Arc base).
/// 3. `profile_package_use` — per-node profile `package.use`, same `defaults`
///    layer, after that node's make.defaults (a later node's make.defaults
///    wins over an earlier node's `package.use`).
/// 4. `conf` — `make.conf` only, above profile `package.use`.
/// 5. `package_use` — `/etc/portage/package.use` (`pkg`).
/// 6. `env_use` — process-environment USE ([`UseLayer`]), unmerged.
#[allow(clippy::too_many_arguments)]
pub fn resolve_effective_use(
    iuse_defaults: &HashMap<Interned<DefaultInterner>, IUseDefault>,
    defaults: &UseLayer,
    cpv: &Cpv,
    slot: Option<Interned<DefaultInterner>>,
    package_use: &[(Dep, Vec<UseOverride>)],
    env_use: &UseLayer,
    profile_package_use: &[ProfileUseNode],
    conf: &UseLayer,
) -> UseConfig {
    // Env-level `-*` wipes everything below — frozen env is the answer.
    if env_use.has_clear_all {
        return UseConfig::from_base_map(Arc::clone(&env_use.frozen));
    }

    let conf_wipes_below = conf.has_clear_all;
    let replay = !conf_wipes_below
        && profile_package_use
            .iter()
            .any(|n| n.puse_clear_all && node_matches(n, cpv, slot));

    let mut overlay: HashMap<Interned<DefaultInterner>, UseFlagState> = HashMap::new();

    if replay {
        apply_iuse(&mut overlay, iuse_defaults, defaults);
        for node in profile_package_use {
            apply_layer_delta(&mut overlay, &node.defaults);
            if node_matches(node, cpv, slot) {
                if node.puse_clear_all {
                    overlay.clear();
                }
                apply_overrides(&mut overlay, &node.package_use, cpv, slot, |_| false);
            }
        }
    } else if !conf_wipes_below {
        apply_iuse(&mut overlay, iuse_defaults, defaults);
        apply_profile_package_use(&mut overlay, profile_package_use, cpv, slot);
    }

    if !conf_wipes_below {
        apply_group_clears(&mut overlay, conf, defaults);
        apply_layer_tokens(&mut overlay, conf);
    }

    apply_overrides(&mut overlay, package_use, cpv, slot, |_| false);

    apply_group_clears(
        &mut overlay,
        env_use,
        if conf_wipes_below { conf } else { defaults },
    );
    apply_layer_tokens(&mut overlay, env_use);

    let base = if conf_wipes_below {
        if conf.frozen.is_empty() {
            None
        } else {
            Some(Arc::clone(&conf.frozen))
        }
    } else if replay || defaults.frozen.is_empty() {
        None
    } else {
        Some(Arc::clone(&defaults.frozen))
    };

    UseConfig { base, overlay }
}

fn node_matches(node: &ProfileUseNode, cpv: &Cpv, slot: Option<Interned<DefaultInterner>>) -> bool {
    node.package_use
        .iter()
        .any(|(dep, _)| dep.cpn == cpv.cpn && atom_matches_cpv(dep, cpv, slot))
}

fn apply_iuse(
    overlay: &mut HashMap<Interned<DefaultInterner>, UseFlagState>,
    iuse_defaults: &HashMap<Interned<DefaultInterner>, IUseDefault>,
    defaults: &UseLayer,
) {
    if defaults.has_clear_all {
        return;
    }
    for (flag, def) in iuse_defaults {
        if matches!(def, IUseDefault::Enabled)
            && !defaults.frozen.contains_key(flag)
            && !defaults.clears_group(*flag)
        {
            overlay.insert(*flag, UseFlagState::Enabled);
        }
    }
}

fn apply_profile_package_use(
    overlay: &mut HashMap<Interned<DefaultInterner>, UseFlagState>,
    nodes: &[ProfileUseNode],
    cpv: &Cpv,
    slot: Option<Interned<DefaultInterner>>,
) {
    if nodes.is_empty() {
        return;
    }
    for (i, node) in nodes.iter().enumerate() {
        if !node_matches(node, cpv, slot) {
            continue;
        }
        apply_overrides(overlay, &node.package_use, cpv, slot, |flag| {
            nodes[i + 1..]
                .iter()
                .any(|later| later.defaults.has_clear_all || later.defaults.mentions(flag))
        });
    }
}

fn apply_overrides(
    overlay: &mut HashMap<Interned<DefaultInterner>, UseFlagState>,
    entries: &[(Dep, Vec<UseOverride>)],
    cpv: &Cpv,
    slot: Option<Interned<DefaultInterner>>,
    skip: impl Fn(Interned<DefaultInterner>) -> bool,
) {
    for (dep, overrides) in entries {
        if dep.cpn != cpv.cpn {
            continue;
        }
        if !atom_matches_cpv(dep, cpv, slot) {
            continue;
        }
        for ov in overrides {
            if skip(ov.flag) {
                continue;
            }
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

fn apply_layer_delta(
    overlay: &mut HashMap<Interned<DefaultInterner>, UseFlagState>,
    layer: &UseLayer,
) {
    if layer.has_clear_all {
        overlay.clear();
    }
    apply_layer_tokens(overlay, layer);
}

fn apply_layer_tokens(
    overlay: &mut HashMap<Interned<DefaultInterner>, UseFlagState>,
    layer: &UseLayer,
) {
    for (&flag, &enable) in layer.frozen.iter() {
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

fn apply_group_clears(
    overlay: &mut HashMap<Interned<DefaultInterner>, UseFlagState>,
    layer: &UseLayer,
    below: &UseLayer,
) {
    if layer.group_clears.is_empty() {
        return;
    }
    for flag in below.frozen.keys() {
        if layer.clears_group(*flag) {
            overlay.insert(*flag, UseFlagState::Disabled);
        }
    }
    for (flag, state) in overlay.iter_mut() {
        if layer.clears_group(*flag) {
            *state = UseFlagState::Disabled;
        }
    }
}

/// Whether a dependency atom matches a given `cpv` (+ optional slot)
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

    fn pnodes(atom: &str, overrides: &[&str]) -> Vec<ProfileUseNode> {
        vec![ProfileUseNode {
            defaults: UseLayer::empty(),
            package_use: pkg_use(atom, overrides),
            puse_clear_all: false,
        }]
    }

    fn none() -> UseLayer {
        UseLayer::empty()
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
            &none(),
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
            &layer(""),
            &cpv(),
            None,
            &[],
            &layer(""),
            &[],
            &layer("l10n_en-GB").with_group_clears(["L10N".to_string()]),
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
            &layer(""),
            &cpv(),
            None,
            &pkg_use("dev-libs/openssl", &["l10n_fr"]),
            &layer(""),
            &pnodes("dev-libs/openssl", &["l10n_de"]),
            &layer("l10n_en-GB").with_group_clears(["L10N".to_string()]),
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
            &layer(""),
            &cpv(),
            None,
            &pkg_use("dev-libs/openssl", &["l10n_fr"]),
            &layer("l10n_de").with_group_clears(["L10N".to_string()]),
            &[],
            &layer("l10n_en-GB").with_group_clears(["L10N".to_string()]),
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
            &layer(""),
            &cpv(),
            None,
            &[],
            &layer(""),
            &[],
            &layer("").with_group_clears(["L10N".to_string()]),
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
            &none(),
        );
        assert_eq!(cfg.get(flag("ssl")), UseFlagState::Enabled);
    }

    #[test]
    fn resolve_effective_use_package_use_survives_conf_level_wildcard() {
        // A `-*` in make.conf does NOT wipe user package.use — confirmed
        // against real emerge: `USE="-* build"` in make.conf still let
        // `package.use: sys-devel/m4 nls` apply.
        let cfg = resolve_effective_use(
            &iuse_defaults(&[]),
            &layer(""),
            &cpv(),
            None,
            &pkg_use("dev-libs/openssl", &["ssl"]),
            &layer(""),
            &[],
            &layer("-* build"),
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
            &none(),
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
            &none(),
        );
        assert_eq!(cfg.get(flag("ssl")), UseFlagState::Enabled);
        assert_eq!(cfg.get(flag("build")), UseFlagState::Enabled);
    }

    #[test]
    fn resolve_effective_use_iuse_default_suppressed_by_conf_level_wildcard() {
        // pkginternal sits *below* conf, so a `-*` in make.conf wipes a
        // `+`-defaulted IUSE flag too — confirmed against real emerge's
        // app-alternatives/awk `+gawk` default.
        let cfg = resolve_effective_use(
            &iuse_defaults(&[("quic", IUseDefault::Enabled)]),
            &layer(""),
            &cpv(),
            None,
            &[],
            &layer(""),
            &[],
            &layer("-* build"),
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
            &none(),
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
            &none(),
        );
        assert_eq!(cfg.get(flag("quic")), UseFlagState::Enabled);
    }

    #[test]
    fn resolve_effective_use_explicit_config_beats_iuse_default() {
        // make.defaults explicitly disabling a flag must survive even though
        // the ebuild's own IUSE default is `+` — pkginternal is folded first.
        let cfg = resolve_effective_use(
            &iuse_defaults(&[("ssl", IUseDefault::Enabled)]),
            &layer("-ssl"),
            &cpv(),
            None,
            &[],
            &layer(""),
            &[],
            &none(),
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
            &none(),
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
            &none(),
        );
        assert_eq!(cfg.get(flag("ssl")), UseFlagState::Disabled);
    }

    // Profile `package.use` sits in portage's `defaults` layer, below
    // make.conf: a `USE=` set in make.conf wins over a profile `-flag`.
    #[test]
    fn resolve_effective_use_profile_package_use_does_not_override_make_conf() {
        let cfg = resolve_effective_use(
            &iuse_defaults(&[]),
            &layer(""),
            &cpv(),
            None,
            &[],
            &layer(""),
            &pnodes("dev-libs/openssl", &["-ssl"]),
            &layer("ssl"),
        );
        assert_eq!(
            cfg.get(flag("ssl")),
            UseFlagState::Enabled,
            "profile package.use -ssl must NOT override a make.conf USE=ssl"
        );
    }

    // Same-node desktop case: make.defaults enables the flag, profile
    // `package.use` turns it off, make.conf is silent → off.
    #[test]
    fn resolve_effective_use_profile_package_use_overrides_make_defaults() {
        let cfg = resolve_effective_use(
            &iuse_defaults(&[]),
            &layer("ssl"),
            &cpv(),
            None,
            &[],
            &layer(""),
            &pnodes("dev-libs/openssl", &["-ssl"]),
            &none(),
        );
        assert_eq!(
            cfg.get(flag("ssl")),
            UseFlagState::Disabled,
            "profile package.use -ssl must override make.defaults USE=ssl"
        );
    }

    // When make.conf is silent and make.defaults is too, profile
    // `package.use` still sets the flag.
    #[test]
    fn resolve_effective_use_profile_package_use_applies_when_make_conf_silent() {
        let cfg = resolve_effective_use(
            &iuse_defaults(&[]),
            &layer(""),
            &cpv(),
            None,
            &[],
            &layer(""),
            &pnodes("dev-libs/openssl", &["-ssl"]),
            &none(),
        );
        assert_eq!(
            cfg.get(flag("ssl")),
            UseFlagState::Disabled,
            "profile package.use -ssl must apply when make.conf is silent on ssl"
        );
    }

    // Parent package.use disables; child make.defaults restates the flag
    // (already in the folded defaults base) → child wins.
    #[test]
    fn child_make_defaults_beats_parent_package_use() {
        let nodes = vec![
            ProfileUseNode {
                defaults: layer("ssl"),
                package_use: pkg_use("dev-libs/openssl", &["-ssl"]),
                puse_clear_all: false,
            },
            ProfileUseNode {
                defaults: layer("ssl"),
                package_use: vec![],
                puse_clear_all: false,
            },
        ];
        let cfg = resolve_effective_use(
            &iuse_defaults(&[]),
            &layer("ssl"),
            &cpv(),
            None,
            &[],
            &layer(""),
            &nodes,
            &none(),
        );
        assert_eq!(
            cfg.get(flag("ssl")),
            UseFlagState::Enabled,
            "child make.defaults restating ssl beats parent package.use -ssl"
        );
    }

    #[test]
    fn conf_wildcard_wipes_profile_package_use_not_user() {
        let cfg = resolve_effective_use(
            &iuse_defaults(&[]),
            &layer("foo"),
            &cpv(),
            None,
            &pkg_use("dev-libs/openssl", &["ssl"]),
            &layer(""),
            &pnodes("dev-libs/openssl", &["bar"]),
            &layer("-* build"),
        );
        assert_eq!(cfg.get(flag("foo")), UseFlagState::Disabled);
        assert_eq!(cfg.get(flag("bar")), UseFlagState::Disabled);
        assert_eq!(cfg.get(flag("ssl")), UseFlagState::Enabled);
        assert_eq!(cfg.get(flag("build")), UseFlagState::Enabled);
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
