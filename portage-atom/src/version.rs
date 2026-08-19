use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use smallvec::SmallVec;
use smol_str::SmolStr;
use winnow::Parser;
use winnow::ascii::digit1;
use winnow::combinator::{alt, cut_err, opt, preceded, repeat, separated};
use winnow::error::StrContext;
use winnow::prelude::*;
use winnow::token::one_of;

use crate::error::{Error, Result};

/// Package revision (`-r1`, `-r2`, etc.)
///
/// Tracks packaging changes independently of the upstream version.
/// A revision of `0` is the implicit default and is omitted from display.
///
/// See [PMS 3.2](https://projects.gentoo.org/pms/9/pms.html#version-specifications).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
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
#[derive(Debug, Clone)]
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
/// components, so this avoids a heap allocation for the common case —
/// `Version` is cloned constantly during dependency resolution (per solver
/// decision, per dependency edge, inside the range-algebra of the
/// version-set crate it's compared through).
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
#[derive(Debug, Clone)]
#[cfg_attr(feature = "builder", derive(bon::Builder))]
pub struct Version {
    /// Dot-separated numeric components (e.g. `[1, 2, 3]` for `1.2.3`)
    ///
    /// Integer form used by callers that need major/minor extraction or
    /// range algebra. Leading zeros are preserved separately in
    /// [`Self::digits`] for PMS Algorithm 3.3.
    #[cfg_attr(feature = "builder", builder(start_fn))]
    pub numbers: Numbers,
    /// Original decimal digit strings for each component (same length as
    /// [`Self::numbers`]). Preserves leading zeros so Algorithm 3.3 can
    /// distinguish `1.01` from `1.1`. Empty when constructed via structural
    /// update that only sets `numbers` — comparison then falls back to
    /// formatting each `u64`
    #[cfg_attr(feature = "builder", builder(skip))]
    pub digits: SmallVec<[SmolStr; 4]>,
    /// Optional single lowercase letter after the numeric components
    pub letter: Option<char>,
    /// Zero or more version suffixes (`_alpha`, `_beta`, `_pre`, `_rc`, `_p`)
    #[cfg_attr(feature = "builder", builder(default))]
    pub suffixes: Vec<Suffix>,
    /// Package revision; defaults to `0` (omitted from display)
    #[cfg_attr(feature = "builder", builder(default))]
    pub revision: Revision,
    /// The version string exactly as parsed, preserving leading zeros
    /// (e.g. `"26.04.0"` instead of the reconstructed `"26.4.0"`)
    ///
    /// `SmolStr` rather than `String`: cheap to clone (inline for short
    /// strings, `Arc<str>` refcount bump otherwise) without touching the
    /// process-global string interner, which is meant for a small,
    /// heavily-reused vocabulary — version strings are the opposite (tens
    /// of thousands of one-shot values per scan the interner never frees).
    ///
    /// `None` when constructed programmatically via [`Version::new`] or the builder.
    #[cfg_attr(feature = "builder", builder(skip))]
    pub raw: Option<SmolStr>,
}

/// Digit string for component `i`, preferring the original parse text
fn component_digits<'a>(v: &'a Version, i: usize) -> Option<std::borrow::Cow<'a, str>> {
    if let Some(d) = v.digits.get(i) {
        return Some(std::borrow::Cow::Borrowed(d.as_str()));
    }
    v.numbers
        .get(i)
        .map(|n| std::borrow::Cow::Owned(n.to_string()))
}

/// PMS Algorithm 3.2 / 3.3 integer compare on digit strings (no fixed-width limit)
fn cmp_int_digits(a: &str, b: &str) -> Ordering {
    let a = a.trim_start_matches('0');
    let b = b.trim_start_matches('0');
    let a = if a.is_empty() { "0" } else { a };
    let b = if b.is_empty() { "0" } else { b };
    match a.len().cmp(&b.len()) {
        Ordering::Equal => a.cmp(b),
        o => o,
    }
}

/// Compare one numeric component pair
///
/// * First component (index 0): always Algorithm 3.2 integer compare.
/// * Later components: Algorithm 3.3 — if either side has a leading `0`,
///   strip trailing zeros and compare as strings; otherwise integer compare.
/// * Missing component (one side exhausted): caller handles length rule.
fn cmp_component(i: usize, a: &str, b: &str) -> Ordering {
    if i == 0 {
        return cmp_int_digits(a, b);
    }
    if a.starts_with('0') || b.starts_with('0') {
        let a = a.trim_end_matches('0');
        let b = b.trim_end_matches('0');
        a.cmp(b)
    } else {
        cmp_int_digits(a, b)
    }
}

/// Hash a single component consistently with [`cmp_component`] equality
fn hash_component<H: Hasher>(i: usize, digits: &str, state: &mut H) {
    if i == 0 || !digits.starts_with('0') {
        let s = digits.trim_start_matches('0');
        let s = if s.is_empty() { "0" } else { s };
        s.hash(state);
    } else {
        digits.trim_end_matches('0').hash(state);
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        // Keep Eq consistent with Ord (PMS Algorithm 3.1 Equal).
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

impl Hash for Version {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Component count is part of identity: 1.0 != 1.0.0.
        let n = self.numbers.len().max(self.digits.len());
        n.hash(state);
        for i in 0..n {
            if let Some(d) = component_digits(self, i) {
                hash_component(i, d.as_ref(), state);
            }
        }
        self.letter.hash(state);
        self.suffixes.hash(state);
        self.revision.hash(state);
    }
}

impl Version {
    /// Create a version from its dot-separated numeric components
    ///
    /// `Version::new(&[1, 75, 0])` produces `1.75.0`. All optional fields
    /// (letter, suffixes, revision, glob) default to their zero values.
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
        let digits = numbers
            .iter()
            .map(|n| SmolStr::new(n.to_string()))
            .collect();
        Version {
            numbers: Numbers::from_slice(numbers),
            digits,
            letter: None,
            suffixes: Vec::new(),
            revision: Revision::default(),
            raw: None,
        }
    }

    /// Parse a version string (without a leading operator)
    ///
    /// Accepts forms like `1.2.3`, `1.2.3a_rc1_p2-r5`.
    pub fn parse(input: &str) -> Result<Self> {
        parse_version
            .parse(input)
            .map_err(|e| Error::InvalidVersion(format!("{}: {}", input, e)))
    }

    /// Check whether this version matches a glob pattern (PMS 8.3.1 `=V*`)
    ///
    /// Truncates this version to the number of numeric components in
    /// `pattern` and requires PMS component equality on that prefix. A
    /// candidate with *fewer* components than the pattern does **not** match
    /// (no zero-padding). If `pattern` specifies a letter, the candidate must
    /// match it exactly. Candidate suffixes and revision are ignored.
    pub fn glob_matches(&self, pattern: &Version) -> bool {
        let n = pattern.numbers.len().max(pattern.digits.len());
        let cand_n = self.numbers.len().max(self.digits.len());
        if cand_n < n {
            return false;
        }
        for i in 0..n {
            let Some(a) = component_digits(self, i) else {
                return false;
            };
            let Some(b) = component_digits(pattern, i) else {
                return false;
            };
            if cmp_component(i, a.as_ref(), b.as_ref()) != Ordering::Equal {
                return false;
            }
        }
        if let Some(pattern_letter) = pattern.letter
            && self.letter.unwrap_or('\0') != pattern_letter
        {
            return false;
        }
        true
    }

    /// Return the version without its revision, for `~` (approximate)
    /// comparison per [PMS 8.3.1]
    ///
    /// [PMS 8.3.1]: https://projects.gentoo.org/pms/9/pms.html#operators
    pub fn base(&self) -> Self {
        Version {
            numbers: self.numbers.clone(),
            digits: self.digits.clone(),
            letter: self.letter,
            suffixes: self.suffixes.clone(),
            revision: Revision::default(),
            raw: None,
        }
    }

    /// Return the version stripped of suffixes and revision, for `*` glob
    /// comparison per [PMS 8.3.1]
    ///
    /// [PMS 8.3.1]: https://projects.gentoo.org/pms/9/pms.html#operators
    pub fn without_suffix(&self) -> Self {
        Version {
            numbers: self.numbers.clone(),
            digits: self.digits.clone(),
            letter: self.letter,
            suffixes: Vec::new(),
            revision: Revision::default(),
            raw: None,
        }
    }

    /// Format the version portion (numbers, letter, suffixes, revision,
    /// glob) without the operator
    ///
    /// Uses the raw string when available to preserve leading zeros in
    /// numeric components.
    pub(crate) fn fmt_version(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if let Some(ref raw) = self.raw {
            write!(f, "{raw}")
        } else {
            for (i, num) in self.numbers.iter().enumerate() {
                if i > 0 {
                    write!(f, ".")?;
                }
                write!(f, "{num}")?;
            }
            if let Some(letter) = self.letter {
                write!(f, "{letter}")?;
            }
            for suffix in &self.suffixes {
                write!(f, "{suffix}")?;
            }
            write!(f, "{}", self.revision)?;
            Ok(())
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.fmt_version(f)
    }
}

/// PMS version comparison (Algorithm 3.1)
impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        // Algorithms 3.2–3.3: pairwise component compare; when one side runs
        // out of components first, the longer version is greater (no zero-pad).
        let a_len = self.numbers.len().max(self.digits.len());
        let b_len = other.numbers.len().max(other.digits.len());
        let shared = a_len.min(b_len);
        for i in 0..shared {
            let a = component_digits(self, i).expect("component in range");
            let b = component_digits(other, i).expect("component in range");
            match cmp_component(i, a.as_ref(), b.as_ref()) {
                Ordering::Equal => continue,
                o => return o,
            }
        }
        match a_len.cmp(&b_len) {
            Ordering::Equal => {}
            o => return o,
        }

        // Algorithm 3.4: letter (absent sorts before any letter)
        let a_letter = self.letter.unwrap_or('\0');
        let b_letter = other.letter.unwrap_or('\0');
        match a_letter.cmp(&b_letter) {
            Ordering::Equal => {}
            o => return o,
        }

        // Algorithms 3.5–3.6: suffixes (_p above base; others below)
        let max_suffixes = self.suffixes.len().max(other.suffixes.len());
        for i in 0..max_suffixes {
            match (self.suffixes.get(i), other.suffixes.get(i)) {
                (Some(a), Some(b)) => match a.cmp(b) {
                    Ordering::Equal => continue,
                    o => return o,
                },
                (Some(s), None) => {
                    return if s.kind == SuffixKind::P {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    };
                }
                (None, Some(s)) => {
                    return if s.kind == SuffixKind::P {
                        Ordering::Less
                    } else {
                        Ordering::Greater
                    };
                }
                (None, None) => break,
            }
        }

        // Algorithm 3.7: revision
        self.revision.cmp(&other.revision)
    }
}

// Winnow parsers

fn parse_number(input: &mut &str) -> ModalResult<u64> {
    digit1.try_map(|s: &str| s.parse::<u64>()).parse_next(input)
}

/// Parse one version component, keeping the original digit text for PMS 3.3
fn parse_component(input: &mut &str) -> ModalResult<(SmolStr, u64)> {
    digit1
        .try_map(|s: &str| s.parse::<u64>().map(|n| (SmolStr::new(s), n)))
        .parse_next(input)
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

pub(crate) fn parse_version(input: &mut &str) -> ModalResult<Version> {
    (
        separated(1.., parse_component, '.'),
        opt(parse_letter),
        repeat(0.., parse_suffix),
        opt(parse_revision),
    )
        .with_taken()
        .map(
            |((components, letter, suffixes, revision), raw): (
                (Vec<(SmolStr, u64)>, _, _, _),
                _,
            )| {
                let mut numbers = Numbers::with_capacity(components.len());
                let mut digits = SmallVec::with_capacity(components.len());
                for (d, n) in components {
                    digits.push(d);
                    numbers.push(n);
                }
                Version {
                    numbers,
                    digits,
                    letter,
                    suffixes,
                    revision: revision.unwrap_or_default(),
                    raw: Some(SmolStr::new(raw)),
                }
            },
        )
        .context(StrContext::Label("version"))
        .parse_next(input)
}

pub(crate) fn parse_version_no_raw(input: &mut &str) -> ModalResult<(Version, bool)> {
    (
        separated(1.., parse_component, '.'),
        opt(parse_letter),
        repeat(0.., parse_suffix),
        opt(parse_revision),
        opt('*'),
    )
        .map(
            |(components, letter, suffixes, revision, has_glob): (
                Vec<(SmolStr, u64)>,
                _,
                _,
                _,
                _,
            )| {
                let mut numbers = Numbers::with_capacity(components.len());
                let mut digits = SmallVec::with_capacity(components.len());
                for (d, n) in components {
                    digits.push(d);
                    numbers.push(n);
                }
                (
                    Version {
                        numbers,
                        digits,
                        letter,
                        suffixes,
                        revision: revision.unwrap_or_default(),
                        raw: None,
                    },
                    has_glob.is_some(),
                )
            },
        )
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
        assert_eq!(v.numbers.len(), 3);
        assert_eq!(v.numbers[0], 1);
        assert_eq!(v.numbers[1], 2);
        assert_eq!(v.numbers[2], 3);
        assert_eq!(v.letter, None);
        assert!(v.suffixes.is_empty());
        assert_eq!(v.revision.0, 0);
    }

    #[test]
    fn test_version_with_letter() {
        let v = Version::parse("1.2.3a").unwrap();
        assert_eq!(v.letter, Some('a'));
    }

    #[test]
    fn test_version_with_suffixes() {
        let v = Version::parse("1.2.3_alpha4_beta5").unwrap();
        assert_eq!(v.suffixes.len(), 2);
        assert_eq!(v.suffixes[0].kind, SuffixKind::Alpha);
        assert_eq!(v.suffixes[0].version.unwrap(), 4);
        assert_eq!(v.suffixes[1].kind, SuffixKind::Beta);
        assert_eq!(v.suffixes[1].version.unwrap(), 5);
    }

    #[test]
    fn test_version_with_revision() {
        let v = Version::parse("1.2.3-r1").unwrap();
        assert_eq!(v.revision.0, 1);
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

    // Issue 3: glob (*) suffix is now handled at the Dep level, not Version.
    // See dep.rs tests for glob matching via Dep::parse("=pkg-ver*").

    #[test]
    fn test_version_component_count() {
        // PMS: "The package manager must neither impose fixed limits upon the number
        // of version components, nor upon the length of any component."

        // Test many components
        let many_components = "1.2.3.4.5.6.7.8.9.10.11.12.13.14.15";
        let version = Version::parse(many_components).unwrap();
        assert_eq!(version.numbers.len(), 15);

        // Test long component values
        let long_component = "12345678901234567890"; // 20 digits
        let version = Version::parse(long_component).unwrap();
        assert_eq!(version.numbers[0], 12345678901234567890u64);
    }

    #[test]
    fn test_version_new_simple() {
        let v = Version::new(&[1, 75, 0]);
        assert_eq!(v.numbers.as_slice(), vec![1, 75, 0].as_slice());
        assert_eq!(v.letter, None);
        assert!(v.suffixes.is_empty());
        assert_eq!(v.revision.0, 0);
        assert_eq!(v.to_string(), "1.75.0");
    }

    #[test]
    #[cfg(feature = "builder")]
    fn test_version_builder_full() {
        let v = Version::builder(vec![1, 75, 0].into())
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
            .build();
        assert_eq!(v.numbers.as_slice(), vec![1, 75, 0].as_slice());
        assert_eq!(v.letter, Some('a'));
        assert_eq!(v.suffixes.len(), 2);
        assert_eq!(v.revision.0, 3);
        assert_eq!(v.to_string(), "1.75.0a_rc1_p2-r3");
    }

    #[test]
    #[cfg(feature = "builder")]
    fn test_version_builder_roundtrip() {
        let original = Version::parse("1.2.3a_rc1_p2-r5").unwrap();
        let built = Version::builder(vec![1, 2, 3].into())
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
            .build();
        assert_eq!(original, built);
    }

    #[test]
    fn test_raw_preserves_leading_zeros() {
        let v = Version::parse("26.04.0").unwrap();
        assert_eq!(v.numbers.as_slice(), vec![26, 4, 0].as_slice());
        assert_eq!(v.raw.as_deref(), Some("26.04.0"));
        assert_eq!(v.to_string(), "26.04.0");
    }

    #[test]
    fn test_raw_in_dep_displays_canonical() {
        use crate::dep::Dep;

        let dep = Dep::parse(">=app-accessibility/kontrast-26.04.0").unwrap();
        assert_eq!(dep.to_string(), ">=app-accessibility/kontrast-26.4.0");
    }

    #[test]
    fn test_raw_in_cpv_round_trip() {
        use crate::cpv::Cpv;

        let cpv = Cpv::parse("app-accessibility/kontrast-26.04.0").unwrap();
        assert_eq!(cpv.to_string(), "app-accessibility/kontrast-26.04.0");
    }

    #[test]
    fn test_raw_none_for_programmatic() {
        let v = Version::new(&[1, 2, 3]);
        assert!(v.raw.is_none());
        assert_eq!(v.to_string(), "1.2.3");
    }

    #[test]
    fn test_raw_with_suffix_and_revision() {
        let v = Version::parse("1.2.3a_rc1_p2-r5").unwrap();
        assert_eq!(v.raw.as_deref(), Some("1.2.3a_rc1_p2-r5"));
        assert_eq!(v.to_string(), "1.2.3a_rc1_p2-r5");
    }

    #[test]
    fn test_raw_glob_preserved() {
        use crate::dep::Dep;

        let dep = Dep::parse("=dev-util/nvidia-cuda-toolkit-11*").unwrap();
        assert_eq!(dep.to_string(), "=dev-util/nvidia-cuda-toolkit-11*");
    }

    // --- PMS 3.2 / Algorithm 3.1 compliance tests ---

    #[test]
    fn test_version_single_component() {
        // PMS 3.2: "an unsigned integer, followed by zero or more dot-prefixed unsigned integers"
        let v = Version::parse("1").unwrap();
        assert_eq!(v.numbers.as_slice(), vec![1].as_slice());
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
}
