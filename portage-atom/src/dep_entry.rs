use std::fmt;
use std::sync::Arc;

use gentoo_interner::{DefaultInterner, Interned};
use winnow::ascii::{multispace0, multispace1};
use winnow::combinator::{cut_err, delimited, dispatch, opt, peek, preceded, repeat, terminated};
use winnow::error::StrContext;
use winnow::prelude::*;
use winnow::token::any;

use crate::UseFlagLookup;
use crate::dep::{Dep, parse_dep};
use crate::error::{Error, Result};
use crate::parsers::parse_ident_with_at;

/// Structured dependency tree entry
///
/// Represents the forms that appear in ebuild `*DEPEND` variables
/// (PMS 8.2): bare atoms, USE-conditional groups, all-of groups,
/// and any-of / exactly-one-of / at-most-one-of groups.
///
/// See [PMS 8.2](https://projects.gentoo.org/pms/9/pms.html#dependency-specification-format).
// `Atom` is the dominant variant, so boxing it would add a per-atom
// allocation on parse and on every `evaluate_use` clone for no memory win.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DepEntry {
    /// A single dependency atom
    Atom(Dep),
    /// `flag? ( children )` or `!flag? ( children )` conditional group
    UseConditional {
        /// USE flag name
        flag: Interned<DefaultInterner>,
        /// `true` for `!use?` (negated conditional)
        negate: bool,
        /// Dependencies guarded by this flag
        children: Vec<DepEntry>,
    },
    /// `( a b c )` — all of the children must be matched
    ///
    /// A bare parenthesised group representing an all-of dependency
    /// specification per [PMS 8.2.1](https://projects.gentoo.org/pms/9/pms.html#all-of-dependency-specifications).
    AllOf(Vec<DepEntry>),
    /// `|| ( a b c )` — any one of the children satisfies the dependency
    AnyOf(Vec<DepEntry>),
    /// `^^ ( a b c )` — exactly one child must be matched
    ExactlyOneOf(Vec<DepEntry>),
    /// `?? ( a b c )` — at most one child must be matched
    AtMostOneOf(Vec<DepEntry>),
}

/// A dependency-class tree (e.g. one package version's parsed `DEPEND`),
/// `Arc`-wrapped so cloning it is a refcount bump rather than a deep copy
///
/// Ebuild metadata gets re-converted into a solver's own representation on
/// every rebuild — for the PubGrub-backed resolver, up to ~8x per
/// invocation during the USE-dep co-solve fixpoint — and a real package can
/// carry hundreds of parsed atoms per class, so a plain `Vec<DepEntry>`
/// clone at every one of those rebuilds was a measured, real cost.
///
/// Derefs to `[DepEntry]`, so existing read-only call sites (iteration,
/// `.is_empty()`, indexing, `evaluate_use(&list, ..)`) need no changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DepList(Arc<Vec<DepEntry>>);

impl DepList {
    /// Wrap an owned dependency list
    pub fn new(entries: Vec<DepEntry>) -> Self {
        Self(Arc::new(entries))
    }

    /// Mutable access to the underlying list, cloning it first if shared
    /// with another `DepList` (copy-on-write via `Arc::make_mut`).
    pub fn make_mut(&mut self) -> &mut Vec<DepEntry> {
        Arc::make_mut(&mut self.0)
    }
}

impl From<Vec<DepEntry>> for DepList {
    fn from(entries: Vec<DepEntry>) -> Self {
        Self::new(entries)
    }
}

/// A DEPEND-class field's raw md5-cache text, parsed to a [`DepList`] on
/// first access
///
/// A resolve's solver only ever examines the dependencies of the small
/// fraction of a repo's ebuilds it actually considers as candidates —
/// everything else is filtered by keyword/mask/slot before a dependency is
/// needed at all. Holding the raw text and parsing on demand, memoized,
/// avoids materializing a `DepEntry` tree for every candidate the solve
/// never looks at.
///
/// A malformed field parses to an empty list rather than propagating an
/// error: unlike eager parsing (where a bad field failed the whole
/// `CacheEntry::parse` and made the package invisible), the package stays
/// visible with just that one field treated as empty.
#[derive(Debug, Default)]
pub struct LazyDepList {
    raw: Option<Arc<str>>,
    parsed: std::sync::OnceLock<DepList>,
}

impl LazyDepList {
    /// Wrap raw md5-cache field text, parsed on first [`Self::list`] call
    pub fn from_raw(s: &str) -> Self {
        if s.is_empty() {
            return Self::default();
        }
        Self {
            raw: Some(Arc::from(s)),
            parsed: std::sync::OnceLock::new(),
        }
    }

    /// Wrap an already-parsed list (sourced-ebuild metadata, tests) — no
    /// raw text, so [`Self::list`] never has anything to parse
    pub fn from_list(list: DepList) -> Self {
        let parsed = std::sync::OnceLock::new();
        let _ = parsed.set(list);
        Self { raw: None, parsed }
    }

    /// Whether the field is empty, without forcing the parse
    pub fn is_empty_raw(&self) -> bool {
        self.raw.is_none() && self.parsed.get().is_none_or(|l| l.is_empty())
    }

    /// The parsed list, computed and memoized on first call
    pub fn list(&self) -> &DepList {
        self.parsed.get_or_init(|| match &self.raw {
            Some(s) => DepEntry::parse(s).map(DepList::new).unwrap_or_default(),
            None => DepList::default(),
        })
    }

    /// Mutable access to the parsed list, forcing the parse first
    ///
    /// Drops the raw text: after mutation there is no source text left to
    /// re-derive from.
    pub fn make_mut(&mut self) -> &mut Vec<DepEntry> {
        self.list();
        self.raw = None;
        self.parsed
            .get_mut()
            .expect("just initialized by list()")
            .make_mut()
    }
}

impl Clone for LazyDepList {
    fn clone(&self) -> Self {
        let parsed = std::sync::OnceLock::new();
        if let Some(v) = self.parsed.get() {
            let _ = parsed.set(v.clone());
        }
        Self {
            raw: self.raw.clone(),
            parsed,
        }
    }
}

impl PartialEq for LazyDepList {
    fn eq(&self, other: &Self) -> bool {
        self.list() == other.list()
    }
}

impl Eq for LazyDepList {}

impl From<DepList> for LazyDepList {
    fn from(list: DepList) -> Self {
        Self::from_list(list)
    }
}

impl From<Vec<DepEntry>> for LazyDepList {
    fn from(entries: Vec<DepEntry>) -> Self {
        Self::from_list(DepList::new(entries))
    }
}

impl std::ops::Deref for DepList {
    type Target = [DepEntry];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'a> IntoIterator for &'a DepList {
    type Item = &'a DepEntry;
    type IntoIter = std::slice::Iter<'a, DepEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl FromIterator<DepEntry> for DepList {
    fn from_iter<I: IntoIterator<Item = DepEntry>>(iter: I) -> Self {
        Self::new(iter.into_iter().collect())
    }
}

impl DepEntry {
    /// Parse a full dependency string into a list of entries
    ///
    /// Accepts the format used in ebuild `*DEPEND` variables: whitespace-separated
    /// atoms, `|| ( ... )` any-of groups, `^^ ( ... )` exactly-one-of groups,
    /// `?? ( ... )` at-most-one-of groups, `use? ( ... )` conditional groups,
    /// and bare `( ... )` all-of groups.
    ///
    /// # Examples
    ///
    /// ```
    /// use portage_atom::DepEntry;
    ///
    /// let entries = DepEntry::parse("dev-lang/rust ssl? ( dev-libs/openssl )").unwrap();
    /// assert_eq!(entries.len(), 2);
    /// ```
    pub fn parse(input: &str) -> Result<Vec<DepEntry>> {
        parse_dep_string
            .parse(input)
            .map_err(|e| Error::InvalidDepString(format!("{e}")))
    }

    /// Evaluate USE conditionals using a predicate
    ///
    /// Resolves every `UseConditional` node: active `flag? ( ... )` and inactive
    /// `!flag? ( ... )` are replaced by their (recursively evaluated) children;
    /// the others are dropped. Empty `||` / `^^` groups are dropped (EAPI 0–6
    /// table 8.6). Callers with an EAPI use [`Self::evaluate_use_groups`].
    ///
    /// `active` is any [`UseFlagLookup`] — typically a `HashSet` of interned
    /// flags, a `&[&str]` slice, or a folded `UseConfig` (from the
    /// `portage-atom-pubgrub` crate).
    ///
    /// ```
    /// use portage_atom::DepEntry;
    /// use portage_atom::interner::Interned;
    /// use std::collections::HashSet;
    ///
    /// let entries = DepEntry::parse("ssl? ( dev-libs/openssl )").unwrap();
    /// let ssl = Interned::intern("ssl");
    /// let active: HashSet<Interned<_>> = [ssl].into();
    /// let resolved = DepEntry::evaluate_use(&entries, &active);
    /// assert_eq!(resolved.len(), 1);
    /// ```
    pub fn evaluate_use<A: UseFlagLookup + ?Sized>(entries: &[Self], active: &A) -> Vec<Self> {
        Self::evaluate_use_groups(entries, active, true)
    }

    /// [`Self::evaluate_use`] with PMS table 8.6 empty-group policy
    ///
    /// `empty_groups_match` is EAPI 0–6: drop empty `||` / `^^`. EAPI 7–9
    /// keep them so the caller sees an unsatisfiable group.
    pub fn evaluate_use_groups<A: UseFlagLookup + ?Sized>(
        entries: &[Self],
        active: &A,
        empty_groups_match: bool,
    ) -> Vec<Self> {
        Self::eval_entries(entries, active, empty_groups_match)
    }

    fn eval_entries<A: UseFlagLookup + ?Sized>(
        entries: &[Self],
        active: &A,
        empty_groups_match: bool,
    ) -> Vec<Self> {
        entries
            .iter()
            .flat_map(|e| e.eval_use_one(active, empty_groups_match))
            .collect()
    }

    fn eval_use_one<A: UseFlagLookup + ?Sized>(
        &self,
        active: &A,
        empty_groups_match: bool,
    ) -> Vec<Self> {
        match self {
            DepEntry::Atom(_) => vec![self.clone()],
            DepEntry::UseConditional {
                flag,
                negate,
                children,
            } => {
                let flag_active = active.use_flag_active(*flag);
                if flag_active != *negate {
                    Self::eval_entries(children, active, empty_groups_match)
                } else {
                    vec![]
                }
            }
            DepEntry::AllOf(children) => {
                let ev = Self::eval_entries(children, active, empty_groups_match);
                if ev.is_empty() {
                    vec![]
                } else {
                    vec![DepEntry::AllOf(ev)]
                }
            }
            DepEntry::AnyOf(children) => {
                let ev = Self::eval_entries(children, active, empty_groups_match);
                if ev.is_empty() {
                    if empty_groups_match {
                        vec![]
                    } else {
                        vec![DepEntry::AnyOf(vec![])]
                    }
                } else {
                    vec![DepEntry::AnyOf(ev)]
                }
            }
            DepEntry::ExactlyOneOf(children) => {
                let ev = Self::eval_entries(children, active, empty_groups_match);
                if ev.is_empty() {
                    if empty_groups_match {
                        vec![]
                    } else {
                        vec![DepEntry::ExactlyOneOf(vec![])]
                    }
                } else {
                    vec![DepEntry::ExactlyOneOf(ev)]
                }
            }
            DepEntry::AtMostOneOf(children) => {
                let ev = Self::eval_entries(children, active, empty_groups_match);
                if ev.is_empty() {
                    vec![]
                } else {
                    vec![DepEntry::AtMostOneOf(ev)]
                }
            }
        }
    }
}

impl fmt::Display for DepEntry {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            DepEntry::Atom(dep) => write!(f, "{dep}"),
            DepEntry::UseConditional {
                flag,
                negate,
                children,
            } => {
                if *negate {
                    write!(f, "!")?;
                }
                write!(f, "{flag}? ( ")?;
                for (i, child) in children.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{child}")?;
                }
                write!(f, " )")
            }
            DepEntry::AllOf(entries) => fmt_group(f, "( ", entries),
            DepEntry::AnyOf(entries) => fmt_group(f, "|| ( ", entries),
            DepEntry::ExactlyOneOf(entries) => fmt_group(f, "^^ ( ", entries),
            DepEntry::AtMostOneOf(entries) => fmt_group(f, "?? ( ", entries),
        }
    }
}

fn fmt_group(f: &mut std::fmt::Formatter, prefix: &str, entries: &[DepEntry]) -> std::fmt::Result {
    write!(f, "{prefix}")?;
    for (i, entry) in entries.iter().enumerate() {
        if i > 0 {
            write!(f, " ")?;
        }
        write!(f, "{entry}")?;
    }
    write!(f, " )")
}

// Winnow parsers

/// Parse a complete dependency string (top-level)
pub(crate) fn parse_dep_string(input: &mut &str) -> ModalResult<Vec<DepEntry>> {
    terminated(parse_dep_entries, multispace0).parse_next(input)
}

/// Parse zero or more dependency entries separated by whitespace
///
/// Stops when it encounters `)` or end-of-input.
fn parse_dep_entries(input: &mut &str) -> ModalResult<Vec<DepEntry>> {
    repeat(0.., preceded(multispace0, parse_dep_entry))
        .fold(Vec::new, |mut acc: Vec<DepEntry>, entry: DepEntry| {
            acc.push(entry);
            acc
        })
        .parse_next(input)
}

/// Quick lookahead: returns `true` when the remaining input is a USE-conditional
/// (`flag? ( ... )` / `!flag? ( ... )`) rather than a dependency atom
///
/// PMS 8.2 defines USE-conditionals as `'!'? flag-name '?' ws '(' ... ')'`.
/// The discriminant is `?` followed by whitespace then `(`.  This sequence
/// never appears in dependency atom syntax — category and package names
/// (PMS 3.1.1, 3.1.2) use `[A-Za-z0-9+_.-]`, which does not contain `?`.
///
/// Short-circuits on `/`, `:`, or `[` — these are atom-only characters that
/// appear before any `?` could in a USE-conditional.
fn is_use_conditional(input: &str) -> bool {
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'?' => {
                return bytes.get(i + 1) == Some(&b'(')
                    || (bytes[i + 1].is_ascii_whitespace() && bytes.get(i + 2) == Some(&b'('));
            }
            b'/' | b':' | b'[' => return false,
            _ => i += 1,
        }
    }
    false
}

/// Parse a single dependency entry
///
/// Uses `dispatch!(peek(any); ...)` to route on the first character:
/// - `|` → any-of group (`|| ( ... )`)
/// - `^` → exactly-one-of group (`^^ ( ... )`)
/// - `?` → at-most-one-of group (`?? ( ... )`)
///
/// - `(` → all-of group (`( ... )`)
/// - `>`, `<`, `~`, `=` → versioned atom
/// - anything else → lookahead to distinguish USE-conditional from atom
fn parse_dep_entry(input: &mut &str) -> ModalResult<DepEntry> {
    dispatch! {peek(any);
        '|' => parse_any_of,
        '^' => parse_exactly_one_of,
        '?' => parse_at_most_one_of,
        '(' => parse_all_of,
        '>' | '<' | '~' | '=' => parse_dep
            .context(StrContext::Label("dependency atom"))
            .map(DepEntry::Atom),
        _ => dispatch_dep_entry_fallback,
    }
    .parse_next(input)
}

fn dispatch_dep_entry_fallback(input: &mut &str) -> ModalResult<DepEntry> {
    if is_use_conditional(input) {
        parse_use_conditional.parse_next(input)
    } else {
        parse_dep
            .context(StrContext::Label("dependency atom"))
            .map(DepEntry::Atom)
            .parse_next(input)
    }
}

/// Parse `|| ( entry+ )`
///
/// After consuming `||`, uses `cut_err` to commit — a missing `(` or `)`
/// becomes a hard error instead of backtracking into `alt`.
fn parse_any_of(input: &mut &str) -> ModalResult<DepEntry> {
    "||".parse_next(input)?;
    multispace1.parse_next(input)?;
    cut_err(delimited('(', parse_dep_entries, (multispace0, ')')))
        .context(StrContext::Label("'||' group"))
        .map(DepEntry::AnyOf)
        .parse_next(input)
}

/// Parse `^^ ( entry+ )`
///
/// After consuming `^^`, uses `cut_err` to commit — a missing `(` or `)`
/// becomes a hard error instead of backtracking into `alt`.
fn parse_exactly_one_of(input: &mut &str) -> ModalResult<DepEntry> {
    "^^".parse_next(input)?;
    multispace1.parse_next(input)?;
    cut_err(delimited('(', parse_dep_entries, (multispace0, ')')))
        .context(StrContext::Label("'^^' group"))
        .map(DepEntry::ExactlyOneOf)
        .parse_next(input)
}

/// Parse `?? ( entry+ )`
///
/// After consuming `??`, uses `cut_err` to commit — a missing `(` or `)`
/// becomes a hard error instead of backtracking into `alt`.
fn parse_at_most_one_of(input: &mut &str) -> ModalResult<DepEntry> {
    "??".parse_next(input)?;
    multispace1.parse_next(input)?;
    cut_err(delimited('(', parse_dep_entries, (multispace0, ')')))
        .context(StrContext::Label("'??' group"))
        .map(DepEntry::AtMostOneOf)
        .parse_next(input)
}

/// Parse `[!]flag? ( entry+ )` per PMS 8.2
///
/// Uses `parse_ident_with_at` for the flag name (PMS 3.1.4: `[A-Za-z0-9+_@-]`,
/// starting with alphanumeric). After `?`, `cut_err` commits so a missing
/// `( ... )` is a hard error.
fn parse_use_conditional(input: &mut &str) -> ModalResult<DepEntry> {
    let negate = opt('!').parse_next(input)?.is_some();
    let flag: Interned<DefaultInterner> = parse_ident_with_at
        .verify(|s: &str| s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()))
        .map(|s: &str| Interned::intern(s))
        .parse_next(input)?;
    '?'.parse_next(input)?;
    // After '?', committed to USE conditional
    multispace1.parse_next(input)?;
    let children = cut_err(delimited('(', parse_dep_entries, (multispace0, ')')))
        .context(StrContext::Label("USE conditional group"))
        .parse_next(input)?;
    Ok(DepEntry::UseConditional {
        flag,
        negate,
        children,
    })
}

/// Parse `( entry* )` — all-of group
///
/// After consuming `(`, uses `cut_err` for the closing `)`.
fn parse_all_of(input: &mut &str) -> ModalResult<DepEntry> {
    delimited(
        '(',
        parse_dep_entries,
        cut_err((multispace0, ')')).context(StrContext::Label("closing ')'")),
    )
    .map(DepEntry::AllOf)
    .parse_next(input)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::dep::Blocker;
    use crate::use_dep::{UseDefault, UseDepKind};
    use crate::version::{Operator, Version};

    #[test]
    fn empty_string() {
        let entries = DepEntry::parse("").unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn single_atom() {
        let entries = DepEntry::parse("dev-lang/rust").unwrap();
        assert_eq!(entries.len(), 1);
        assert!(matches!(&entries[0], DepEntry::Atom(dep) if dep.category() == "dev-lang"));
    }

    #[test]
    fn multiple_atoms() {
        let entries = DepEntry::parse("dev-lang/rust dev-libs/bar").unwrap();
        assert_eq!(entries.len(), 2);
        assert!(matches!(&entries[0], DepEntry::Atom(dep) if dep.package() == "rust"));
        assert!(matches!(&entries[1], DepEntry::Atom(dep) if dep.package() == "bar"));
    }

    #[test]
    fn versioned_atom() {
        let entries = DepEntry::parse(">=dev-lang/rust-1.75.0").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            DepEntry::Atom(dep) => {
                assert!(dep.version.is_some());
                let v = dep.version.as_ref().unwrap();
                assert_eq!(v.numbers[0], 1);
                assert_eq!(v.numbers[1], 75);
            }
            _ => panic!("expected Atom"),
        }
    }

    #[test]
    fn any_of_group() {
        let entries = DepEntry::parse("|| ( dev-libs/bar dev-libs/baz )").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            DepEntry::AnyOf(children) => {
                assert_eq!(children.len(), 2);
                assert!(matches!(&children[0], DepEntry::Atom(dep) if dep.package() == "bar"));
                assert!(matches!(&children[1], DepEntry::Atom(dep) if dep.package() == "baz"));
            }
            _ => panic!("expected AnyOf"),
        }

        assert!(DepEntry::parse("||( dev-libs/bar )").is_err());
    }

    #[test]
    fn use_conditional() {
        let entries = DepEntry::parse("ssl? ( dev-libs/openssl )").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            DepEntry::UseConditional {
                flag,
                negate,
                children,
            } => {
                assert_eq!(flag, "ssl");
                assert!(!negate);
                assert_eq!(children.len(), 1);
            }
            _ => panic!("expected UseConditional"),
        }

        assert!(DepEntry::parse("ssl?( dev-libs/openssl )").is_err());
    }

    #[test]
    fn negated_use_conditional() {
        let entries = DepEntry::parse("!debug? ( dev-libs/bar )").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            DepEntry::UseConditional {
                flag,
                negate,
                children,
            } => {
                assert_eq!(flag, "debug");
                assert!(negate);
                assert_eq!(children.len(), 1);
            }
            _ => panic!("expected UseConditional"),
        }
    }

    #[test]
    fn nested_use_in_any_of() {
        let entries = DepEntry::parse("|| ( ssl? ( dev-libs/openssl ) dev-libs/gnutls )").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            DepEntry::AnyOf(children) => {
                assert_eq!(children.len(), 2);
                assert!(matches!(
                    &children[0],
                    DepEntry::UseConditional { flag, .. } if flag == "ssl"
                ));
                assert!(matches!(&children[1], DepEntry::Atom(dep) if dep.package() == "gnutls"));
            }
            _ => panic!("expected AnyOf"),
        }
    }

    #[test]
    fn all_of_group() {
        let entries = DepEntry::parse("( dev-libs/a dev-libs/b )").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            DepEntry::AllOf(children) => {
                assert_eq!(children.len(), 2);
                assert!(matches!(&children[0], DepEntry::Atom(dep) if dep.package() == "a"));
                assert!(matches!(&children[1], DepEntry::Atom(dep) if dep.package() == "b"));
            }
            _ => panic!("expected AllOf"),
        }
    }

    #[test]
    fn all_of_round_trip() {
        let input = "( dev-libs/a dev-libs/b )";
        let entries = DepEntry::parse(input).unwrap();
        let displayed: Vec<String> = entries.iter().map(|e| e.to_string()).collect();
        let rejoined = displayed.join(" ");
        let reparsed = DepEntry::parse(&rejoined).unwrap();
        assert_eq!(entries, reparsed);
    }

    #[test]
    fn any_of_with_all_of_round_trip() {
        let input = "|| ( ( dev-lang/python:3.14 dev-python/sphinx ) ( dev-lang/python:3.13 dev-python/sphinx ) )";
        let entries = DepEntry::parse(input).unwrap();
        let displayed: Vec<String> = entries.iter().map(|e| e.to_string()).collect();
        let rejoined = displayed.join(" ");
        assert_eq!(rejoined, input);
        let reparsed = DepEntry::parse(&rejoined).unwrap();
        assert_eq!(entries, reparsed);
    }

    #[test]
    fn complex_mixed() {
        let input =
            "dev-lang/rust || ( dev-libs/openssl dev-libs/libressl ) ssl? ( net-misc/curl )";
        let entries = DepEntry::parse(input).unwrap();
        assert_eq!(entries.len(), 3);
        assert!(matches!(&entries[0], DepEntry::Atom(_)));
        assert!(matches!(&entries[1], DepEntry::AnyOf(_)));
        assert!(matches!(&entries[2], DepEntry::UseConditional { .. }));
    }

    #[test]
    fn display_round_trip() {
        let input =
            "dev-lang/rust || ( dev-libs/openssl dev-libs/libressl ) ssl? ( net-misc/curl )";
        let entries = DepEntry::parse(input).unwrap();
        let displayed: Vec<String> = entries.iter().map(|e| e.to_string()).collect();
        let rejoined = displayed.join(" ");
        let reparsed = DepEntry::parse(&rejoined).unwrap();
        assert_eq!(entries, reparsed);
    }

    #[test]
    fn blocker_in_dep_string() {
        let entries = DepEntry::parse("!dev-libs/old").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            DepEntry::Atom(dep) => {
                assert_eq!(dep.blocker, Some(Blocker::Weak));
                assert_eq!(dep.package(), "old");
            }
            _ => panic!("expected Atom"),
        }
    }

    #[test]
    fn strong_blocker_in_dep_string() {
        let entries = DepEntry::parse("!!dev-libs/old").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            DepEntry::Atom(dep) => {
                assert_eq!(dep.blocker, Some(Blocker::Strong));
                assert_eq!(dep.package(), "old");
            }
            _ => panic!("expected Atom"),
        }
    }

    #[test]
    fn slot_in_dep_string() {
        let entries = DepEntry::parse("dev-lang/python:3.11").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            DepEntry::Atom(dep) => {
                assert!(dep.slot_dep.is_some());
            }
            _ => panic!("expected Atom"),
        }
    }

    #[test]
    fn error_unmatched_paren() {
        let result = DepEntry::parse("( dev-libs/a");
        assert!(result.is_err());
        match result.unwrap_err() {
            Error::InvalidDepString(_) => {}
            other => panic!("expected InvalidDepString, got: {other:?}"),
        }
    }

    // --- dispatch-specific edge cases ---

    // `>=`, `<`, `~`, `=` dispatch directly to atom parser.
    #[test]
    fn operator_prefixed_atoms() {
        for input in [
            ">=dev-lang/rust-1.75.0",
            "<dev-libs/bar-2.0",
            "~dev-libs/baz-1.0",
            "=dev-libs/qux-3.0",
        ] {
            let entries = DepEntry::parse(input).unwrap();
            assert_eq!(entries.len(), 1, "failed for: {input}");
            assert!(matches!(&entries[0], DepEntry::Atom(dep) if dep.version.is_some()));
        }
    }

    // `!` followed by a category/package must parse as a blocker atom, not
    // a USE conditional.
    #[test]
    fn blocker_not_confused_with_use_conditional() {
        let entries = DepEntry::parse("!dev-libs/old ssl? ( dev-libs/openssl )").unwrap();
        assert_eq!(entries.len(), 2);
        assert!(matches!(&entries[0], DepEntry::Atom(dep) if dep.blocker == Some(Blocker::Weak)));
        assert!(
            matches!(&entries[1], DepEntry::UseConditional { flag, negate, .. }
            if flag == "ssl" && !negate)
        );
    }

    // Empty USE conditional body.
    #[test]
    fn empty_use_conditional() {
        let entries = DepEntry::parse("test? ( )").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            DepEntry::UseConditional { flag, children, .. } => {
                assert_eq!(flag, "test");
                assert!(children.is_empty());
            }
            _ => panic!("expected UseConditional"),
        }
    }

    // Missing `( )` after `||` is a hard error (cut_err), not a backtrack.
    #[test]
    fn error_any_of_missing_paren() {
        assert!(DepEntry::parse("|| dev-libs/a").is_err());
    }

    // Missing `( )` after `flag?` is a hard error (cut_err).
    #[test]
    fn error_use_cond_missing_paren() {
        assert!(DepEntry::parse("ssl? dev-libs/openssl").is_err());
    }

    // Extra whitespace should be tolerated everywhere.
    #[test]
    fn extra_whitespace() {
        let entries = DepEntry::parse("  dev-lang/rust   ssl? (  dev-libs/openssl  )  ").unwrap();
        assert_eq!(entries.len(), 2);
        assert!(matches!(&entries[0], DepEntry::Atom(_)));
        assert!(matches!(&entries[1], DepEntry::UseConditional { .. }));
    }

    // Display round-trip with nested structures.
    #[test]
    fn display_round_trip_nested() {
        let input = "|| ( ssl? ( dev-libs/openssl ) !ssl? ( dev-libs/libressl ) )";
        let entries = DepEntry::parse(input).unwrap();
        let displayed: Vec<String> = entries.iter().map(|e| e.to_string()).collect();
        let rejoined = displayed.join(" ");
        let reparsed = DepEntry::parse(&rejoined).unwrap();
        assert_eq!(entries, reparsed);
    }

    // Atoms with USE deps and repo in a dep string.
    #[test]
    fn complex_atoms_in_dep_string() {
        let entries =
            DepEntry::parse(">=dev-lang/rust-1.75.0:0[llvm_targets_AMDGPU] dev-libs/bar::gentoo")
                .unwrap();
        assert_eq!(entries.len(), 2);
        match &entries[0] {
            DepEntry::Atom(dep) => {
                assert!(dep.version.is_some());
                assert!(dep.slot_dep.is_some());
                assert!(dep.use_deps.is_some());
            }
            _ => panic!("expected Atom"),
        }
        match &entries[1] {
            DepEntry::Atom(dep) => {
                assert_eq!(dep.repo, Some(gentoo_interner::Interned::intern("gentoo")));
            }
            _ => panic!("expected Atom"),
        }
    }

    #[test]
    fn exactly_one_of_group() {
        let entries = DepEntry::parse("^^ ( dev-libs/bar dev-libs/baz )").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            DepEntry::ExactlyOneOf(children) => {
                assert_eq!(children.len(), 2);
                assert!(matches!(&children[0], DepEntry::Atom(dep) if dep.package() == "bar"));
                assert!(matches!(&children[1], DepEntry::Atom(dep) if dep.package() == "baz"));
            }
            _ => panic!("expected ExactlyOneOf"),
        }
    }

    #[test]
    fn at_most_one_of_group() {
        let entries = DepEntry::parse("?? ( dev-libs/bar dev-libs/baz )").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            DepEntry::AtMostOneOf(children) => {
                assert_eq!(children.len(), 2);
                assert!(matches!(&children[0], DepEntry::Atom(dep) if dep.package() == "bar"));
                assert!(matches!(&children[1], DepEntry::Atom(dep) if dep.package() == "baz"));
            }
            _ => panic!("expected AtMostOneOf"),
        }
    }

    #[test]
    fn display_round_trip_exactly_one_of() {
        let input = "^^ ( dev-libs/bar dev-libs/baz )";
        let entries = DepEntry::parse(input).unwrap();
        let displayed: Vec<String> = entries.iter().map(|e| e.to_string()).collect();
        let rejoined = displayed.join(" ");
        let reparsed = DepEntry::parse(&rejoined).unwrap();
        assert_eq!(entries, reparsed);
    }

    #[test]
    fn display_round_trip_at_most_one_of() {
        let input = "?? ( dev-libs/bar dev-libs/baz )";
        let entries = DepEntry::parse(input).unwrap();
        let displayed: Vec<String> = entries.iter().map(|e| e.to_string()).collect();
        let rejoined = displayed.join(" ");
        let reparsed = DepEntry::parse(&rejoined).unwrap();
        assert_eq!(entries, reparsed);
    }

    #[test]
    fn nested_use_in_exactly_one_of() {
        let entries = DepEntry::parse("^^ ( ssl? ( dev-libs/openssl ) dev-libs/gnutls )").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            DepEntry::ExactlyOneOf(children) => {
                assert_eq!(children.len(), 2);
                assert!(matches!(
                    &children[0],
                    DepEntry::UseConditional { flag, .. } if flag == "ssl"
                ));
                assert!(matches!(&children[1], DepEntry::Atom(dep) if dep.package() == "gnutls"));
            }
            _ => panic!("expected ExactlyOneOf"),
        }
    }

    #[test]
    fn mixed_with_exactly_one_of() {
        let input =
            "dev-lang/rust ^^ ( dev-libs/openssl dev-libs/libressl ) ssl? ( net-misc/curl )";
        let entries = DepEntry::parse(input).unwrap();
        assert_eq!(entries.len(), 3);
        assert!(matches!(&entries[0], DepEntry::Atom(_)));
        assert!(matches!(&entries[1], DepEntry::ExactlyOneOf(_)));
        assert!(matches!(&entries[2], DepEntry::UseConditional { .. }));
    }

    #[test]
    fn error_exactly_one_of_missing_paren() {
        assert!(DepEntry::parse("^^ dev-libs/a").is_err());
    }

    #[test]
    fn error_at_most_one_of_missing_paren() {
        assert!(DepEntry::parse("?? dev-libs/a").is_err());
    }

    // Issue 1: Empty USE dep brackets []
    #[test]
    fn test_atoms_with_empty_use_deps() {
        let entries = DepEntry::parse("dev-libs/libbsd[]").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            DepEntry::Atom(dep) => {
                assert_eq!(dep.package(), "libbsd");
                assert!(dep.use_deps.as_ref().unwrap().is_empty());
            }
            _ => panic!("expected Atom"),
        }

        let entries = DepEntry::parse(">=dev-libs/libatomic_ops-7.4[]").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            DepEntry::Atom(dep) => {
                assert_eq!(dep.package(), "libatomic_ops");
                assert!(dep.version.is_some());
                assert!(dep.use_deps.as_ref().unwrap().is_empty());
            }
            _ => panic!("expected Atom"),
        }
    }

    // Issue 2: USE dep defaults (+) and (-)
    #[test]
    fn test_atoms_with_use_dep_defaults() {
        let entries = DepEntry::parse("sys-libs/ncurses:=[unicode(+)?]").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            DepEntry::Atom(dep) => {
                assert_eq!(dep.package(), "ncurses");
                let use_deps = dep.use_deps.as_ref().unwrap();
                assert_eq!(use_deps.len(), 1);
                assert_eq!(use_deps[0].flag, "unicode");
                assert_eq!(use_deps[0].kind, UseDepKind::Conditional);
                assert_eq!(use_deps[0].default, Some(UseDefault::Enabled));
            }
            _ => panic!("expected Atom"),
        }

        let entries = DepEntry::parse("dev-libs/libxml2[icu(+)]").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            DepEntry::Atom(dep) => {
                assert_eq!(dep.package(), "libxml2");
                let use_deps = dep.use_deps.as_ref().unwrap();
                assert_eq!(use_deps.len(), 1);
                assert_eq!(use_deps[0].flag, "icu");
                assert_eq!(use_deps[0].kind, UseDepKind::Enabled);
                assert_eq!(use_deps[0].default, Some(UseDefault::Enabled));
            }
            _ => panic!("expected Atom"),
        }

        let entries = DepEntry::parse("sys-libs/readline:=[unicode(-)]").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            DepEntry::Atom(dep) => {
                assert_eq!(dep.package(), "readline");
                let use_deps = dep.use_deps.as_ref().unwrap();
                assert_eq!(use_deps.len(), 1);
                assert_eq!(use_deps[0].flag, "unicode");
                assert_eq!(use_deps[0].kind, UseDepKind::Enabled);
                assert_eq!(use_deps[0].default, Some(UseDefault::Disabled));
            }
            _ => panic!("expected Atom"),
        }
    }

    // Issue 3: = version prefix with glob * suffix
    #[test]
    fn test_atoms_with_glob_version() {
        let entries = DepEntry::parse("=dev-util/nvidia-cuda-toolkit-11*").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            DepEntry::Atom(dep) => {
                assert_eq!(dep.package(), "nvidia-cuda-toolkit");
                assert!(dep.version.is_some());
                assert_eq!(dep.op, Some(Operator::Equal));
                assert!(dep.glob);
            }
            _ => panic!("expected Atom"),
        }

        let entries = DepEntry::parse("=sys-devel/gcc-13*").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            DepEntry::Atom(dep) => {
                assert_eq!(dep.package(), "gcc");
                assert!(dep.version.is_some());
                assert_eq!(dep.op, Some(Operator::Equal));
                assert!(dep.glob);
            }
            _ => panic!("expected Atom"),
        }
    }

    // Test PMS glob version matching behavior
    #[test]
    fn test_glob_version_matching() {
        // PMS: =1.2* should match 1.2.3, 1.2.4, etc.
        let v1_2_star = Version::new(&[1, 2]);
        let v1_2_3 = Version::new(&[1, 2, 3]);
        let v1_2_4 = Version::new(&[1, 2, 4]);
        let v1_3 = Version::new(&[1, 3]);

        // PMS glob matching: 1.2* should match 1.2.3 and 1.2.4
        assert!(v1_2_3.glob_matches(&v1_2_star));
        assert!(v1_2_4.glob_matches(&v1_2_star));

        // But should not match 1.3
        assert!(!v1_3.glob_matches(&v1_2_star));
    }

    // Issue 4: Trailing comma in USE dep list
    #[test]
    fn test_atoms_with_trailing_comma_in_use_deps() {
        let entries =
            DepEntry::parse(">=app-accessibility/at-spi2-core-2.46.0[introspection?,]").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            DepEntry::Atom(dep) => {
                assert_eq!(dep.package(), "at-spi2-core");
                assert!(dep.version.is_some());
                let use_deps = dep.use_deps.as_ref().unwrap();
                assert_eq!(use_deps.len(), 1);
                assert_eq!(use_deps[0].flag, "introspection");
                assert_eq!(use_deps[0].kind, UseDepKind::Conditional);
            }
            _ => panic!("expected Atom"),
        }
    }

    // Issue 5: USE-conditional dep groups with whitespace handling
    #[test]
    fn test_use_conditional_with_whitespace() {
        let entries =
            DepEntry::parse("python_single_target_python3_11? ( dev-lang/python )").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            DepEntry::UseConditional {
                flag,
                negate,
                children,
            } => {
                assert_eq!(flag, "python_single_target_python3_11");
                assert!(!negate);
                assert_eq!(children.len(), 1);
            }
            _ => panic!("expected UseConditional"),
        }

        let entries = DepEntry::parse("test? ( dev-libs/check )").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            DepEntry::UseConditional {
                flag,
                negate,
                children,
            } => {
                assert_eq!(flag, "test");
                assert!(!negate);
                assert_eq!(children.len(), 1);
            }
            _ => panic!("expected UseConditional"),
        }

        let entries = DepEntry::parse("|| ( dev-libs/openssl dev-libs/libressl )").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            DepEntry::AnyOf(children) => {
                assert_eq!(children.len(), 2);
            }
            _ => panic!("expected AnyOf"),
        }

        let entries = DepEntry::parse("X? ( x11-libs/libX11 )").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            DepEntry::UseConditional {
                flag,
                negate,
                children,
            } => {
                assert_eq!(flag, "X");
                assert!(!negate);
                assert_eq!(children.len(), 1);
            }
            _ => panic!("expected UseConditional"),
        }
    }

    // --- PMS compliance tests ---

    #[test]
    fn test_use_flag_with_at_sign() {
        // PMS 3.1.4: USE flag names may contain [A-Za-z0-9+_@-]
        // @ is deprecated (was for LINGUAS) but still valid
        let entries = DepEntry::parse("foo@bar? ( dev-libs/openssl )").unwrap();
        match &entries[0] {
            DepEntry::UseConditional { flag, .. } => {
                assert_eq!(flag, "foo@bar");
            }
            _ => panic!("expected UseConditional"),
        }
    }

    #[test]
    fn test_is_use_conditional_discriminant() {
        // USE conditionals are identified by '? (' pattern
        assert!(is_use_conditional("ssl? ( dev-libs/openssl )"));
        assert!(is_use_conditional("!debug? ( dev-libs/bar )"));
        assert!(is_use_conditional("test?(\tdev-libs/x )"));
        assert!(is_use_conditional("X?( y )"));

        // Not USE conditionals — these are atoms
        assert!(!is_use_conditional("dev-libs/openssl"));
        assert!(!is_use_conditional("!dev-libs/old"));
        assert!(!is_use_conditional("!!dev-libs/old"));
        assert!(!is_use_conditional(">=dev-lang/rust-1.0"));
        assert!(!is_use_conditional("dev-libs/openssl:0"));
        assert!(!is_use_conditional("dev-libs/openssl[ssl]"));
    }

    #[test]
    fn test_deeply_nested() {
        // USE conditional inside any-of inside USE conditional
        let input = "ssl? ( || ( dev-libs/openssl !libressl? ( dev-libs/libressl ) ) )";
        let entries = DepEntry::parse(input).unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            DepEntry::UseConditional {
                flag,
                negate,
                children,
                ..
            } => {
                assert_eq!(flag, "ssl");
                assert!(!negate);
                assert_eq!(children.len(), 1);
                match &children[0] {
                    DepEntry::AnyOf(inner) => {
                        assert_eq!(inner.len(), 2);
                        assert!(matches!(&inner[0], DepEntry::Atom(_)));
                        match &inner[1] {
                            DepEntry::UseConditional { flag, negate, .. } => {
                                assert_eq!(flag, "libressl");
                                assert!(negate);
                            }
                            _ => panic!("expected UseConditional"),
                        }
                    }
                    _ => panic!("expected AnyOf"),
                }
            }
            _ => panic!("expected UseConditional"),
        }
    }

    // --- evaluate_use tests ---

    #[test]
    fn evaluate_use_active_conditional_included() {
        let entries = DepEntry::parse("ssl? ( dev-libs/openssl )").unwrap();
        let result = DepEntry::evaluate_use(&entries, &["ssl"]);
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], DepEntry::Atom(d) if d.package() == "openssl"));
    }

    #[test]
    fn evaluate_use_inactive_conditional_dropped() {
        let entries = DepEntry::parse("ssl? ( dev-libs/openssl )").unwrap();
        let result = DepEntry::evaluate_use(&entries, &[] as &[&str]);
        assert!(result.is_empty());
    }

    #[test]
    fn evaluate_use_negated_active_dropped() {
        let entries = DepEntry::parse("!debug? ( dev-libs/bar )").unwrap();
        let result = DepEntry::evaluate_use(&entries, &["debug"]);
        assert!(result.is_empty());
    }

    #[test]
    fn evaluate_use_negated_inactive_included() {
        let entries = DepEntry::parse("!debug? ( dev-libs/bar )").unwrap();
        let result = DepEntry::evaluate_use(&entries, &[] as &[&str]);
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], DepEntry::Atom(d) if d.package() == "bar"));
    }

    #[test]
    fn evaluate_use_inside_any_of() {
        // ssl active: AnyOf collapses to just openssl inside it
        let entries = DepEntry::parse("|| ( ssl? ( dev-libs/openssl ) dev-libs/gnutls )").unwrap();
        let result = DepEntry::evaluate_use(&entries, &["ssl"]);
        assert_eq!(result.len(), 1);
        match &result[0] {
            DepEntry::AnyOf(children) => {
                assert_eq!(children.len(), 2);
                assert!(matches!(&children[0], DepEntry::Atom(d) if d.package() == "openssl"));
                assert!(matches!(&children[1], DepEntry::Atom(d) if d.package() == "gnutls"));
            }
            _ => panic!("expected AnyOf"),
        }
    }

    #[test]
    fn evaluate_use_empty_any_of_dropped() {
        // EAPI 0–6 (table 8.6): empty || matches, so the group is dropped
        let entries = DepEntry::parse("|| ( ssl? ( dev-libs/openssl ) )").unwrap();
        let result = DepEntry::evaluate_use(&entries, &[] as &[&str]);
        assert!(result.is_empty());
    }

    #[test]
    fn evaluate_use_empty_any_of_kept_when_unmatched() {
        // EAPI 7–9: empty || does not match
        let entries = DepEntry::parse("|| ( ssl? ( dev-libs/openssl ) )").unwrap();
        let result = DepEntry::evaluate_use_groups(&entries, &[] as &[&str], false);
        assert!(matches!(&result[..], [DepEntry::AnyOf(c)] if c.is_empty()));
    }

    #[test]
    fn evaluate_use_atoms_pass_through() {
        let entries = DepEntry::parse("dev-lang/rust dev-libs/bar").unwrap();
        let result = DepEntry::evaluate_use(&entries, &["ssl"]);
        assert_eq!(result.len(), 2);
        assert!(matches!(&result[0], DepEntry::Atom(d) if d.package() == "rust"));
        assert!(matches!(&result[1], DepEntry::Atom(d) if d.package() == "bar"));
    }

    #[test]
    fn evaluate_use_matches_by_interned_key() {
        let entries = DepEntry::parse("ssl? ( dev-libs/openssl )").unwrap();
        let ssl = Interned::intern("ssl");
        let active: std::collections::HashSet<_> = [ssl].into();
        let result = DepEntry::evaluate_use(&entries, &active);
        assert_eq!(result.len(), 1);
        let inactive: std::collections::HashSet<Interned<_>> = HashSet::new();
        let result = DepEntry::evaluate_use(&entries, &inactive);
        assert!(result.is_empty());
    }

    #[test]
    fn test_dep_entry_round_trip_complex() {
        let inputs = [
            "dev-lang/rust dev-libs/bar",
            "|| ( dev-libs/openssl dev-libs/libressl )",
            "ssl? ( dev-libs/openssl ) !ssl? ( dev-libs/libressl )",
            "|| ( ssl? ( dev-libs/openssl ) dev-libs/gnutls )",
            "^^ ( dev-libs/a dev-libs/b ) ?? ( dev-libs/c dev-libs/d )",
            "!dev-libs/old !!dev-libs/older >=dev-lang/rust-1.75.0:0[ssl]::gentoo",
        ];
        for input in inputs {
            let entries = DepEntry::parse(input).unwrap();
            let displayed: Vec<String> = entries.iter().map(|e| e.to_string()).collect();
            let rejoined = displayed.join(" ");
            let reparsed = DepEntry::parse(&rejoined).unwrap();
            assert_eq!(entries, reparsed, "round-trip failed for: {input}");
        }
    }

    #[test]
    fn lazy_dep_list_parses_on_first_access_only() {
        let lazy = LazyDepList::from_raw("dev-lang/rust");
        assert!(!lazy.is_empty_raw());
        assert!(lazy.parsed.get().is_none(), "must not parse eagerly");
        assert_eq!(lazy.list().len(), 1);
        assert!(
            lazy.parsed.get().is_some(),
            "must memoize after first access"
        );
    }

    #[test]
    fn lazy_dep_list_empty_raw_never_forces() {
        let lazy = LazyDepList::from_raw("");
        assert!(lazy.is_empty_raw());
        assert!(lazy.parsed.get().is_none());
    }

    #[test]
    fn lazy_dep_list_malformed_field_is_empty_not_an_error() {
        // A syntactically invalid dep string parses to an empty list rather
        // than propagating an error or panicking.
        let lazy = LazyDepList::from_raw("( unterminated");
        assert!(lazy.list().is_empty());
    }

    #[test]
    fn lazy_dep_list_from_list_never_reparses() {
        let list = DepEntry::parse("dev-lang/rust").unwrap();
        let lazy = LazyDepList::from_list(list.clone().into());
        assert_eq!(lazy.list(), &DepList::from(list));
    }

    #[test]
    fn lazy_dep_list_clone_preserves_memoized_state() {
        let lazy = LazyDepList::from_raw("dev-lang/rust");
        lazy.list(); // force
        let cloned = lazy.clone();
        assert!(
            cloned.parsed.get().is_some(),
            "clone must carry the already-computed value, not re-parse lazily"
        );
        assert_eq!(lazy.list(), cloned.list());
    }

    #[test]
    fn lazy_dep_list_eq_compares_parsed_content() {
        // Equal content, different construction path (raw text vs. a
        // pre-built list) must still compare equal.
        let from_raw = LazyDepList::from_raw("dev-lang/rust");
        let from_list = LazyDepList::from_list(DepEntry::parse("dev-lang/rust").unwrap().into());
        assert_eq!(from_raw, from_list);

        let different = LazyDepList::from_raw("dev-libs/bar");
        assert_ne!(from_raw, different);
    }
}
