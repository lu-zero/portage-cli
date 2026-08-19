//! PMS version matching operators.
//!
//! Implements [PMS 8.3.2](https://projects.gentoo.org/pms/9/pms.html#block-operator):
//! given a candidate version, an operator, and a constraint version, determine
//! whether the candidate satisfies the constraint. See [`version_matches`]
//! for the operator table and the glob (`=cat/pkg-1.2*`) special case.

use portage_atom::{Operator, Version};

/// Test whether `candidate` satisfies the version constraint `op constraint`
/// ([PMS 8.3.1](https://projects.gentoo.org/pms/9/pms.html#operators)):
///
/// | Operator | Meaning |
/// |----------|---------|
/// | `<` / `<=` | strictly less / less-or-equal than constraint |
/// | `=` | exactly equal to constraint (including revision) |
/// | `>=` / `>` | greater-or-equal / strictly greater than constraint |
/// | `~` | same base version as constraint, ignoring revision |
///
/// When `constraint.glob` is `true` (the `=cat/pkg-1.2*` form), `=` performs
/// prefix matching via [`Version::glob_matches`] instead of equality.
pub fn version_matches(
    candidate: &Version,
    op: &Operator,
    glob: bool,
    constraint: &Version,
) -> bool {
    match op {
        Operator::Less => candidate < constraint,
        Operator::LessOrEqual => candidate <= constraint,
        Operator::Equal => {
            if glob {
                candidate.glob_matches(constraint)
            } else {
                candidate == constraint
            }
        }
        Operator::GreaterOrEqual => candidate >= constraint,
        Operator::Greater => candidate > constraint,
        Operator::Approximate => candidate.base() == constraint.base(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    // --- Less ---
    #[test]
    fn less_matches() {
        assert!(version_matches(
            &v("1.2.3"),
            &Operator::Less,
            false,
            &v("1.2.4")
        ));
    }

    #[test]
    fn less_equal_does_not_match() {
        assert!(!version_matches(
            &v("1.2.3"),
            &Operator::Less,
            false,
            &v("1.2.3")
        ));
    }

    #[test]
    fn less_greater_does_not_match() {
        assert!(!version_matches(
            &v("1.2.4"),
            &Operator::Less,
            false,
            &v("1.2.3")
        ));
    }

    // --- LessOrEqual ---
    #[test]
    fn less_or_equal_matches_equal() {
        assert!(version_matches(
            &v("1.2.3"),
            &Operator::LessOrEqual,
            false,
            &v("1.2.3")
        ));
    }

    #[test]
    fn less_or_equal_matches_less() {
        assert!(version_matches(
            &v("1.2.2"),
            &Operator::LessOrEqual,
            false,
            &v("1.2.3")
        ));
    }

    #[test]
    fn less_or_equal_does_not_match_greater() {
        assert!(!version_matches(
            &v("1.2.4"),
            &Operator::LessOrEqual,
            false,
            &v("1.2.3")
        ));
    }

    // --- Equal ---
    #[test]
    fn equal_matches() {
        assert!(version_matches(
            &v("1.2.3"),
            &Operator::Equal,
            false,
            &v("1.2.3")
        ));
    }

    #[test]
    fn equal_includes_revision() {
        assert!(!version_matches(
            &v("1.2.3-r1"),
            &Operator::Equal,
            false,
            &v("1.2.3")
        ));
    }

    #[test]
    fn equal_revisions_match() {
        assert!(version_matches(
            &v("1.2.3-r1"),
            &Operator::Equal,
            false,
            &v("1.2.3-r1")
        ));
    }

    // --- GreaterOrEqual ---
    #[test]
    fn greater_or_equal_matches_equal() {
        assert!(version_matches(
            &v("1.2.3"),
            &Operator::GreaterOrEqual,
            false,
            &v("1.2.3")
        ));
    }

    #[test]
    fn greater_or_equal_matches_greater() {
        assert!(version_matches(
            &v("1.2.4"),
            &Operator::GreaterOrEqual,
            false,
            &v("1.2.3")
        ));
    }

    #[test]
    fn greater_or_equal_does_not_match_less() {
        assert!(!version_matches(
            &v("1.2.2"),
            &Operator::GreaterOrEqual,
            false,
            &v("1.2.3")
        ));
    }

    // --- Greater ---
    #[test]
    fn greater_matches() {
        assert!(version_matches(
            &v("1.2.4"),
            &Operator::Greater,
            false,
            &v("1.2.3")
        ));
    }

    #[test]
    fn greater_equal_does_not_match() {
        assert!(!version_matches(
            &v("1.2.3"),
            &Operator::Greater,
            false,
            &v("1.2.3")
        ));
    }

    // --- Approximate (~) ---
    #[test]
    fn approximate_ignores_revision() {
        assert!(version_matches(
            &v("1.2.3-r1"),
            &Operator::Approximate,
            false,
            &v("1.2.3")
        ));
    }

    #[test]
    fn approximate_matches_same_base() {
        assert!(version_matches(
            &v("1.2.3"),
            &Operator::Approximate,
            false,
            &v("1.2.3-r2")
        ));
    }

    #[test]
    fn approximate_different_base() {
        assert!(!version_matches(
            &v("1.2.4"),
            &Operator::Approximate,
            false,
            &v("1.2.3")
        ));
    }

    // --- Equal with glob (=*) ---

    #[test]
    fn glob_matches_prefix() {
        assert!(version_matches(
            &v("1.75.0"),
            &Operator::Equal,
            true,
            &v("1.75")
        ));
    }

    #[test]
    fn glob_matches_exact() {
        assert!(version_matches(
            &v("1.75"),
            &Operator::Equal,
            true,
            &v("1.75")
        ));
    }

    #[test]
    fn glob_does_not_match_shorter() {
        assert!(!version_matches(
            &v("1.7"),
            &Operator::Equal,
            true,
            &v("1.75")
        ));
    }

    #[test]
    fn glob_does_not_match_different() {
        assert!(!version_matches(
            &v("1.76.0"),
            &Operator::Equal,
            true,
            &v("1.75")
        ));
    }

    #[test]
    fn glob_with_letter() {
        assert!(version_matches(
            &v("1.2.3a"),
            &Operator::Equal,
            true,
            &v("1.2.3a")
        ));
        assert!(!version_matches(
            &v("1.2.3b"),
            &Operator::Equal,
            true,
            &v("1.2.3a")
        ));
    }

    #[test]
    fn glob_without_letter_matches_any_letter() {
        assert!(version_matches(
            &v("1.2.3a"),
            &Operator::Equal,
            true,
            &v("1.2.3")
        ));
    }

    // --- Suffix edge cases ---
    #[test]
    fn less_with_suffix() {
        assert!(version_matches(
            &v("1.2.3_rc1"),
            &Operator::Less,
            false,
            &v("1.2.3")
        ));
    }

    #[test]
    fn greater_with_patchlevel() {
        assert!(version_matches(
            &v("1.2.3_p1"),
            &Operator::Greater,
            false,
            &v("1.2.3")
        ));
    }
}
