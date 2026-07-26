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
/// every flag the package cares about already decided — there is no separate
/// "fall back to the IUSE default" step, because [`resolve_effective_use`]
/// already folded the ebuild's own `+`/`-` IUSE defaults in at their correct
/// position in portage's real USE-resolution order. See that function's doc
/// for why a config built any other way (e.g. a bare [`UseConfig::new`] plus
/// ad hoc overrides) must not be treated as authoritative for a real package.
#[derive(Debug, Clone, Default)]
pub struct UseConfig {
    flags: HashMap<Interned<DefaultInterner>, UseFlagState>,
}

impl UseConfig {
    /// Create an empty config (all flags default to `Disabled`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a flag's state.
    pub fn set(&mut self, flag: Interned<DefaultInterner>, state: UseFlagState) {
        self.flags.insert(flag, state);
    }

    /// Enable a flag.
    pub fn enable(&mut self, flag: Interned<DefaultInterner>) {
        self.flags.insert(flag, UseFlagState::Enabled);
    }

    /// Disable a flag.
    pub fn disable(&mut self, flag: Interned<DefaultInterner>) {
        self.flags.insert(flag, UseFlagState::Disabled);
    }

    /// Mark a flag as solver-decided, with the caller's preferred value.
    pub fn solver_decide(&mut self, flag: Interned<DefaultInterner>, prefer: bool) {
        self.flags
            .insert(flag, UseFlagState::SolverDecided { prefer });
    }

    /// Get the state of a flag. Unset flags default to `Disabled`.
    pub fn get(&self, flag: Interned<DefaultInterner>) -> UseFlagState {
        self.flags
            .get(&flag)
            .copied()
            .unwrap_or(UseFlagState::Disabled)
    }

    /// Return `Some(state)` if the flag is explicitly set, `None` if absent.
    pub fn get_opt(&self, flag: Interned<DefaultInterner>) -> Option<UseFlagState> {
        self.flags.get(&flag).copied()
    }

    /// Returns all flags explicitly enabled in this config (sorted, for stable output).
    pub fn enabled_flags(&self) -> Vec<Interned<DefaultInterner>> {
        let mut v: Vec<Interned<DefaultInterner>> = self
            .flags
            .iter()
            .filter(|(_, s)| matches!(s, UseFlagState::Enabled))
            .map(|(f, _)| *f)
            .collect();
        v.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        v
    }

    /// Returns all flags marked `SolverDecided` (the ones ceded to the solver
    /// for Level-C `REQUIRED_USE` handling). Order is not guaranteed.
    pub fn solver_decided_flags(&self) -> Vec<Interned<DefaultInterner>> {
        self.flags
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
/// Parse that string **once** at config load ([`UseLayer::parse`]) and reuse
/// the interned tokens on every per-package [`resolve_effective_use`] call —
/// re-splitting and re-interning the same layer for every CPV is pure waste.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UseLayer {
    tokens: Vec<LayerTok>,
}

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
        Self { tokens }
    }

    /// Whether this layer contributes no tokens.
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// Number of tokens (including `-*`).
    pub fn len(&self) -> usize {
        self.tokens.len()
    }
}

/// Resolve a single package's effective USE.
///
/// This is the **only** place "is flag F on for package P" gets decided —
/// every consumer that used to re-derive its own "unset flag → check the
/// IUSE default" fallback (there were five independent copies of this logic
/// scattered across `portage-cli` before 2026-07-12; see
/// `use-config-duplicate-fallback-logic` in the project's own notes) must
/// call this function instead. Do not reimplement any part of this fold
/// elsewhere.
///
/// Folds four token groups, in exactly portage's own USE-resolution order
/// (`pkginternal < defaults/conf < pkg < env` — Portage's `USE_ORDER`,
/// `config.py`'s `regenerate()`/`setcpv()`):
///
/// 1. `iuse_defaults` — the ebuild's own `+`/`-` IUSE defaults (`pkginternal`).
/// 2. `pre_env` — the profile/`make.conf` fold, already computed by
///    `portage_repo`'s `ResolvedUse::pre_env` (`defaults` + `conf`), parsed
///    once into a [`UseLayer`].
/// 3. This package's matching `package_use` entries (`pkg`).
/// 4. `env_use` — the process-environment USE layer ([`UseLayer`]), unmerged.
///
/// A `-*` token in **any** of these clears exactly what's accumulated from
/// the layers before it — that's an ordinary property of the fold itself,
/// not a derived flag anyone needs to track or branch on. This is why,
/// empirically, `package.use` survives a `-*` in `make.conf` (layer 2) but
/// not one in the environment (layer 4), and why the ebuild's own
/// `+`-defaulted IUSE (layer 1) is wiped by a `-*` in *either* — confirmed
/// against real `emerge` (`em stages --stage1` live-testing, 2026-07-12).
pub fn resolve_effective_use(
    iuse_defaults: &HashMap<Interned<DefaultInterner>, IUseDefault>,
    pre_env: &UseLayer,
    cpv: &Cpv,
    slot: Option<Interned<DefaultInterner>>,
    package_use: &[(Dep, Vec<UseOverride>)],
    env_use: &UseLayer,
) -> UseConfig {
    // Interned fold state — no intermediate whitespace strings, no re-split
    // of the (often large) profile pre_env layer on each call.
    let mut order: Vec<Interned<DefaultInterner>> = Vec::new();
    let mut state: HashMap<Interned<DefaultInterner>, bool> = HashMap::new();

    // 1. pkginternal (IUSE defaults)
    for (flag, def) in iuse_defaults {
        fold_flag(
            &mut order,
            &mut state,
            *flag,
            matches!(def, IUseDefault::Enabled),
        );
    }
    // 2. defaults/conf
    fold_layer(&mut order, &mut state, pre_env);
    // 3. package.use / package.env for this CPV
    for (dep, overrides) in package_use {
        if !atom_matches_cpv(dep, cpv, slot) {
            continue;
        }
        for ov in overrides {
            fold_flag(&mut order, &mut state, ov.flag, ov.enable);
        }
    }
    // 4. process env
    fold_layer(&mut order, &mut state, env_use);

    let mut cfg = UseConfig::new();
    for flag in order {
        if state.get(&flag).copied().unwrap_or(false) {
            cfg.enable(flag);
        } else {
            cfg.disable(flag);
        }
    }
    cfg
}

fn fold_flag(
    order: &mut Vec<Interned<DefaultInterner>>,
    state: &mut HashMap<Interned<DefaultInterner>, bool>,
    flag: Interned<DefaultInterner>,
    enable: bool,
) {
    use std::collections::hash_map::Entry;
    match state.entry(flag) {
        Entry::Vacant(e) => {
            order.push(flag);
            e.insert(enable);
        }
        Entry::Occupied(mut e) => {
            *e.get_mut() = enable;
        }
    }
}

fn fold_layer(
    order: &mut Vec<Interned<DefaultInterner>>,
    state: &mut HashMap<Interned<DefaultInterner>, bool>,
    layer: &UseLayer,
) {
    for tok in &layer.tokens {
        match *tok {
            LayerTok::ClearAll => {
                order.clear();
                state.clear();
            }
            LayerTok::Flag { flag, enable } => fold_flag(order, state, flag, enable),
        }
    }
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
        );
        assert_eq!(cfg.get(flag("ssl")), UseFlagState::Disabled);
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
