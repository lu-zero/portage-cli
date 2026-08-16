//! Root-target atoms: where each one came from, and what to do when no
//! acceptable ebuild satisfies it.
//!
//! Portage decides fatal-vs-soft for an unsatisfiable argument in argument
//! processing (`depgraph._resolve`), keyed on the *argument type* — and, for a
//! set, on the literal set name: only `selected`/`system`/`world` get the
//! friendly "carry on and warn" treatment. A user-defined set member takes the
//! same fatal path as an atom typed on the command line.

use std::collections::HashMap;

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, Dep, Version};

/// Where a root-target atom came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetOrigin {
    /// Named directly on the command line.
    Explicit,
    /// Expanded from `@<name>`.
    Set(String),
}

impl TargetOrigin {
    /// The `@selected`/`@system`/`@world` family, which portage treats as a
    /// standing selection rather than a request: an unsatisfiable member warns
    /// and the run continues.
    ///
    /// Portage splits this three ways (`@system` gets no missing-args warning,
    /// and stays fatal when its ebuild left the tree); em collapses it to one
    /// rule — the world-family cases are the ones users actually hit.
    pub fn is_world_family(&self) -> bool {
        matches!(self, Self::Set(name) if matches!(name.as_str(), "selected" | "system" | "world"))
    }

    /// How to name this origin when heading a group of targets that share it.
    pub fn label(&self) -> String {
        match self {
            Self::Explicit => "requested".to_string(),
            Self::Set(name) => format!("@{name}"),
        }
    }

    /// Portage's `(dependency required by …)` trailer under an unsatisfied
    /// argument. An atom typed on the command line is its own argument and
    /// gets none; a set names itself, so the user can tell where the atom they
    /// never typed came from.
    ///
    /// Portage prints one line per level of set nesting (`"@selected" [set]`
    /// then `"@world" [argument]`); `expand_sets` keeps only the name the user
    /// actually asked for, so there is exactly one line.
    pub fn trailer(&self) -> Option<String> {
        match self {
            Self::Explicit => None,
            Self::Set(name) => Some(format!("(dependency required by \"@{name}\" [argument])")),
        }
    }
}

/// One resolved root target plus its provenance.
#[derive(Debug, Clone)]
pub struct TargetAtom {
    /// The atom text handed to the resolver.
    pub atom: String,
    /// Where it came from.
    pub origin: TargetOrigin,
}

impl TargetAtom {
    /// A target named on the command line.
    pub fn explicit(atom: impl Into<String>) -> Self {
        Self {
            atom: atom.into(),
            origin: TargetOrigin::Explicit,
        }
    }
}

/// Why a root target has no acceptable candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetProblem {
    /// The tree has versions, but keyword/mask/license filtering rejects every
    /// one matching the atom.
    AllFiltered,
    /// No ebuild for this package at all.
    NoEbuilds,
}

/// What to do with a root target no acceptable ebuild satisfies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootTargetDecision {
    /// Drop the atom, report it, and keep resolving (exit 0).
    DropWithWarning,
    /// Drop the atom without a word: an installed instance satisfies it and the
    /// resolve is selective, so there is nothing to do and nothing wrong.
    DropSilently,
    /// Abort the resolve.
    Fatal,
}

/// Mirror of `depgraph._resolve`'s fatal-vs-soft decision for an argument with
/// no acceptable candidate.
///
/// `installed_satisfies` must already account for `--emptytree`, which makes
/// portage ignore installed instances (`depgraph.py:7691`).
pub fn classify_root_target(
    origin: &TargetOrigin,
    installed_satisfies: bool,
    selective: bool,
) -> RootTargetDecision {
    if origin.is_world_family() {
        RootTargetDecision::DropWithWarning
    } else if selective && installed_satisfies {
        RootTargetDecision::DropSilently
    } else {
        RootTargetDecision::Fatal
    }
}

/// Whether an installed instance satisfies `dep`, including its slot and
/// version qualifiers.
pub fn installed_satisfies(
    dep: &Dep,
    installed: &HashMap<Cpn, HashMap<Interned<DefaultInterner>, Version>>,
) -> bool {
    installed.get(&dep.cpn).is_some_and(|slots| {
        slots.iter().any(|(slot, version)| {
            let slot = (!slot.as_str().is_empty()).then(|| portage_atom::Slot::from_name(*slot));
            dep.matches_cpv(&Cpv::new(dep.cpn, version.clone()), slot.as_ref())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(name: &str) -> TargetOrigin {
        TargetOrigin::Set(name.to_string())
    }

    /// Every row was measured against emerge 3.13.
    #[test]
    fn fatal_vs_soft_matches_portage() {
        use RootTargetDecision::*;
        // A world-family member is soft however the run was invoked.
        for name in ["world", "selected", "system"] {
            for installed in [false, true] {
                for selective in [false, true] {
                    assert_eq!(
                        classify_root_target(&set(name), installed, selective),
                        DropWithWarning,
                        "@{name} must not abort the resolve"
                    );
                }
            }
        }
        // An explicit atom, and a user-defined set's member, are spared only
        // when an installed instance satisfies them and the run is selective.
        for origin in [TargetOrigin::Explicit, set("mytest")] {
            assert_eq!(classify_root_target(&origin, true, true), DropSilently);
            assert_eq!(classify_root_target(&origin, true, false), Fatal);
            assert_eq!(classify_root_target(&origin, false, true), Fatal);
            assert_eq!(classify_root_target(&origin, false, false), Fatal);
        }
    }

    #[test]
    fn user_set_is_not_world_family() {
        assert!(set("world").is_world_family());
        assert!(!set("profile").is_world_family());
        assert!(!TargetOrigin::Explicit.is_world_family());
    }

    #[test]
    fn only_a_set_gets_a_trailer() {
        assert_eq!(
            set("world").trailer().as_deref(),
            Some("(dependency required by \"@world\" [argument])")
        );
        assert_eq!(TargetOrigin::Explicit.trailer(), None);
    }

    fn installed_map(
        entries: &[(&str, &str, &str)],
    ) -> HashMap<Cpn, HashMap<Interned<DefaultInterner>, Version>> {
        let mut map: HashMap<Cpn, HashMap<Interned<DefaultInterner>, Version>> = HashMap::new();
        for (cpn, slot, version) in entries {
            let cpn = Cpn::try_new(cpn).expect("test cpn parses");
            let version: Version = version.parse().expect("test version parses");
            map.entry(cpn)
                .or_default()
                .insert(Interned::intern(slot), version);
        }
        map
    }

    #[test]
    fn installed_satisfaction_honours_slot_and_version() {
        let map = installed_map(&[("app-misc/asciinema", "0", "3.2.0")]);
        let dep = |s: &str| Dep::parse(s).expect("test atom parses");
        assert!(installed_satisfies(&dep("app-misc/asciinema"), &map));
        assert!(installed_satisfies(&dep("app-misc/asciinema:0"), &map));
        assert!(!installed_satisfies(&dep("app-misc/asciinema:1"), &map));
        assert!(!installed_satisfies(&dep(">=app-misc/asciinema-4"), &map));
        assert!(!installed_satisfies(&dep("app-misc/other"), &map));
    }
}
