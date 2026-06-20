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

use std::borrow::Cow;
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

    /// Get the state of a flag, falling back to an IUSE default if the flag
    /// is not explicitly configured.
    ///
    /// If the flag is set in the config, returns its state. Otherwise, if the
    /// IUSE default is `Enabled` (the `+` prefix), returns `Enabled`.
    /// Otherwise returns `Disabled`.
    pub fn get_with_iuse_default(
        &self,
        flag: Interned<DefaultInterner>,
        iuse_default: Option<IUseDefault>,
    ) -> UseFlagState {
        match self.flags.get(&flag) {
            Some(&state) => state,
            None => match iuse_default {
                Some(IUseDefault::Enabled) => UseFlagState::Enabled,
                _ => UseFlagState::Disabled,
            },
        }
    }

    /// Fold a version's IUSE defaults into this config: for every flag not
    /// already set explicitly, apply its `+`/`-` default. After this, the
    /// config is an authoritative "desired" set — a plain `get()` gives the
    /// flag's effective state with no separate default lookup needed.
    pub fn fold_iuse_defaults(
        &mut self,
        defaults: &HashMap<Interned<DefaultInterner>, IUseDefault>,
    ) {
        for (flag, def) in defaults {
            if !self.flags.contains_key(flag) {
                self.flags.insert(
                    *flag,
                    match def {
                        IUseDefault::Enabled => UseFlagState::Enabled,
                        IUseDefault::Disabled => UseFlagState::Disabled,
                    },
                );
            }
        }
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
/// [`apply_package_use`] call does no string work. Cheap to copy (an interned
/// `u32` plus a bool).
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

/// Apply per-package USE flag overrides on top of a base [`UseConfig`].
///
/// Scans `package_use` in order and applies any entries whose atom matches
/// `cpv`. Returns [`Cow::Borrowed`] when no entries match to avoid a clone.
/// This is policy resolution the *caller* performs to build the desired set;
/// the solver itself never calls it. Overrides are pre-parsed
/// [`UseOverride`]s, so this does no string work.
pub fn apply_package_use<'a>(
    base: &'a UseConfig,
    cpv: &Cpv,
    slot: Option<Interned<DefaultInterner>>,
    package_use: &[(Dep, Vec<UseOverride>)],
) -> Cow<'a, UseConfig> {
    if package_use.is_empty() {
        return Cow::Borrowed(base);
    }
    let mut cfg = base.clone();
    for (dep, overrides) in package_use {
        if atom_matches_cpv(dep, cpv, slot) {
            for ov in overrides {
                if ov.enable {
                    cfg.enable(ov.flag);
                } else {
                    cfg.disable(ov.flag);
                }
            }
        }
    }
    Cow::Owned(cfg)
}

/// Whether a dependency atom matches a given `cpv` (+ optional slot).
///
/// Pure helper used by [`apply_package_use`]; mirrors the PMS atom-matching
/// operators (including `~` revision-stripping and `=*` glob) without taking a
/// solver dependency.
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
    fn get_with_iuse_default_none_is_disabled() {
        assert_eq!(
            UseConfig::new().get_with_iuse_default(flag("ssl"), None),
            UseFlagState::Disabled
        );
    }

    #[test]
    fn get_with_iuse_default_overridden_by_config() {
        let mut config = UseConfig::new();
        config.disable(flag("ssl"));
        assert_eq!(
            config.get_with_iuse_default(flag("ssl"), Some(IUseDefault::Enabled)),
            UseFlagState::Disabled
        );
    }

    #[test]
    fn fold_iuse_defaults_only_missing() {
        let mut config = UseConfig::new();
        config.disable(flag("a")); // explicit — must survive
        let mut defaults = HashMap::new();
        defaults.insert(flag("a"), IUseDefault::Enabled);
        defaults.insert(flag("b"), IUseDefault::Enabled);
        config.fold_iuse_defaults(&defaults);
        // 'a' stays disabled (explicit), 'b' picks up its enabled default.
        assert_eq!(config.get(flag("a")), UseFlagState::Disabled);
        assert_eq!(config.get(flag("b")), UseFlagState::Enabled);
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

    #[test]
    fn apply_package_use_borrowed_when_empty() {
        let base = UseConfig::new();
        let cpv = Cpv::parse("dev-libs/openssl-3.0.0").unwrap();
        let out = apply_package_use(&base, &cpv, None, &[]);
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn apply_package_use_applies_matching_atom() {
        let base = UseConfig::new();
        let cpv = Cpv::parse("dev-libs/openssl-3.0.0").unwrap();
        let dep = Dep::parse("dev-libs/openssl").unwrap();
        let out = apply_package_use(
            &base,
            &cpv,
            None,
            &[(
                dep,
                vec![UseOverride::parse("ssl"), UseOverride::parse("-debug")],
            )],
        );
        let owned = match out {
            Cow::Owned(c) => c,
            _ => panic!("expected owned"),
        };
        assert_eq!(owned.get(flag("ssl")), UseFlagState::Enabled);
        assert_eq!(owned.get(flag("debug")), UseFlagState::Disabled);
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
