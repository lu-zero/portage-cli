use std::fmt;

use winnow::ascii::multispace0;
use winnow::combinator::{alt, cut_err, delimited, dispatch, opt, peek, preceded, repeat};
use winnow::error::StrContext;
use winnow::prelude::*;
use winnow::token::{any, take_while};

use crate::error::{Error, Result};

/// A node in a `RESTRICT` or `PROPERTIES` expression.
///
/// Before EAPI 8, these are simple space-separated token lists.
/// In EAPI 8, they support USE-conditional groups (`flag? ( ... )`).
///
/// See [PMS 7.3.6](https://projects.gentoo.org/pms/9/pms.html#restrict).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestrictExpr {
    /// A single restriction/property token (e.g. `mirror`, `test`, `live`).
    Token(String),
    /// `flag? ( ... )` or `!flag? ( ... )` conditional group (EAPI 8+).
    UseConditional {
        /// USE flag name.
        flag: String,
        /// `true` for `!flag?` (negated conditional).
        negated: bool,
        /// Entries guarded by this flag.
        entries: Vec<RestrictExpr>,
    },
}

impl RestrictExpr {
    /// Parse a `RESTRICT` or `PROPERTIES` expression string.
    ///
    /// Handles both the simple space-separated format (EAPI <8) and
    /// the USE-conditional format (EAPI 8).
    ///
    /// # Examples
    ///
    /// ```
    /// use portage_metadata::RestrictExpr;
    ///
    /// // Simple tokens
    /// let entries = RestrictExpr::parse("mirror test").unwrap();
    /// assert_eq!(entries.len(), 2);
    ///
    /// // USE-conditional (EAPI 8)
    /// let entries = RestrictExpr::parse("!test? ( test )").unwrap();
    /// assert_eq!(entries.len(), 1);
    /// ```
    pub fn parse(input: &str) -> Result<Vec<RestrictExpr>> {
        parse_restrict_string.parse(input).map_err(|e| {
            Error::InvalidRestrict(crate::diagnostic::ParseDiagnostic::from_winnow(
                "RESTRICT/PROPERTIES",
                e,
            ))
        })
    }

    /// Collect all plain token values, ignoring USE-conditional structure.
    ///
    /// Useful for simple queries like "does RESTRICT contain `test`?"
    /// when you don't need to evaluate USE conditions.
    pub fn flat_tokens(entries: &[RestrictExpr]) -> Vec<&str> {
        let mut out = Vec::new();
        for entry in entries {
            match entry {
                RestrictExpr::Token(t) => out.push(t.as_str()),
                RestrictExpr::UseConditional { entries, .. } => {
                    out.extend(Self::flat_tokens(entries));
                }
            }
        }
        out
    }

    /// Tokens that apply **unconditionally** — every USE-conditional group
    /// dropped, `flag?` and `!flag?` alike.
    ///
    /// This is portage's `use_reduce(restrict, flat=True, matchnone=True)`
    /// (`portage/dep/__init__.py`): the `matchnone` docstring reads "Treat
    /// all conditionals as inactive," and `is_active()` returns `False` for
    /// *every* conditional under it, negated included — this is not "treat
    /// every USE flag as unset" (a `!flag?` under that reading would still
    /// apply), it is "no conditional ever applies, full stop."
    ///
    /// Server-side tools (`emirrordist`) use this so that no client's
    /// particular USE selection can change what gets mirrored: only plain
    /// tokens apply. A package with `RESTRICT="mirror"` is restricted; one
    /// with only `RESTRICT="!test? ( mirror )"` is **not** — matchnone drops
    /// every conditional (including negated ones), same as real portage.
    ///
    /// No recursion needed: [`RestrictExpr`] has no `Group` variant — bare
    /// parens are already flattened into the top-level slice by the parser
    /// — so a top-level [`RestrictExpr::UseConditional`] is dropped whole,
    /// its nested content included or not.
    ///
    /// Contrast [`Self::flat_tokens`], which flattens conditional bodies
    /// **into** the result regardless of the flag — the opposite bias, and
    /// wrong for this use.
    ///
    /// ```
    /// use portage_metadata::RestrictExpr;
    ///
    /// let entries = RestrictExpr::parse("mirror !test? ( fetch )").unwrap();
    /// assert_eq!(RestrictExpr::unconditional_tokens(&entries), vec!["mirror"]);
    /// ```
    pub fn unconditional_tokens(entries: &[RestrictExpr]) -> Vec<&str> {
        entries
            .iter()
            .filter_map(|e| match e {
                RestrictExpr::Token(t) => Some(t.as_str()),
                RestrictExpr::UseConditional { .. } => None,
            })
            .collect()
    }

    /// Whether `token` applies unconditionally. See [`Self::unconditional_tokens`].
    pub fn has_unconditional(entries: &[RestrictExpr], token: &str) -> bool {
        entries
            .iter()
            .any(|e| matches!(e, RestrictExpr::Token(t) if t == token))
    }
}

impl fmt::Display for RestrictExpr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            RestrictExpr::Token(t) => write!(f, "{t}"),
            RestrictExpr::UseConditional {
                flag,
                negated,
                entries,
            } => {
                if *negated {
                    write!(f, "!")?;
                }
                write!(f, "{flag}? ( ")?;
                for (i, entry) in entries.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{entry}")?;
                }
                write!(f, " )")
            }
        }
    }
}

// Winnow parsers

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+')
}

fn is_flag_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '+' || c == '@'
}

fn parse_token(input: &mut &str) -> ModalResult<RestrictExpr> {
    take_while(1.., is_token_char)
        .map(|s: &str| RestrictExpr::Token(s.to_string()))
        .parse_next(input)
}

fn parse_use_conditional(input: &mut &str) -> ModalResult<RestrictExpr> {
    let negated = opt('!').parse_next(input)?.is_some();
    let flag: String = take_while(1.., is_flag_char)
        .verify(|name: &str| {
            name.chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric())
        })
        .map(|s: &str| s.to_string())
        .parse_next(input)?;
    '?'.parse_next(input)?;
    multispace0.parse_next(input)?;
    let entries = cut_err(delimited('(', parse_restrict_entries, (multispace0, ')')))
        .context(StrContext::Label("USE conditional group"))
        .parse_next(input)?;
    Ok(RestrictExpr::UseConditional {
        flag,
        negated,
        entries,
    })
}

fn parse_restrict_entry(input: &mut &str) -> ModalResult<RestrictExpr> {
    dispatch! {peek(any);
        _ => alt((
            parse_use_conditional,
            parse_token,
        )),
    }
    .parse_next(input)
}

fn parse_paren_or_entry(input: &mut &str) -> ModalResult<Vec<RestrictExpr>> {
    dispatch! {peek(any);
        '(' => cut_err(delimited('(', parse_restrict_entries, (multispace0, ')')))
            .context(StrContext::Label("paren group")),
        _ => parse_restrict_entry.map(|e| vec![e]),
    }
    .parse_next(input)
}

fn parse_restrict_entries(input: &mut &str) -> ModalResult<Vec<RestrictExpr>> {
    repeat(0.., preceded(multispace0, parse_paren_or_entry))
        .map(|vecs: Vec<Vec<RestrictExpr>>| vecs.into_iter().flatten().collect())
        .parse_next(input)
}

pub(crate) fn parse_restrict_string(input: &mut &str) -> ModalResult<Vec<RestrictExpr>> {
    let entries = parse_restrict_entries(input)?;
    multispace0.parse_next(input)?;
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_tokens() {
        let entries = RestrictExpr::parse("mirror test").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], RestrictExpr::Token("mirror".to_string()));
        assert_eq!(entries[1], RestrictExpr::Token("test".to_string()));
    }

    #[test]
    fn parse_use_conditional() {
        let entries = RestrictExpr::parse("!test? ( test )").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            RestrictExpr::UseConditional {
                flag,
                negated,
                entries,
            } => {
                assert_eq!(flag, "test");
                assert!(negated);
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0], RestrictExpr::Token("test".to_string()));
            }
            _ => unreachable!("expected UseConditional"),
        }
    }

    #[test]
    fn parse_mixed() {
        let entries = RestrictExpr::parse("mirror !test? ( test )").unwrap();
        assert_eq!(entries.len(), 2);
        assert!(matches!(&entries[0], RestrictExpr::Token(t) if t == "mirror"));
        assert!(matches!(&entries[1], RestrictExpr::UseConditional { .. }));
    }

    #[test]
    fn parse_empty() {
        let entries = RestrictExpr::parse("").unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn flat_tokens() {
        let entries = RestrictExpr::parse("mirror !test? ( test )").unwrap();
        let tokens = RestrictExpr::flat_tokens(&entries);
        assert_eq!(tokens, vec!["mirror", "test"]);
    }

    #[test]
    fn display_token() {
        let entry = RestrictExpr::Token("test".to_string());
        assert_eq!(entry.to_string(), "test");
    }

    #[test]
    fn display_conditional() {
        let entry = RestrictExpr::UseConditional {
            flag: "test".to_string(),
            negated: true,
            entries: vec![RestrictExpr::Token("test".to_string())],
        };
        assert_eq!(entry.to_string(), "!test? ( test )");
    }

    #[test]
    fn parse_bare_paren_single() {
        let entries = RestrictExpr::parse("( test )").unwrap();
        assert_eq!(entries, vec![RestrictExpr::Token("test".to_string())]);
    }

    #[test]
    fn parse_bare_paren_multi() {
        let entries = RestrictExpr::parse("( mirror test )").unwrap();
        assert_eq!(
            entries,
            vec![
                RestrictExpr::Token("mirror".to_string()),
                RestrictExpr::Token("test".to_string()),
            ]
        );
    }

    #[test]
    fn parse_bare_paren_round_trip() {
        let input = "( mirror test )";
        let entries = RestrictExpr::parse(input).unwrap();
        let displayed: Vec<String> = entries.iter().map(|e| e.to_string()).collect();
        let rejoined = displayed.join(" ");
        assert_eq!(rejoined, "mirror test");
        let reparsed = RestrictExpr::parse(&rejoined).unwrap();
        assert_eq!(entries, reparsed);
    }

    #[test]
    fn display_round_trip() {
        let input = "!test? ( test )";
        let entries = RestrictExpr::parse(input).unwrap();
        let displayed: Vec<String> = entries.iter().map(|e| e.to_string()).collect();
        let rejoined = displayed.join(" ");
        let reparsed = RestrictExpr::parse(&rejoined).unwrap();
        assert_eq!(entries, reparsed);
    }

    #[test]
    fn use_conditional_flag_with_at_sign() {
        // python_targets_python3_11@std is a real-world flag name pattern
        let entries = RestrictExpr::parse("python_targets_python3_11@std? ( test )").unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            RestrictExpr::UseConditional { flag, negated, .. } => {
                assert_eq!(flag, "python_targets_python3_11@std");
                assert!(!negated);
            }
            _ => unreachable!("expected UseConditional"),
        }
    }

    #[test]
    fn invalid_use_conditional_flag_starting_with_at() {
        assert!(RestrictExpr::parse("@flag? ( test )").is_err());
    }

    #[test]
    fn unconditional_tokens_keeps_only_plain_tokens() {
        let entries = RestrictExpr::parse("mirror bindist? ( fetch )").unwrap();
        assert_eq!(RestrictExpr::unconditional_tokens(&entries), vec!["mirror"]);
    }

    /// The matchnone regression: a *negated* conditional is dropped too,
    /// not included — matchnone means "no conditional ever applies," not
    /// "treat every flag as unset" (which would make `!flag?` apply).
    #[test]
    fn unconditional_tokens_drops_negated_conditionals_too() {
        let entries = RestrictExpr::parse("!test? ( mirror )").unwrap();
        assert!(RestrictExpr::unconditional_tokens(&entries).is_empty());
    }

    #[test]
    fn has_unconditional_true_for_plain_token() {
        let entries = RestrictExpr::parse("mirror test").unwrap();
        assert!(RestrictExpr::has_unconditional(&entries, "mirror"));
        assert!(!RestrictExpr::has_unconditional(&entries, "fetch"));
    }

    #[test]
    fn has_unconditional_false_for_any_conditional_token() {
        let entries = RestrictExpr::parse("bindist? ( fetch ) !live? ( fetch )").unwrap();
        assert!(!RestrictExpr::has_unconditional(&entries, "fetch"));
    }
}
