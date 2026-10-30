use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use smallvec::SmallVec;
use winnow::Parser;
use winnow::ascii::digit1;
use winnow::combinator::{alt, cut_err, fail, opt, preceded, repeat, separated};
use winnow::error::StrContext;
use winnow::prelude::*;
use winnow::token::one_of;

use crate::error::{Error, Result};

mod encoding;

use encoding::{Layout, Repr, render};

/// Package revision (`-r1`, `-r2`, etc.)
///
/// Tracks packaging changes independently of the upstream version.
/// A revision of `0` is the implicit default and is omitted from display.
///
/// See [PMS 3.2](https://projects.gentoo.org/pms/9/pms.html#version-specifications).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Revision(
    /// The revision number (e.g. `1` for `-r1`, `2` for `-r2`)
    ///
    /// `0` means no revision (omitted from display).
    pub u64,
);

impl fmt::Display for Revision {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.0 == 0 {
            Ok(())
        } else {
            write!(f, "-r{}", self.0)
        }
    }
}

impl PartialOrd for Revision {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Revision {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0)
    }
}

/// Version suffix kind
///
/// PMS defines five ordered suffix types that modify version comparison.
/// `Alpha`, `Beta`, `Pre`, and `Rc` sort *below* the unsuffixed version,
/// while `P` (patchlevel) sorts *above* it.
///
/// See [PMS 3.2](https://projects.gentoo.org/pms/9/pms.html#version-specifications)
/// and [Algorithm 3.1](https://projects.gentoo.org/pms/9/pms.html#version-comparison).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SuffixKind {
    /// `_alpha` — earliest pre-release stage
    Alpha,
    /// `_beta` — feature-complete but not yet stable
    Beta,
    /// `_pre` — pre-release snapshot
    Pre,
    /// `_rc` — release candidate
    Rc,
    /// `_p` — post-release patchlevel (sorts *above* the base version)
    P,
}

impl SuffixKind {
    /// Ordering value for PMS version comparison
    fn order(&self) -> i32 {
        match self {
            SuffixKind::Alpha => -4,
            SuffixKind::Beta => -3,
            SuffixKind::Pre => -2,
            SuffixKind::Rc => -1,
            SuffixKind::P => 1,
        }
    }
}

impl fmt::Display for SuffixKind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            SuffixKind::Alpha => write!(f, "_alpha"),
            SuffixKind::Beta => write!(f, "_beta"),
            SuffixKind::Pre => write!(f, "_pre"),
            SuffixKind::Rc => write!(f, "_rc"),
            SuffixKind::P => write!(f, "_p"),
        }
    }
}

impl FromStr for SuffixKind {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "alpha" => Ok(SuffixKind::Alpha),
            "beta" => Ok(SuffixKind::Beta),
            "pre" => Ok(SuffixKind::Pre),
            "rc" => Ok(SuffixKind::Rc),
            "p" => Ok(SuffixKind::P),
            _ => Err(Error::InvalidVersion(format!("invalid suffix kind: {}", s))),
        }
    }
}

/// A version suffix with an optional numeric qualifier
///
/// Represents a single `_alpha`, `_beta`, `_pre`, `_rc`, or `_p` segment,
/// optionally followed by a number (e.g. `_rc2`, `_p1`).
///
/// See [PMS 3.2](https://projects.gentoo.org/pms/9/pms.html#version-specifications)
/// and [Algorithm 3.1](https://projects.gentoo.org/pms/9/pms.html#version-comparison)
/// for the ordering rules.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "builder", derive(bon::Builder))]
pub struct Suffix {
    /// The suffix kind (`_alpha`, `_beta`, `_pre`, `_rc`, or `_p`)
    pub kind: SuffixKind,
    /// Optional numeric qualifier (e.g. `2` in `_rc2`, absent in `_rc`)
    ///
    /// When absent, the implicit value is `0` (PMS Algorithm 3.6).
    pub version: Option<u64>,
}

impl fmt::Display for Suffix {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.kind)?;
        if let Some(v) = self.version {
            write!(f, "{}", v)?;
        }
        Ok(())
    }
}

impl PartialOrd for Suffix {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Suffix {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind && self.version.unwrap_or(0) == other.version.unwrap_or(0)
    }
}

impl Eq for Suffix {}

impl Hash for Suffix {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.kind.hash(state);
        // PMS Algorithm 3.6: absent numeric part is implicit 0.
        self.version.unwrap_or(0).hash(state);
    }
}

impl Ord for Suffix {
    fn cmp(&self, other: &Self) -> Ordering {
        // PMS: Compare suffix kind first
        match self.kind.order().cmp(&other.kind.order()) {
            Ordering::Equal => {
                // Algorithm 3.6: integer part if any, otherwise 0.
                self.version.unwrap_or(0).cmp(&other.version.unwrap_or(0))
            }
            other => other,
        }
    }
}

/// Version comparison operator for dependency atoms
///
/// Used as a prefix on versioned dependencies to constrain which versions
/// satisfy the dependency.
///
/// See [PMS 8.3.1](https://projects.gentoo.org/pms/9/pms.html#operators).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Operator {
    /// `<` — strictly less than the specified version
    Less,
    /// `<=` — less than or equal to the specified version
    LessOrEqual,
    /// `=` — exactly the specified version (including revision)
    ///
    /// When used with a version ending in `*`, performs prefix matching
    /// per PMS 8.3.1 (e.g., `=pkg-1.2*` matches `1.2.3`, `1.2.4`, etc.).
    Equal,
    /// `~` — matches the same base version, ignoring the revision
    /// (e.g. `~dev-lang/rust-1.75.0` matches `-r0`, `-r1`, etc.)
    Approximate,
    /// `>=` — greater than or equal to the specified version
    GreaterOrEqual,
    /// `>` — strictly greater than the specified version
    Greater,
}

impl fmt::Display for Operator {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Operator::Less => write!(f, "<"),
            Operator::LessOrEqual => write!(f, "<="),
            Operator::Equal => write!(f, "="),
            Operator::Approximate => write!(f, "~"),
            Operator::GreaterOrEqual => write!(f, ">="),
            Operator::Greater => write!(f, ">"),
        }
    }
}

/// Dot-separated numeric components
///
/// `SmallVec`-backed: real-world versions almost always have 4 or fewer
/// components, so the common case needs no heap allocation.
pub type Numbers = SmallVec<[u64; 4]>;

/// Package version according to PMS
///
/// Represents a version string such as `1.2.3a_alpha4_beta5_pre6_rc7_p8-r9`.
///
/// Ordering implements
/// [Algorithm 3.1](https://projects.gentoo.org/pms/9/pms.html#version-comparison):
/// numeric components are compared left-to-right, then the optional letter,
/// then suffixes (where `_p` sorts above the base while `_alpha`/`_beta`/`_pre`/`_rc`
/// sort below), and finally the revision.
///
/// See [PMS 3.2](https://projects.gentoo.org/pms/9/pms.html#version-specifications)
/// for the full version syntax.
///
/// A 24-byte value: a byte string whose byte order is the version order, so comparing,
/// hashing and cloning are cheap. Read it through the accessors; build or edit one as
/// [`VersionParts`]. It displays as written: `003.03` and `3.03` are equal yet print apart.
///
/// # Differences from semver
///
/// - **Variable component count** — `1`, `1.2`, `1.2.3.4` are all valid
///   (semver requires exactly `major.minor.patch`).
/// - **Letter suffix** — a single lowercase letter after the numbers (e.g.
///   `1.2.3a`); no semver equivalent.
///
/// - **Typed suffixes** — `_alpha`, `_beta`, `_pre`, `_rc`, `_p` with
///   defined ordering; semver uses free-form pre-release identifiers.
/// - **Revision** — a dedicated `-rN` component for distribution-level changes.
/// - **Ordering** — `_p` sorts *above* the base version; semver pre-releases
///   always sort below.
#[derive(Clone)]
pub struct Version(Repr);

/// The fields of a [`Version`], for building or editing one
///
/// See [PMS 3.2](https://projects.gentoo.org/pms/9/pms.html#version-specifications).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "builder", derive(bon::Builder))]
pub struct VersionParts {
    /// Dot-separated numeric components (e.g. `[1, 2, 3]` for `1.2.3`)
    ///
    /// At least one (PMS 3.2). Each must fit in a `u64`; a longer one is a parse error, not a truncation.
    #[cfg_attr(feature = "builder", builder(start_fn))]
    pub numbers: Numbers,
    /// Zeros written before each component's value (`[2, 1]` for `003.03`); empty when none are
    ///
    /// Algorithm 3.3 compares a later component written with one as a string (`1.01` is not `1.1`).
    #[cfg_attr(feature = "builder", builder(default))]
    pub leading_zeros: Vec<u32>,
    /// Optional single lowercase letter after the numeric components
    pub letter: Option<char>,
    /// Zero or more version suffixes (`_alpha`, `_beta`, `_pre`, `_rc`, `_p`)
    #[cfg_attr(feature = "builder", builder(default))]
    pub suffixes: Vec<Suffix>,
    /// Package revision; defaults to `0` (omitted from display)
    #[cfg_attr(feature = "builder", builder(default))]
    pub revision: Revision,
}

impl From<VersionParts> for Version {
    fn from(parts: VersionParts) -> Self {
        Version(Repr::encode(&parts, None))
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.0.order() == other.0.order()
    }
}

impl Eq for Version {}

impl Hash for Version {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.order().hash(state);
    }
}

impl Version {
    /// Create a version from its dot-separated numeric components
    ///
    /// `Version::new(&[1, 75, 0])` produces `1.75.0`. The letter, suffixes and
    /// revision are absent.
    ///
    /// # Panics
    ///
    /// Debug-asserts that `numbers` is non-empty (PMS 3.2 requires at least
    /// one numeric component).
    pub fn new(numbers: &[u64]) -> Self {
        debug_assert!(
            !numbers.is_empty(),
            "Version must have at least one numeric component per PMS 3.2"
        );
        VersionParts {
            numbers: Numbers::from_slice(numbers),
            leading_zeros: Vec::new(),
            letter: None,
            suffixes: Vec::new(),
            revision: Revision::default(),
        }
        .into()
    }

    fn layout(&self) -> Layout<'_> {
        Layout::of(self.0.order())
    }

    /// The fields of this version, including the zeros written before its components
    pub fn to_parts(&self) -> VersionParts {
        if let Some(parts) = self.0.spelling().and_then(|s| parse_parts.parse(s).ok()) {
            return parts;
        }
        self.layout().parts(self.0.order().len())
    }

    /// Number of dot-separated numeric components (`3` for `1.2.3`)
    pub fn component_count(&self) -> usize {
        self.layout().components.len()
    }

    /// Numeric component `i`, from the left (`component(1)` of `1.2.3` is `Some(2)`)
    pub fn component(&self, i: usize) -> Option<u64> {
        self.components().nth(i)
    }

    /// The numeric components, from the left
    pub fn components(&self) -> impl Iterator<Item = u64> + '_ {
        let numbers: Numbers = match self.0.spelling() {
            Some(_) => self.to_parts().numbers,
            None => {
                let layout = self.layout();
                layout
                    .components
                    .iter()
                    .enumerate()
                    .map(|(i, s)| Layout::component(s, i == 0).0)
                    .collect()
            }
        };
        numbers.into_iter()
    }

    /// The letter after the numeric components (`Some('a')` for `1.2a`)
    pub fn letter(&self) -> Option<char> {
        self.layout().tail(self.0.order().len()).0
    }

    /// The suffixes in order (`_alpha1`, then `_p2` for `1_alpha1_p2`)
    pub fn suffixes(&self) -> impl Iterator<Item = Suffix> + '_ {
        self.to_parts().suffixes.into_iter()
    }

    /// The revision; `0` when none was written
    pub fn revision(&self) -> Revision {
        self.layout().tail(self.0.order().len()).2
    }

    /// Parse a version string (without a leading operator)
    ///
    /// Accepts forms like `1.2.3`, `1.2.3a_rc1_p2-r5`.
    pub fn parse(input: &str) -> Result<Self> {
        parse_version
            .parse(input)
            .map_err(|e| Error::InvalidVersion(format!("{}: {}", input, e)))
    }

    /// Whether this version matches a glob pattern (PMS 8.3.1 `=V*`).
    ///
    /// The pattern's components must equal the candidate's in the same positions,
    /// compared as components, so `1.010.5` matches `1.01*`. Anything may follow
    /// the last one. A letter, suffix, or non-zero revision ends the numeric part,
    /// so `1.0.1-r1` is not a `1.0-r1*` match. A last suffix written without a number
    /// takes any number of its kind, as in Portage: `5.3_p12` is a `5.3_p*` match.
    pub fn glob_matches(&self, pattern: &Version) -> bool {
        let (cand, pat) = (self.layout(), pattern.layout());
        let (letter, suffixes, revision, _) = pat.tail(pattern.0.order().len());
        let has_rev = revision.0 != 0;
        let has_tail = letter.is_some() || !suffixes.is_empty() || has_rev;
        let n = pat.components.len();
        let cand_n = cand.components.len();
        if cand_n < n || (has_tail && cand_n != n) {
            return false;
        }
        if cand.components[..n] != pat.components[..] {
            return false;
        }
        let (cand_letter, cand_suffixes, cand_revision, _) = cand.tail(self.0.order().len());
        if has_tail && cand_letter != letter {
            return false;
        }
        if suffixes.len() > cand_suffixes.len()
            || (has_rev && suffixes.len() != cand_suffixes.len())
        {
            return false;
        }
        let open = !has_rev && pattern.last_suffix_unnumbered();
        let last = suffixes.len().saturating_sub(1);
        let differs = |(i, (c, p)): (usize, (&Suffix, &Suffix))| {
            if open && i == last {
                c.kind != p.kind
            } else {
                c != p
            }
        };
        if cand_suffixes.iter().zip(&suffixes).enumerate().any(differs) {
            return false;
        }
        !has_rev || cand_revision == revision
    }

    /// Whether the last suffix is written without a number (`_p`, not `_p0`)
    fn last_suffix_unnumbered(&self) -> bool {
        let (_, suffixes, _, _) = self.layout().tail(self.0.order().len());
        // A written `0` decodes like no number; only the spelling tells them apart.
        suffixes.last().is_some_and(|s| s.version.is_none())
            && self
                .to_parts()
                .suffixes
                .last()
                .is_some_and(|s| s.version.is_none())
    }

    /// Half-open bounds of `=V*`. A revision pattern is `[V, V-r(N+1))`. Otherwise
    /// lower adds one `_alpha` (a further suffix under it sorts lower), and upper
    /// bumps the last suffix number, else the letter, else the last numeric
    /// component, then adds `_alpha`. Written digits stay: `26.04*` ends at `26.05_alpha`.
    /// An unnumbered last suffix ends at the next kind instead: `1.0_rc*` ends at `1.0`.
    ///
    /// `None` means that bump does not fit in `u64`. See [`Self::glob_bounds_exact`]
    /// for when the range holds versions that are no match.
    pub fn glob_bounds(&self) -> (Version, Option<Version>) {
        let parts = self.to_parts();
        if parts.revision.0 != 0 {
            let upper = match parts.revision.0.checked_add(1) {
                Some(next) => {
                    let mut upper = parts.clone();
                    upper.revision = Revision(next);
                    Some(upper.into())
                }
                None => bumped_numeric(&parts),
            };
            return (parts.into(), upper);
        }
        let mut lower = parts.clone();
        lower.suffixes.push(alpha_suffix());

        let upper = if self.last_suffix_unnumbered() {
            bumped_past_unnumbered_suffix(&parts)
        } else if !parts.suffixes.is_empty() {
            bumped_suffix(&parts).or_else(|| bumped_numeric(&parts))
        } else if parts.letter.is_some() {
            Some(bumped_letter(&parts))
        } else {
            bumped_numeric(&parts)
        };
        (lower.into(), upper)
    }

    /// Whether [`Self::glob_bounds`] holds only [`Self::glob_matches`] matches,
    /// short of extra suffixes under the `_alpha` padding
    ///
    /// Not when a plain pattern's last numeric component (not the first) is `0`
    /// or starts with one: `26.041` sorts inside `=26.04*`, and `04` has no least
    /// successor. Nor when a suffix or revision bump overflows.
    pub fn glob_bounds_exact(&self) -> bool {
        let layout = self.layout();
        let (letter, suffixes, revision, _) = layout.tail(self.0.order().len());
        if revision.0 != 0 {
            return revision.0 != u64::MAX;
        }
        if let Some(last) = suffixes.last() {
            return last.version != Some(u64::MAX);
        }
        if letter.is_some() {
            return true;
        }
        let n = layout.components.len();
        n < 2 || layout.components[n - 1].first() != Some(&encoding::ZERO_LED)
    }

    /// Bounds of `~V`: `[V, V_p)` with any revision dropped, per
    /// [PMS 8.3.1](https://projects.gentoo.org/pms/9/pms.html#operators)
    ///
    /// `_p` sorts above every revision of `V` and below any other suffix change, so the range
    /// holds exactly `V` and its revisions (`~1.2.3_alpha1` ends at `1.2.3_alpha1_p`).
    pub fn approximate_bounds(&self) -> (Version, Version) {
        let mut parts = self.to_parts();
        parts.revision = Revision::default();
        let lower = Version::from(parts.clone());
        parts.suffixes.push(Suffix {
            kind: SuffixKind::P,
            version: None,
        });
        (lower, parts.into())
    }

    /// Whether this is a live (VCS/snapshot-tracking) version per Gentoo's
    /// `*9999` naming convention — not a PMS concept, but every repo treats
    /// the trailing `.9999` component as "track upstream HEAD"
    pub fn is_live(&self) -> bool {
        self.components().last() == Some(9999)
    }

    /// Return the version without its revision, for `~` (approximate)
    /// comparison per [PMS 8.3.1]
    ///
    /// [PMS 8.3.1]: https://projects.gentoo.org/pms/9/pms.html#operators
    pub fn base(&self) -> Self {
        let mut parts = self.to_parts();
        parts.revision = Revision::default();
        parts.into()
    }

    /// Whether this version satisfies a PMS dependency operator
    ///
    /// `glob` selects the `=cat/pkg-1.2*` prefix form and is ignored for
    /// operators other than [`Operator::Equal`]. See
    /// [PMS 8.3.1](https://projects.gentoo.org/pms/9/pms.html#operators).
    pub fn matches_operator(&self, op: Operator, glob: bool, constraint: &Self) -> bool {
        match op {
            Operator::Less => self < constraint,
            Operator::LessOrEqual => self <= constraint,
            Operator::Equal if glob => self.glob_matches(constraint),
            Operator::Equal => self == constraint,
            Operator::GreaterOrEqual => self >= constraint,
            Operator::Greater => self > constraint,
            Operator::Approximate => {
                self.order_without_revision() == constraint.order_without_revision()
            }
        }
    }

    // Algorithm 3.7 compares the revision last, so it is the key's last field.
    fn order_without_revision(&self) -> &[u8] {
        let order = self.0.order();
        let before_revision = self.layout().tail(order.len()).3;
        &order[..before_revision]
    }

    /// Version with suffixes and revision removed.
    ///
    /// Numeric components and the letter stay. `=V*` matching is
    /// [`Self::glob_matches`], including a pattern suffix prefix and a non-zero
    /// revision.
    pub fn without_suffix(&self) -> Self {
        let mut parts = self.to_parts();
        parts.suffixes.clear();
        parts.revision = Revision::default();
        parts.into()
    }

    /// Format the version as written
    pub(crate) fn fmt_version(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.0.spelling() {
            Some(s) => f.write_str(s),
            None => f.write_str(&render(&self.layout().parts(self.0.order().len()))),
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.fmt_version(f)
    }
}

impl fmt::Debug for Version {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Version({self})")
    }
}

fn alpha_suffix() -> Suffix {
    Suffix {
        kind: SuffixKind::Alpha,
        version: None,
    }
}

fn bumped_numeric(v: &VersionParts) -> Option<Version> {
    let mut upper = v.clone();
    upper.revision = Revision::default();
    upper.letter = None;
    upper.suffixes.clear();
    if !bump_last_numeric(&mut upper) {
        return None;
    }
    upper.suffixes.push(alpha_suffix());
    Some(upper.into())
}

// Above every `V_kN…` for an unnumbered last suffix `_k`: the next kind, or for `_rc` the
// version without it. `_p` is the highest kind, so after it comes what follows `V`'s own
// preceding suffix or letter.
fn bumped_past_unnumbered_suffix(v: &VersionParts) -> Option<Version> {
    let mut upper = v.clone();
    upper.revision = Revision::default();
    let next = match upper.suffixes.pop()?.kind {
        SuffixKind::Alpha => SuffixKind::Beta,
        SuffixKind::Beta => SuffixKind::Pre,
        SuffixKind::Pre => SuffixKind::Rc,
        SuffixKind::Rc => return Some(upper.into()),
        SuffixKind::P if !upper.suffixes.is_empty() => {
            return bumped_suffix(&upper).or_else(|| bumped_numeric(&upper));
        }
        SuffixKind::P => {
            return Some(match upper.letter {
                Some(_) => bumped_letter(&upper),
                None => {
                    upper.letter = Some('a');
                    upper.suffixes.push(alpha_suffix());
                    upper.into()
                }
            });
        }
    };
    upper.suffixes.push(Suffix {
        kind: next,
        version: None,
    });
    upper.suffixes.push(alpha_suffix());
    Some(upper.into())
}

fn bumped_suffix(v: &VersionParts) -> Option<Version> {
    let mut upper = v.clone();
    upper.revision = Revision::default();
    let last = upper.suffixes.last_mut()?;
    let next = last.version.unwrap_or(0).checked_add(1)?;
    last.version = Some(next);
    upper.suffixes.push(alpha_suffix());
    Some(upper.into())
}

// After `z` comes the same numbers plus `.0`, the smallest longer version.
fn bumped_letter(v: &VersionParts) -> Version {
    let mut upper = v.clone();
    upper.revision = Revision::default();
    match upper.letter {
        Some(c) if c < 'z' => upper.letter = Some((c as u8 + 1) as char),
        _ => {
            upper.letter = None;
            upper.numbers.push(0);
        }
    }
    upper.suffixes.push(alpha_suffix());
    upper.into()
}

fn decimal_len(n: u64) -> u32 {
    n.checked_ilog10().map_or(1, |l| l + 1)
}

// Width-preserving successor (`04` -> `05`, `009` -> `010`). Overflow drops the component:
// `2.0_alpha` still sorts above `2_alpha`, so the carry must not leave a trailing zero.
fn bump_last_numeric(v: &mut VersionParts) -> bool {
    while let Some(&current) = v.numbers.last() {
        let i = v.numbers.len() - 1;
        match current.checked_add(1) {
            Some(next) => {
                v.numbers[i] = next;
                if let Some(zeros) = v.leading_zeros.get_mut(i) {
                    let width = zeros.saturating_add(decimal_len(current));
                    *zeros = width.saturating_sub(decimal_len(next));
                }
                return true;
            }
            None => {
                v.numbers.pop();
                v.leading_zeros.truncate(i);
            }
        }
    }
    false
}

/// PMS version comparison (Algorithm 3.1)
impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.order().cmp(other.0.order())
    }
}

// Winnow parsers

/// A run of digits that does not fit in `u64` is a committed error.
///
/// Backtracking would let an operator-less atom keep the digits as part of
/// the package name, and a later component would report trailing input
/// instead of the overflow.
fn overflow_number<T>(input: &mut &str) -> ModalResult<T> {
    cut_err(fail)
        .context(StrContext::Label("number too large to fit in target type"))
        .parse_next(input)
}

fn parse_number(input: &mut &str) -> ModalResult<u64> {
    let digits = digit1.parse_next(input)?;
    match digits.parse::<u64>() {
        Ok(n) => Ok(n),
        Err(_) => overflow_number(input),
    }
}

/// Parse one version component: the zeros written before its value, and the value
fn parse_component(input: &mut &str) -> ModalResult<(u32, u64)> {
    let digits = digit1.parse_next(input)?;
    match digits.parse::<u64>() {
        Ok(n) => {
            let significant = digits.trim_start_matches('0').len().max(1);
            Ok(((digits.len() - significant) as u32, n))
        }
        Err(_) => overflow_number(input),
    }
}

fn parse_letter(input: &mut &str) -> ModalResult<char> {
    one_of('a'..='z').parse_next(input)
}

fn parse_suffix_kind(input: &mut &str) -> ModalResult<SuffixKind> {
    alt((
        "alpha".value(SuffixKind::Alpha),
        "beta".value(SuffixKind::Beta),
        "pre".value(SuffixKind::Pre),
        "rc".value(SuffixKind::Rc),
        "p".value(SuffixKind::P),
    ))
    .parse_next(input)
}

fn parse_suffix(input: &mut &str) -> ModalResult<Suffix> {
    preceded('_', cut_err((parse_suffix_kind, opt(parse_number))))
        .map(|(kind, version)| Suffix { kind, version })
        .parse_next(input)
}

fn parse_revision(input: &mut &str) -> ModalResult<Revision> {
    preceded("-r", cut_err(parse_number))
        .map(Revision)
        .parse_next(input)
}

fn parse_parts(input: &mut &str) -> ModalResult<VersionParts> {
    (
        separated(1.., parse_component, '.'),
        opt(parse_letter),
        repeat(0.., parse_suffix),
        opt(parse_revision),
    )
        .map(
            |(components, letter, suffixes, revision): (Vec<(u32, u64)>, _, _, _)| {
                let numbers = components.iter().map(|&(_, n)| n).collect();
                let leading_zeros = if components.iter().any(|&(z, _)| z > 0) {
                    components.iter().map(|&(z, _)| z).collect()
                } else {
                    Vec::new()
                };
                VersionParts {
                    numbers,
                    leading_zeros,
                    letter,
                    suffixes,
                    revision: revision.unwrap_or_default(),
                }
            },
        )
        .parse_next(input)
}

pub(crate) fn parse_version(input: &mut &str) -> ModalResult<Version> {
    parse_parts
        .with_taken()
        .map(|(parts, raw)| Version(Repr::encode(&parts, Some(raw))))
        .context(StrContext::Label("version"))
        .parse_next(input)
}

pub(crate) fn parse_dep_version(input: &mut &str) -> ModalResult<(Version, bool)> {
    (parse_parts, opt('*'))
        .map(|(parts, glob)| (Version::from(parts), glob.is_some()))
        .context(StrContext::Label("version"))
        .parse_next(input)
}

pub(crate) fn parse_operator(input: &mut &str) -> ModalResult<Operator> {
    alt((
        "<=".value(Operator::LessOrEqual),
        "<".value(Operator::Less),
        ">=".value(Operator::GreaterOrEqual),
        ">".value(Operator::Greater),
        "~".value(Operator::Approximate),
        "=".value(Operator::Equal),
    ))
    .context(StrContext::Label("operator"))
    .parse_next(input)
}

impl FromStr for Version {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_parsing() {
        let v = Version::parse("1.2.3").unwrap();
        assert_eq!(v.component_count(), 3);
        assert_eq!(v.component(0).unwrap(), 1);
        assert_eq!(v.component(1).unwrap(), 2);
        assert_eq!(v.component(2).unwrap(), 3);
        assert_eq!(v.letter(), None);
        assert!(v.suffixes().next().is_none());
        assert_eq!(v.revision().0, 0);
    }

    #[test]
    fn test_version_with_letter() {
        let v = Version::parse("1.2.3a").unwrap();
        assert_eq!(v.letter(), Some('a'));
    }

    #[test]
    fn test_version_with_suffixes() {
        let v = Version::parse("1.2.3_alpha4_beta5").unwrap();
        assert_eq!(v.suffixes().count(), 2);
        assert_eq!(v.suffixes().next().unwrap().kind, SuffixKind::Alpha);
        assert_eq!(v.suffixes().next().unwrap().version.unwrap(), 4);
        assert_eq!(v.suffixes().nth(1).unwrap().kind, SuffixKind::Beta);
        assert_eq!(v.suffixes().nth(1).unwrap().version.unwrap(), 5);
    }

    #[test]
    fn test_version_with_revision() {
        let v = Version::parse("1.2.3-r1").unwrap();
        assert_eq!(v.revision().0, 1);
    }

    #[test]
    fn test_version_comparison() {
        let v1 = Version::parse("1.2.3").unwrap();
        let v2 = Version::parse("1.2.4").unwrap();
        assert!(v1 < v2);

        let v3 = Version::parse("1.2.3-r1").unwrap();
        assert!(v1 < v3);

        let v4 = Version::parse("1.2.3_rc1").unwrap();
        assert!(v4 < v1);
    }

    // The glob (*) suffix is a `Dep` property; see the dep.rs tests.

    #[test]
    fn test_version_component_count() {
        // PMS: "The package manager must neither impose fixed limits upon the number
        // of version components, nor upon the length of any component."

        // Test many components
        let many_components = "1.2.3.4.5.6.7.8.9.10.11.12.13.14.15";
        let version = Version::parse(many_components).unwrap();
        assert_eq!(version.component_count(), 15);

        // Test long component values
        let long_component = "12345678901234567890"; // 20 digits
        let version = Version::parse(long_component).unwrap();
        assert_eq!(version.component(0).unwrap(), 12345678901234567890u64);
    }

    #[test]
    fn test_version_new_simple() {
        let v = Version::new(&[1, 75, 0]);
        assert_eq!(
            v.components().collect::<Vec<_>>().as_slice(),
            vec![1, 75, 0].as_slice()
        );
        assert_eq!(v.letter(), None);
        assert!(v.suffixes().next().is_none());
        assert_eq!(v.revision().0, 0);
        assert_eq!(v.to_string(), "1.75.0");
    }

    #[test]
    #[cfg(feature = "builder")]
    fn test_version_builder_full() {
        let v = Version::from(
            VersionParts::builder(vec![1, 75, 0].into())
                .letter('a')
                .suffixes(vec![
                    Suffix {
                        kind: SuffixKind::Rc,
                        version: Some(1),
                    },
                    Suffix {
                        kind: SuffixKind::P,
                        version: Some(2),
                    },
                ])
                .revision(Revision(3))
                .build(),
        );
        assert_eq!(
            v.components().collect::<Vec<_>>().as_slice(),
            vec![1, 75, 0].as_slice()
        );
        assert_eq!(v.letter(), Some('a'));
        assert_eq!(v.suffixes().count(), 2);
        assert_eq!(v.revision().0, 3);
        assert_eq!(v.to_string(), "1.75.0a_rc1_p2-r3");
    }

    #[test]
    #[cfg(feature = "builder")]
    fn test_version_builder_roundtrip() {
        let original = Version::parse("1.2.3a_rc1_p2-r5").unwrap();
        let built = Version::from(
            VersionParts::builder(vec![1, 2, 3].into())
                .letter('a')
                .suffixes(vec![
                    Suffix {
                        kind: SuffixKind::Rc,
                        version: Some(1),
                    },
                    Suffix {
                        kind: SuffixKind::P,
                        version: Some(2),
                    },
                ])
                .revision(Revision(5))
                .build(),
        );
        assert_eq!(original, built);
    }

    #[test]
    fn test_spelling_preserves_leading_zeros() {
        let v = Version::parse("26.04.0").unwrap();
        assert_eq!(
            v.components().collect::<Vec<_>>().as_slice(),
            vec![26, 4, 0].as_slice()
        );
        assert_eq!(v.to_string(), "26.04.0");
    }

    #[test]
    fn test_spelling_in_cpv_round_trip() {
        use crate::cpv::Cpv;

        let cpv = Cpv::parse("app-accessibility/kontrast-26.04.0").unwrap();
        assert_eq!(cpv.to_string(), "app-accessibility/kontrast-26.04.0");
    }

    // Rendering preserves the ebuild's spelling, so it is deliberately *not*
    // an inverse of `Eq`: Algorithm 3.2 compares the first component
    // numerically, so a leading zero there is not significant. Pinned because
    // "make Display canonical" would look like a free cleanup and would
    // rewrite how the tree's `003.03`/`019`/`073` versions print.
    #[test]
    fn display_is_not_an_inverse_of_eq_for_a_padded_first_component() {
        let padded = Version::parse("01.1").unwrap();
        let plain = Version::parse("1.1").unwrap();
        assert_eq!(padded, plain, "first component compares numerically");
        assert_ne!(padded.to_string(), plain.to_string());
        // A non-first component's zero *is* significant, so both survive.
        let zeroed = Version::parse("1.01").unwrap();
        assert_ne!(zeroed, plain, "Algorithm 3.3 keeps non-first zeros");
        assert_eq!(zeroed.to_string(), "1.01");
    }

    // PMS sets no bound on a version component; u64 is ours, so the limit is
    // documented and enforced by rejecting rather than truncating.
    #[test]
    fn a_component_past_u64_is_rejected_rather_than_truncated() {
        assert!(
            Version::parse("18446744073709551615").is_ok(),
            "u64::MAX fits"
        );
        let over = Version::parse("18446744073709551616")
            .unwrap_err()
            .to_string();
        assert!(over.contains("number too large"), "{over}");
        let later = Version::parse("1.18446744073709551616")
            .unwrap_err()
            .to_string();
        assert!(
            later.contains("number too large"),
            "a later component must commit the overflow, got {later}"
        );
    }

    #[test]
    fn a_dep_keeps_the_leading_zeros_of_its_version() {
        let dep = crate::Dep::parse(">=app-arch/7zip-24.09").unwrap();
        assert_eq!(dep.to_string(), ">=app-arch/7zip-24.09");
    }

    #[test]
    fn test_built_version_displays_canonically() {
        let v = Version::new(&[1, 2, 3]);
        assert_eq!(v.to_string(), "1.2.3");
    }

    #[test]
    fn test_spelling_with_suffix_and_revision() {
        let v = Version::parse("1.2.3a_rc1_p2-r5").unwrap();
        assert_eq!(v.to_string(), "1.2.3a_rc1_p2-r5");
    }

    #[test]
    fn test_spelling_glob_preserved() {
        use crate::dep::Dep;

        let dep = Dep::parse("=dev-util/nvidia-cuda-toolkit-11*").unwrap();
        assert_eq!(dep.to_string(), "=dev-util/nvidia-cuda-toolkit-11*");
    }

    // --- PMS 3.2 / Algorithm 3.1 compliance tests ---

    #[test]
    fn test_version_single_component() {
        // PMS 3.2: "an unsigned integer, followed by zero or more dot-prefixed unsigned integers"
        let v = Version::parse("1").unwrap();
        assert_eq!(
            v.components().collect::<Vec<_>>().as_slice(),
            vec![1].as_slice()
        );
        assert_eq!(v.to_string(), "1");
    }

    #[test]
    fn test_version_letter_ordering() {
        // PMS Algorithm 3.1: letter suffixes compared after numeric components
        let v1 = Version::parse("1.2").unwrap();
        let v2 = Version::parse("1.2a").unwrap();
        let v3 = Version::parse("1.2b").unwrap();
        assert!(v1 < v2);
        assert!(v2 < v3);
    }

    #[test]
    fn test_version_suffix_ordering() {
        // PMS Algorithm 3.1: _alpha < _beta < _pre < _rc < (no suffix) < _p
        let alpha = Version::parse("1.0_alpha").unwrap();
        let beta = Version::parse("1.0_beta").unwrap();
        let pre = Version::parse("1.0_pre").unwrap();
        let rc = Version::parse("1.0_rc").unwrap();
        let base = Version::parse("1.0").unwrap();
        let p = Version::parse("1.0_p").unwrap();

        assert!(alpha < beta);
        assert!(beta < pre);
        assert!(pre < rc);
        assert!(rc < base);
        assert!(base < p);
    }

    #[test]
    fn test_version_suffix_with_number_ordering() {
        // PMS: suffixes with numbers compared numerically
        assert!(Version::parse("1.0_alpha1").unwrap() < Version::parse("1.0_alpha2").unwrap());
        assert!(Version::parse("1.0_rc1").unwrap() < Version::parse("1.0_rc2").unwrap());
        assert!(Version::parse("1.0_p1").unwrap() < Version::parse("1.0_p2").unwrap());
        // Absent number is 0
        assert!(Version::parse("1.0_alpha").unwrap() < Version::parse("1.0_alpha1").unwrap());
    }

    #[test]
    fn test_version_revision_ordering() {
        // PMS: revision compared after everything else
        let v0 = Version::parse("1.0").unwrap();
        let v1 = Version::parse("1.0-r1").unwrap();
        let v2 = Version::parse("1.0-r2").unwrap();
        assert!(v0 < v1);
        assert!(v1 < v2);
    }

    #[test]
    fn test_version_full_ordering_chain() {
        // Algorithm 3.1: numeric → letter → suffixes → revision
        // _p sorts above base, so 1.0_p1 > 1.0-r1 (suffixes compared before revision)
        let versions: Vec<Version> = [
            "1.0_alpha",
            "1.0_alpha1",
            "1.0_beta",
            "1.0_pre",
            "1.0_rc1",
            "1.0_rc2",
            "1.0",
            "1.0-r1",
            "1.0-r2",
            "1.0_p",
            "1.0_p1",
            "1.0a",
            "1.0a-r1",
            "1.0a_p",
            "1.0a_p1",
            "1.0b",
        ]
        .iter()
        .map(|s| Version::parse(s).unwrap())
        .collect();

        for i in 0..versions.len() - 1 {
            assert!(
                versions[i] < versions[i + 1],
                "expected {} < {}",
                versions[i],
                versions[i + 1],
            );
        }
    }

    #[test]
    fn test_version_unequal_component_count() {
        // PMS Algorithms 3.2–3.3: after pairwise equality, the longer side wins.
        // Real Portage: 1 < 1.0 < 1.0.0; 1.0 != 1.0.0.
        assert!(Version::parse("1").unwrap() < Version::parse("1.0").unwrap());
        assert!(Version::parse("1.0").unwrap() < Version::parse("1.0.0").unwrap());
        assert!(Version::parse("1.0").unwrap() < Version::parse("1.0.1").unwrap());
        assert_ne!(
            Version::parse("1.0").unwrap(),
            Version::parse("1.0.0").unwrap()
        );
        assert_ne!(Version::parse("1").unwrap(), Version::parse("1.0").unwrap());
    }

    #[test]
    fn test_version_leading_zeros_pms_3_3() {
        // Portage vercmp: 1.01 < 1.1; 1.010 == 1.01.
        assert!(Version::parse("1.01").unwrap() < Version::parse("1.1").unwrap());
        assert_eq!(
            Version::parse("1.010").unwrap(),
            Version::parse("1.01").unwrap()
        );
        assert_ne!(
            Version::parse("1.01").unwrap(),
            Version::parse("1.1").unwrap()
        );
    }

    #[test]
    fn test_suffix_absent_number_equals_zero() {
        // PMS Algorithm 3.6 / Portage: _alpha == _alpha0.
        assert_eq!(
            Version::parse("1.0_alpha").unwrap(),
            Version::parse("1.0_alpha0").unwrap()
        );
        assert_eq!(
            Version::parse("1.0_alpha")
                .unwrap()
                .cmp(&Version::parse("1.0_alpha0").unwrap()),
            Ordering::Equal
        );
    }

    #[test]
    fn test_glob_matches_no_zero_pad() {
        // =pkg-1.0* must not match version 1 (fewer components than pattern).
        let pattern = Version::parse("1.0").unwrap();
        assert!(!Version::parse("1").unwrap().glob_matches(&pattern));
        assert!(Version::parse("1.0").unwrap().glob_matches(&pattern));
        assert!(Version::parse("1.0.1").unwrap().glob_matches(&pattern));
        assert!(Version::parse("1.0_alpha").unwrap().glob_matches(&pattern));
        assert!(!Version::parse("1.1").unwrap().glob_matches(&pattern));
        // Leading-zero string mode on the truncated prefix.
        assert!(
            Version::parse("1.010.5")
                .unwrap()
                .glob_matches(&Version::parse("1.01").unwrap())
        );
    }

    #[test]
    fn test_glob_matches_suffix_and_revision() {
        let rc1 = Version::parse("262_rc1").unwrap();
        assert!(Version::parse("262_rc1").unwrap().glob_matches(&rc1));
        assert!(Version::parse("262_rc1-r3").unwrap().glob_matches(&rc1));
        assert!(Version::parse("262_rc1_p2").unwrap().glob_matches(&rc1));
        assert!(!Version::parse("262.1_rc1").unwrap().glob_matches(&rc1));
        assert!(!Version::parse("262a_rc1").unwrap().glob_matches(&rc1));
        assert!(!Version::parse("262").unwrap().glob_matches(&rc1));
        assert!(!Version::parse("262_rc2").unwrap().glob_matches(&rc1));
        // Absent suffix number is 0, so `_rc` is `_rc0`, not a prefix of `_rc1`.
        assert!(!Version::parse("262_rc").unwrap().glob_matches(&rc1));

        let rc = Version::parse("1.0_rc").unwrap();
        assert!(Version::parse("1.0_rc").unwrap().glob_matches(&rc));
        assert!(Version::parse("1.0_rc0").unwrap().glob_matches(&rc));
        // Unnumbered, the last suffix takes any number of its kind, as in Portage.
        assert!(Version::parse("1.0_rc3").unwrap().glob_matches(&rc));
        assert!(Version::parse("1.0_rc3_p1-r2").unwrap().glob_matches(&rc));
        assert!(!Version::parse("1.0_p3").unwrap().glob_matches(&rc));
        assert!(!Version::parse("1.0").unwrap().glob_matches(&rc));
        // A written number stays exact, `0` included, and so does a revision pattern's tail.
        let p0 = Version::parse("1.0_p0").unwrap();
        assert!(!Version::parse("1.0_p12").unwrap().glob_matches(&p0));
        let p_r1 = Version::parse("1.0_p-r1").unwrap();
        assert!(!Version::parse("1.0_p2-r1").unwrap().glob_matches(&p_r1));

        let rev = Version::parse("1.0-r1").unwrap();
        assert!(Version::parse("1.0-r1").unwrap().glob_matches(&rev));
        assert!(!Version::parse("1.0.1-r1").unwrap().glob_matches(&rev));
        assert!(!Version::parse("1.0a-r1").unwrap().glob_matches(&rev));
        assert!(!Version::parse("1.0_p1-r1").unwrap().glob_matches(&rev));
        assert!(!Version::parse("1.0-r2").unwrap().glob_matches(&rev));
        assert!(!Version::parse("1.0").unwrap().glob_matches(&rev));

        let letter = Version::parse("1.0a").unwrap();
        assert!(Version::parse("1.0a").unwrap().glob_matches(&letter));
        assert!(Version::parse("1.0a_p1-r2").unwrap().glob_matches(&letter));
        assert!(!Version::parse("1.0.5a").unwrap().glob_matches(&letter));
        assert!(!Version::parse("1.0b").unwrap().glob_matches(&letter));

        // Without a letter, suffix or revision, more numbers may follow.
        let plain = Version::parse("1.0").unwrap();
        assert!(
            Version::parse("1.0.5a_rc1-r1")
                .unwrap()
                .glob_matches(&plain)
        );
        // `041` sits between `04` and `05`.
        assert!(
            !Version::parse("26.041")
                .unwrap()
                .glob_matches(&Version::parse("26.04").unwrap())
        );
    }

    /// `glob_matches` against the answers Portage's `match_from_list` gives for `=V*`
    #[test]
    fn glob_matches_agree_with_portage() {
        // (candidate, pattern, Portage's answer)
        let cases = [
            ("262", "262_rc1", false),
            ("262_rc2", "262_rc1", false),
            ("262_rc1-r1", "262_rc1", true),
            ("0_pre20241109", "0_p", false),
            ("1.0", "1.0_rc3_p1", false),
            ("1.0.0-r2", "1.0.0_p20250710", false),
            ("4.13.2_p20240222", "4.13.2_p20", false),
            ("10", "1", false),
            ("1.0-r10", "1.0-r1", false),
            ("1.2.3", "1.2", true),
            ("1.2_rc1", "1.2", true),
            ("1.2a", "1.2", true),
            ("1.0_rc3", "1.0_rc", true),
            ("1.0-r3", "1.0", true),
            ("007.1", "7", true),
            ("7.1", "007", true),
            ("26.04.3", "26.04", true),
            ("26.4.3", "26.04", false),
            ("1.0_p20250710", "1.0_p", true),
            ("262.1_rc1", "262_rc1", false),
            ("1.0.1-r1", "1.0-r1", false),
            ("1.0-r2", "1.0-r1", false),
            ("1.2_alpha_alpha", "1.2", true),
            ("26.041", "26.04", false),
        ];
        let bad: Vec<_> = cases
            .iter()
            .filter(|(c, p, want)| {
                let (c, p) = (Version::parse(c).unwrap(), Version::parse(p).unwrap());
                c.glob_matches(&p) != *want
            })
            .map(|(c, p, want)| format!("={p}* vs {c}: portage={want}"))
            .collect();
        assert!(bad.is_empty(), "{bad:#?}");
    }

    /// Where Portage's text prefix and PMS disagree, em follows PMS 8.3.1: components compare
    /// as components, and Algorithm 3.3 makes `01` and `010`, or `0` and `00`, equal
    #[test]
    fn glob_matches_pms_equal_components_unlike_portage() {
        for (candidate, pattern) in [("1.010.5", "1.01"), ("2025.04.14.00", "2025.04.14.0")] {
            let (c, p) = (
                Version::parse(candidate).unwrap(),
                Version::parse(pattern).unwrap(),
            );
            assert!(c.glob_matches(&p), "={pattern}* vs {candidate}");
        }
    }

    #[test]
    fn glob_bounds_agree_with_glob_matches() {
        // Patterns whose last numeric component neither is `0` nor starts with one.
        let patterns = [
            "1",
            "1.2",
            "1.2.3",
            "1.2a",
            "1.2z",
            "1.2_rc",
            "1.2_rc3",
            "1.2_p",
            "1.2_p1",
            "1.2-r1",
            "1.2a_beta2-r3",
            "26.04.3",
            "26.04a",
            "26.04_rc1",
            "26.04-r2",
            "1.2_alpha",
            "1.2_pre",
            "1.2_p0",
            "1.2_rc3_p",
            "1.2a_p",
            "1.2z_p",
        ];
        let candidates = [
            "1.2_alpha3",
            "1.2_pre1",
            "1.2_rc9_beta1",
            "1.2_rc3_p9",
            "1.2_rc4_p1",
            "1.2_p7_rc1",
            "1.2a_p2",
            "1.2b_p1",
            "1.2z_p4",
            "1.2.0_p1",
            "1",
            "1.1",
            "1.2",
            "1.2.0",
            "1.2.3",
            "1.2.3.4",
            "1.2.4",
            "1.3",
            "1.10",
            "1.2a",
            "1.2b",
            "1.2z",
            "1.2.0a",
            "1.2.5a",
            "1.2_alpha",
            "1.2_rc",
            "1.2_rc0",
            "1.2_rc3",
            "1.2_rc3_p1",
            "1.2_rc4",
            "1.2_p",
            "1.2_p1",
            "1.2_p20250710",
            "1.2-r1",
            "1.2-r2",
            "1.2-r10",
            "1.2_p1-r1",
            "1.2a-r1",
            "1.2a_beta2",
            "1.2a_beta2-r3",
            "1.2a_beta2_p1-r3",
            "1.2a_beta3-r3",
            "1.2.3-r1",
            "1.2.30",
            "1.2.03",
            "2",
            "26.04",
            "26.040",
            "26.04.3",
            "26.04.3-r1",
            "26.04.4",
            "26.04a",
            "26.04b",
            "26.04_rc1",
            "26.04_rc1_p2",
            "26.04_rc2",
            "26.04-r2",
            "26.04-r3",
            "26.4",
            "26.05",
        ];
        let mut bad = Vec::new();
        for p in patterns {
            let pattern = Version::parse(p).unwrap();
            assert!(pattern.glob_bounds_exact(), "{p}");
            let (lower, upper) = pattern.glob_bounds();
            for c in candidates {
                let v = Version::parse(c).unwrap();
                let in_range = v >= lower && upper.as_ref().is_none_or(|u| v < *u);
                if in_range != v.glob_matches(&pattern) {
                    bad.push(format!("={p}* vs {c}: range {in_range}"));
                }
            }
        }
        assert!(bad.is_empty(), "{bad:#?}");

        for p in ["1.0", "26.04", "3.0.0", "1.09"] {
            assert!(!Version::parse(p).unwrap().glob_bounds_exact(), "{p}");
        }
        let max = u64::MAX;
        assert!(
            !Version::parse(&format!("1_rc{max}"))
                .unwrap()
                .glob_bounds_exact()
        );
        assert!(Version::parse("0").unwrap().glob_bounds_exact());
    }

    #[test]
    fn test_glob_bounds_keep_written_digits() {
        let (lower, upper) = Version::parse("26.04.3").unwrap().glob_bounds();
        assert_eq!(lower.to_string(), "26.04.3_alpha");
        assert_eq!(upper.unwrap().to_string(), "26.04.4_alpha");

        let (lower, upper) = Version::parse("26.04").unwrap().glob_bounds();
        assert_eq!(lower.to_string(), "26.04_alpha");
        assert_eq!(upper.unwrap().to_string(), "26.05_alpha");

        let (lower, upper) = Version::parse("1.09").unwrap().glob_bounds();
        assert_eq!(upper.unwrap().to_string(), "1.10_alpha");
        assert_eq!(lower.to_string(), "1.09_alpha");

        let (lower, upper) = Version::parse("1.0a").unwrap().glob_bounds();
        assert_eq!(lower.to_string(), "1.0a_alpha");
        assert_eq!(upper.unwrap().to_string(), "1.0b_alpha");

        let (lower, upper) = Version::parse("1.0z").unwrap().glob_bounds();
        assert_eq!(lower.to_string(), "1.0z_alpha");
        assert_eq!(upper.unwrap().to_string(), "1.0.0_alpha");

        let (lower, upper) = Version::parse("262_rc1").unwrap().glob_bounds();
        assert_eq!(lower.to_string(), "262_rc1_alpha");
        assert_eq!(upper.unwrap().to_string(), "262_rc2_alpha");

        let (lower, upper) = Version::parse("1.0_rc").unwrap().glob_bounds();
        assert_eq!(lower.to_string(), "1.0_rc_alpha");
        assert_eq!(upper.unwrap().to_string(), "1.0");
        for (pattern, end) in [
            ("1.0_pre", "1.0_rc_alpha"),
            ("1.0_p", "1.0a_alpha"),
            ("1.0c_p", "1.0d_alpha"),
            ("1.0_rc2_p", "1.0_rc3_alpha"),
            ("1.0_p0", "1.0_p1_alpha"),
        ] {
            let (_, upper) = Version::parse(pattern).unwrap().glob_bounds();
            assert_eq!(upper.unwrap().to_string(), end, "{pattern}");
        }

        let (lower, upper) = Version::parse("1.0-r1").unwrap().glob_bounds();
        assert_eq!(lower.to_string(), "1.0-r1");
        assert_eq!(upper.unwrap().to_string(), "1.0-r2");

        let max = u64::MAX.to_string();
        let (lower, upper) = Version::parse(&max).unwrap().glob_bounds();
        assert_eq!(lower.to_string(), format!("{max}_alpha"));
        assert!(upper.is_none());

        let (_, upper) = Version::parse(&format!("1.{max}")).unwrap().glob_bounds();
        assert_eq!(upper.unwrap().to_string(), "2_alpha");

        let (_, upper) = Version::parse(&format!("1_rc{max}")).unwrap().glob_bounds();
        assert_eq!(upper.unwrap().to_string(), "2_alpha");
    }

    #[test]
    fn operator_matching_handles_glob_and_revision_free_approximation() {
        let candidate = Version::parse("1.2.3-r2").unwrap();
        assert!(candidate.matches_operator(
            Operator::Approximate,
            false,
            &Version::parse("1.2.3").unwrap()
        ));
        assert!(!candidate.matches_operator(
            Operator::Approximate,
            false,
            &Version::parse("1.2.4").unwrap()
        ));
        assert!(Version::parse("1.2.3.4").unwrap().matches_operator(
            Operator::Equal,
            true,
            &Version::parse("1.2").unwrap(),
        ));
        assert!(!Version::parse("1.2").unwrap().matches_operator(
            Operator::Equal,
            true,
            &Version::parse("1.2.3").unwrap(),
        ));
    }

    #[test]
    fn test_version_display_round_trip() {
        let inputs = [
            "1",
            "1.2",
            "1.2.3",
            "1.2.3a",
            "1.2.3_alpha",
            "1.2.3_alpha4",
            "1.2.3a_rc1_p2-r5",
            "1.2.3-r0",
            "26.04.0",
        ];
        for input in inputs {
            let v = Version::parse(input).unwrap();
            assert_eq!(v.to_string(), input, "round-trip failed for: {input}");
        }
    }

    #[test]
    fn test_operator_display() {
        assert_eq!(Operator::Less.to_string(), "<");
        assert_eq!(Operator::LessOrEqual.to_string(), "<=");
        assert_eq!(Operator::Equal.to_string(), "=");
        assert_eq!(Operator::Approximate.to_string(), "~");
        assert_eq!(Operator::GreaterOrEqual.to_string(), ">=");
        assert_eq!(Operator::Greater.to_string(), ">");
    }

    #[test]
    fn test_version_eq_ord_consistency() {
        use std::cmp::Ordering;
        let a = Version::parse("1.2.3").unwrap();
        let b = Version::parse("1.2.3").unwrap();

        assert_eq!(a, b);
        assert_eq!(a.cmp(&b), Ordering::Equal);

        let mut s = std::collections::HashSet::new();
        let v1 = Version::parse("1.2.3").unwrap();
        let v2 = Version::parse("1.2.3").unwrap();
        s.insert(v1.clone());
        assert!(
            s.contains(&v2),
            "hash/equality must agree for identical versions"
        );
    }

    // H2: `_p<N>` suffix with large N values and revisions, as seen in kpathsea
    // (`6.4.0_p20240311-r1`) and other TeX packages.  These must compare as
    // GREATER than the base version so `>=6.4.0` matches `6.4.0_p20240311-r1`.
    #[test]
    fn p_suffix_large_number_is_greater_than_base() {
        let base = Version::parse("6.4.0").unwrap();
        let patched = Version::parse("6.4.0_p20240311").unwrap();
        let revised = Version::parse("6.4.0_p20240311-r1").unwrap();
        assert!(patched > base, "6.4.0_p20240311 must be > 6.4.0");
        assert!(
            revised > patched,
            "6.4.0_p20240311-r1 must be > 6.4.0_p20240311"
        );
        assert!(revised > base, "6.4.0_p20240311-r1 must be > 6.4.0");
    }

    #[test]
    fn ge_constraint_matches_p_suffix_versions() {
        // Simulates the `>=dev-libs/kpathsea-6.4.0` constraint.
        let constraint = Version::parse("6.4.0").unwrap();
        for candidate in [
            "6.4.0_p20230311",
            "6.4.0_p20240311",
            "6.4.0_p20240311-r1",
            "6.5.0",
        ] {
            let v = Version::parse(candidate).unwrap();
            assert!(
                v >= constraint,
                "{candidate} must satisfy >=6.4.0 but comparison says otherwise"
            );
        }
        // Strictly less — should NOT match
        for candidate in ["6.3.5", "6.3.5_p20230311", "6.4.0_alpha"] {
            let v = Version::parse(candidate).unwrap();
            assert!(v < constraint, "{candidate} should NOT satisfy >=6.4.0");
        }
    }

    #[test]
    fn ge_constraint_large_major_matches() {
        // `>=media-libs/harfbuzz-1.4.5` must be satisfied by `12.3.2`.
        let constraint = Version::parse("1.4.5").unwrap();
        let candidate = Version::parse("12.3.2").unwrap();
        assert!(candidate >= constraint, "12.3.2 must satisfy >=1.4.5");
    }

    #[test]
    fn texlive_package_versions_parse() {
        // Versions used by texlive packages — some have large `_p` date suffixes.
        let versions = [
            "2024",
            "2024-r1",
            "2024-r2",
            "2024_p72890",
            "2024_p71912",
            "1.2.43-r2", // libpng
            "6.4.0_p20240311",
            "6.4.0_p20240311-r1",
            "1.4.5",  // harfbuzz lower bound
            "12.3.2", // harfbuzz installed version
        ];
        for v in versions {
            Version::parse(v).unwrap_or_else(|e| panic!("failed to parse version '{v}': {e}"));
        }
    }

    #[test]
    fn common_versions_fit_inline_in_24_bytes() {
        assert_eq!(std::mem::size_of::<Version>(), 24);
        for s in ["1.2.3", "5.15.2-r3", "2.0_pre20240101", "1.2.3_rc1-r1"] {
            let v = Version::parse(s).unwrap();
            assert!(matches!(v.0, Repr::Inline { .. }), "{s}");
        }
    }

    #[test]
    fn spelling_stays_out_of_equality_and_hash() {
        use std::hash::{BuildHasher, RandomState};
        let state = RandomState::new();
        for (a, b) in [
            ("1.0", "1.00"),
            ("3.03", "003.03"),
            ("1.2_p", "1.2_p0"),
            ("1-r1", "1-r01"),
        ] {
            let (va, vb) = (Version::parse(a).unwrap(), Version::parse(b).unwrap());
            assert_eq!(va, vb);
            assert_eq!(state.hash_one(&va), state.hash_one(&vb));
            assert_eq!(
                (va.to_string(), vb.to_string()),
                (a.to_string(), b.to_string())
            );
        }
    }

    #[test]
    fn component_order_follows_algorithms_3_2_and_3_3() {
        use std::hash::{BuildHasher, RandomState};
        // The first component compares as an integer; a later one starting with `0` compares as
        // a string with trailing zeros dropped, against either kind.
        let pms = |i: usize, a: &str, b: &str| {
            let int = |s: &str| s.parse::<u64>().unwrap();
            if i > 0 && (a.starts_with('0') || b.starts_with('0')) {
                a.trim_end_matches('0').cmp(b.trim_end_matches('0'))
            } else {
                int(a).cmp(&int(b))
            }
        };
        let written = [
            "0", "00", "000", "1", "01", "001", "10", "010", "100", "5", "50", "05",
        ];
        let hasher = RandomState::new();
        for i in 0..2 {
            let at = |c: &str| {
                Version::parse(&if i == 0 {
                    format!("{c}.1")
                } else {
                    format!("1.{c}")
                })
                .unwrap()
            };
            for a in written {
                for b in written {
                    let (va, vb) = (at(a), at(b));
                    assert_eq!(va.cmp(&vb), pms(i, a, b), "{va} vs {vb}");
                    if va == vb {
                        assert_eq!(hasher.hash_one(&va), hasher.hash_one(&vb), "{va} vs {vb}");
                    }
                }
            }
        }
    }
}
