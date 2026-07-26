//! Root-target atoms: where each one came from, and what to do when no
//! acceptable ebuild satisfies it.
//!
//! Portage decides fatal-vs-soft for an unsatisfiable argument in argument
//! processing (`depgraph._resolve`), keyed on the *argument type* — and, for a
//! set, on the literal set name: only `selected`/`system`/`world` get the
//! friendly "carry on and warn" treatment. A user-defined set member takes the
//! same fatal path as an atom typed on the command line.

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
    /// Abort the resolve.
    Fatal,
}

/// Mirror of `depgraph._resolve`'s fatal-vs-soft decision for an argument with
/// no acceptable candidate.
pub fn classify_root_target(origin: &TargetOrigin) -> RootTargetDecision {
    if origin.is_world_family() {
        RootTargetDecision::DropWithWarning
    } else {
        RootTargetDecision::Fatal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(name: &str) -> TargetOrigin {
        TargetOrigin::Set(name.to_string())
    }

    #[test]
    fn world_family_is_soft_everything_else_fatal() {
        for name in ["world", "selected", "system"] {
            assert_eq!(
                classify_root_target(&set(name)),
                RootTargetDecision::DropWithWarning,
                "@{name} must not abort the resolve"
            );
        }
        assert_eq!(
            classify_root_target(&set("mytest")),
            RootTargetDecision::Fatal
        );
        assert_eq!(
            classify_root_target(&TargetOrigin::Explicit),
            RootTargetDecision::Fatal
        );
    }

    #[test]
    fn user_set_is_not_world_family() {
        assert!(set("world").is_world_family());
        assert!(!set("profile").is_world_family());
        assert!(!TargetOrigin::Explicit.is_world_family());
    }
}
