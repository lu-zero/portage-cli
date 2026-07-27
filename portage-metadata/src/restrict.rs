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
            Error::InvalidRestrict(crate::diagnostic::render("RESTRICT/PROPERTIES", e))
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
}
