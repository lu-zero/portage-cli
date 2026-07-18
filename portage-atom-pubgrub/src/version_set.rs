use std::fmt;

use portage_atom::{Operator, Revision, Suffix, SuffixKind, Version};
use pubgrub::VersionSet;
use version_ranges::Ranges;

/// A PubGrub `VersionSet` backed by PMS version ranges.
///
/// Maps PMS dependency operators (`>=`, `~`, `=*`, etc.) to `Ranges<Version>`,
/// enabling set algebra (intersection, complement, union) on version constraints.
///
/// See [PMS 8.3.1](https://projects.gentoo.org/pms/9/pms.html#version_specs).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct PortageVersionSet(Ranges<Version>);

impl PortageVersionSet {
    /// Create a version set matching all versions.
    pub fn any() -> Self {
        Self(Ranges::full())
    }

    /// Returns `true` if this set matches all versions.
    pub fn is_full(&self) -> bool {
        self.0 == Ranges::full()
    }

    /// Returns the inner ranges.
    pub fn ranges(&self) -> &Ranges<Version> {
        &self.0
    }

    /// Returns `true` if the given version is in this set.
    pub fn contains(&self, v: &Version) -> bool {
        use pubgrub::VersionSet;
        <Self as VersionSet>::contains(self, v)
    }

    /// Convert a PMS operator, glob flag, and version to a version set.
    ///
    /// See [PMS 8.3.1](https://projects.gentoo.org/pms/9/pms.html#version_specs).
    pub fn from_operator(op: Operator, glob: bool, v: Version) -> Self {
        match op {
            Operator::GreaterOrEqual => Self(Ranges::higher_than(v)),
            Operator::Greater => Self(Ranges::strictly_higher_than(v)),
            Operator::LessOrEqual => Self(Ranges::lower_than(v)),
            Operator::Less => Self(Ranges::strictly_lower_than(v)),
            Operator::Equal if glob => {
                let next = next_after_glob(&v);
                Self(Ranges::between(v, next))
            }
            Operator::Equal => Self(Ranges::singleton(v)),
            Operator::Approximate => {
                let (base, bumped) = approximate_bounds(&v);
                Self(Ranges::between(base, bumped))
            }
        }
    }
}

impl VersionSet for PortageVersionSet {
    type V = Version;

    fn empty() -> Self {
        Self(Ranges::empty())
    }

    fn singleton(v: Self::V) -> Self {
        Self(Ranges::singleton(v))
    }

    fn complement(&self) -> Self {
        Self(self.0.complement())
    }

    fn intersection(&self, other: &Self) -> Self {
        Self(self.0.intersection(&other.0))
    }

    fn contains(&self, v: &Self::V) -> bool {
        self.0.contains(v)
    }

    fn full() -> Self {
        Self(Ranges::full())
    }

    fn union(&self, other: &Self) -> Self {
        Self(self.0.union(&other.0))
    }

    fn is_disjoint(&self, other: &Self) -> bool {
        self.0.is_disjoint(&other.0)
    }

    fn subset_of(&self, other: &Self) -> bool {
        self.0.subset_of(&other.0)
    }
}

impl fmt::Display for PortageVersionSet {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Compute the lower and upper bounds for the `~` (approximate) operator.
///
/// PMS 8.3.1: "~ V" matches versions identical to V when revision is ignored.
///
/// PMS version ordering (Algorithm 3.1): numeric → letter → suffixes → revision.
/// Suffix ordering: _alpha < _beta < _pre < _rc < (none) < _p.
///
/// So `_p` sorts after all revisions of the base version:
/// `1.2.3 < 1.2.3-r1 < ... < 1.2.3_p < 1.2.3_p-r1 < ... < 1.2.4`
///
/// The range `[V_rev0, V_p)` captures exactly the base version and all its
/// revisions, excluding `_p` patchlevels and other suffix variants.
///
/// For suffixed versions the same trick works by appending `_p`:
/// `~1.2.3_alpha1` → `[1.2.3_alpha1, 1.2.3_alpha1_p)` — the appended `_p`
/// sorts after all revisions of `1.2.3_alpha1` but before any version whose
/// suffixes differ from `_alpha1`.
fn approximate_bounds(v: &Version) -> (Version, Version) {
    let mut base = v.clone();
    base.revision = Revision::default();

    let mut upper = base.clone();
    upper.revision = Revision::default();
    upper.suffixes.push(Suffix {
        kind: SuffixKind::P,
        version: None,
    });

    (base, upper)
}

/// Compute the first version NOT matched by a glob pattern.
///
/// PMS 8.3.1: "=V*" compares only the version components present before `*`.
/// The exclusive upper bound is computed by bumping the last specified numeric
/// component.
fn next_after_glob(v: &Version) -> Version {
    let n = v.numbers.len();
    let mut next_numbers = v.numbers.clone();

    if n > 0 {
        next_numbers[n - 1] += 1;
    }

    // Rebuild digits from the bumped integers so PMS compare stays consistent.
    Version::new(&next_numbers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greater_or_equal() {
        let v = Version::parse("1.2.3").unwrap();
        let vs = PortageVersionSet::from_operator(Operator::GreaterOrEqual, false, v.clone());
        assert!(vs.contains(&Version::parse("1.2.3").unwrap()));
        assert!(vs.contains(&Version::parse("2.0.0").unwrap()));
        assert!(!vs.contains(&Version::parse("1.2.2").unwrap()));
    }

    #[test]
    fn less() {
        let v = Version::parse("1.2.3").unwrap();
        let vs = PortageVersionSet::from_operator(Operator::Less, false, v);
        assert!(!vs.contains(&Version::parse("1.2.3").unwrap()));
        assert!(vs.contains(&Version::parse("1.2.2").unwrap()));
        assert!(vs.contains(&Version::parse("0.9.9").unwrap()));
    }

    #[test]
    fn exact() {
        let v = Version::parse("1.2.3").unwrap();
        let vs = PortageVersionSet::from_operator(Operator::Equal, false, v.clone());
        assert!(vs.contains(&v));
        assert!(!vs.contains(&Version::parse("1.2.4").unwrap()));
        assert!(!vs.contains(&Version::parse("1.2.2").unwrap()));
    }

    #[test]
    fn glob() {
        let v = Version::parse("1.2").unwrap();
        let vs = PortageVersionSet::from_operator(Operator::Equal, true, v);
        assert!(vs.contains(&Version::parse("1.2.0").unwrap()));
        assert!(vs.contains(&Version::parse("1.2.3").unwrap()));
        assert!(vs.contains(&Version::parse("1.2.3-r1").unwrap()));
        assert!(!vs.contains(&Version::parse("1.3.0").unwrap()));
        assert!(!vs.contains(&Version::parse("1.1.9").unwrap()));
    }

    #[test]
    fn approximate() {
        let v = Version::parse("1.2.3-r5").unwrap();
        let vs = PortageVersionSet::from_operator(Operator::Approximate, false, v);
        assert!(vs.contains(&Version::parse("1.2.3").unwrap()));
        assert!(vs.contains(&Version::parse("1.2.3-r0").unwrap()));
        assert!(vs.contains(&Version::parse("1.2.3-r5").unwrap()));
        assert!(!vs.contains(&Version::parse("1.2.4").unwrap()));
        assert!(!vs.contains(&Version::parse("1.2.2").unwrap()));
        assert!(!vs.contains(&Version::parse("1.2.3_p").unwrap()));
        assert!(!vs.contains(&Version::parse("1.2.3_p1").unwrap()));
        assert!(!vs.contains(&Version::parse("1.2.3_alpha").unwrap()));
    }

    #[test]
    fn approximate_suffixed() {
        let v = Version::parse("1.2.3_alpha1").unwrap();
        let vs = PortageVersionSet::from_operator(Operator::Approximate, false, v);
        assert!(vs.contains(&Version::parse("1.2.3_alpha1").unwrap()));
        assert!(vs.contains(&Version::parse("1.2.3_alpha1-r0").unwrap()));
        assert!(vs.contains(&Version::parse("1.2.3_alpha1-r5").unwrap()));
        assert!(!vs.contains(&Version::parse("1.2.3").unwrap()));
        assert!(!vs.contains(&Version::parse("1.2.3-r1").unwrap()));
        assert!(!vs.contains(&Version::parse("1.2.3_alpha2").unwrap()));
        assert!(!vs.contains(&Version::parse("1.2.3_beta1").unwrap()));
        assert!(!vs.contains(&Version::parse("1.2.3_p1").unwrap()));
        assert!(!vs.contains(&Version::parse("1.2.4").unwrap()));
    }

    #[test]
    fn approximate_multi_suffix() {
        let v = Version::parse("1.2.3_pre1_p2").unwrap();
        let vs = PortageVersionSet::from_operator(Operator::Approximate, false, v);
        assert!(vs.contains(&Version::parse("1.2.3_pre1_p2").unwrap()));
        assert!(vs.contains(&Version::parse("1.2.3_pre1_p2-r1").unwrap()));
        assert!(!vs.contains(&Version::parse("1.2.3_pre1_p3").unwrap()));
        assert!(!vs.contains(&Version::parse("1.2.3").unwrap()));
        assert!(!vs.contains(&Version::parse("1.2.4").unwrap()));
    }

    #[test]
    fn intersection() {
        let v1 = PortageVersionSet::from_operator(
            Operator::GreaterOrEqual,
            false,
            Version::parse("1.0").unwrap(),
        );
        let v2 =
            PortageVersionSet::from_operator(Operator::Less, false, Version::parse("2.0").unwrap());
        let inter = v1.intersection(&v2);
        assert!(inter.contains(&Version::parse("1.5").unwrap()));
        assert!(!inter.contains(&Version::parse("0.9").unwrap()));
        assert!(!inter.contains(&Version::parse("2.0").unwrap()));
    }

    #[test]
    fn complement() {
        let vs = PortageVersionSet::from_operator(
            Operator::GreaterOrEqual,
            false,
            Version::parse("2.0").unwrap(),
        );
        let comp = vs.complement();
        assert!(comp.contains(&Version::parse("1.9").unwrap()));
        assert!(!comp.contains(&Version::parse("2.0").unwrap()));
    }

    #[test]
    fn strictly_greater() {
        let vs = PortageVersionSet::from_operator(
            Operator::Greater,
            false,
            Version::parse("3.0.0").unwrap(),
        );
        assert!(!vs.contains(&Version::parse("3.0.0").unwrap()));
        assert!(vs.contains(&Version::parse("3.0.1").unwrap()));
        assert!(!vs.contains(&Version::parse("2.9.9").unwrap()));
    }

    #[test]
    fn less_or_equal() {
        let vs = PortageVersionSet::from_operator(
            Operator::LessOrEqual,
            false,
            Version::parse("2.0.0").unwrap(),
        );
        assert!(vs.contains(&Version::parse("2.0.0").unwrap()));
        assert!(vs.contains(&Version::parse("1.9.9").unwrap()));
        assert!(!vs.contains(&Version::parse("2.0.1").unwrap()));
    }

    #[test]
    fn union_covers_both_ranges() {
        let a = PortageVersionSet::from_operator(
            Operator::GreaterOrEqual,
            false,
            Version::parse("2.0").unwrap(),
        );
        let b = PortageVersionSet::from_operator(
            Operator::LessOrEqual,
            false,
            Version::parse("1.0").unwrap(),
        );
        let u = a.union(&b);
        assert!(u.contains(&Version::parse("0.5").unwrap()));
        assert!(u.contains(&Version::parse("2.5").unwrap()));
        assert!(!u.contains(&Version::parse("1.5").unwrap()));
    }

    #[test]
    fn subset_of_wider_range() {
        let narrow = PortageVersionSet::from_operator(
            Operator::GreaterOrEqual,
            false,
            Version::parse("2.0").unwrap(),
        );
        let wide = PortageVersionSet::from_operator(
            Operator::GreaterOrEqual,
            false,
            Version::parse("1.0").unwrap(),
        );
        assert!(narrow.subset_of(&wide));
        assert!(!wide.subset_of(&narrow));
    }

    #[test]
    fn is_disjoint_non_overlapping() {
        let a = PortageVersionSet::from_operator(
            Operator::GreaterOrEqual,
            false,
            Version::parse("2.0").unwrap(),
        );
        let b =
            PortageVersionSet::from_operator(Operator::Less, false, Version::parse("1.0").unwrap());
        assert!(a.is_disjoint(&b));
    }

    #[test]
    fn is_disjoint_overlapping() {
        let a = PortageVersionSet::from_operator(
            Operator::GreaterOrEqual,
            false,
            Version::parse("1.0").unwrap(),
        );
        let b = PortageVersionSet::from_operator(
            Operator::LessOrEqual,
            false,
            Version::parse("2.0").unwrap(),
        );
        assert!(!a.is_disjoint(&b));
    }

    #[test]
    fn any_is_full() {
        let vs = PortageVersionSet::any();
        assert!(vs.is_full());
    }

    #[test]
    fn constrained_is_not_full() {
        let vs = PortageVersionSet::from_operator(
            Operator::GreaterOrEqual,
            false,
            Version::parse("1.0").unwrap(),
        );
        assert!(!vs.is_full());
    }

    #[test]
    fn display_does_not_panic() {
        let vs = PortageVersionSet::from_operator(
            Operator::GreaterOrEqual,
            false,
            Version::parse("1.0").unwrap(),
        );
        let s = vs.to_string();
        assert!(!s.is_empty());
    }
}
