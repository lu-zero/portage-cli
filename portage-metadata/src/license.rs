use std::fmt;

use winnow::ascii::multispace0;
use winnow::combinator::{alt, cut_err, delimited, dispatch, opt, peek, preceded, repeat};
use winnow::error::StrContext;
use winnow::prelude::*;
use winnow::token::{any, take_while};

use crate::error::{Error, Result};

/// A node in a `LICENSE` expression tree.
///
/// The `LICENSE` variable uses a dependency-specification-like grammar
/// with `||` (any-of) groups and USE-conditional groups.
///
/// See [PMS 7.2](https://projects.gentoo.org/pms/9/pms.html#mandatory-ebuilddefined-variables)
/// and [PMS 8.2](https://projects.gentoo.org/pms/9/pms.html#dependency-specification-format).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LicenseExpr {
    /// A single license identifier (e.g. `MIT`, `GPL-2+`).
    License(String),
    /// `|| ( license1 license2 ... )` — any one license is acceptable.
    AnyOf(Vec<LicenseExpr>),
    /// `flag? ( licenses... )` or `!flag? ( licenses... )` conditional group.
    UseConditional {
        /// USE flag name.
        flag: String,
        /// `true` for `!flag?` (negated conditional).
        negated: bool,
        /// License entries guarded by this flag.
        entries: Vec<LicenseExpr>,
    },
    /// Top-level grouping: all listed licenses apply.
    All(Vec<LicenseExpr>),
}

impl LicenseExpr {
    /// Parse a `LICENSE` expression string.
    ///
    /// # Examples
    ///
    /// ```
    /// use portage_metadata::LicenseExpr;
    ///
    /// let expr = LicenseExpr::parse("|| ( MIT Apache-2.0 )").unwrap();
    /// assert!(matches!(expr, LicenseExpr::AnyOf(_)));
    ///
    /// let expr = LicenseExpr::parse("GPL-2+").unwrap();
    /// assert!(matches!(expr, LicenseExpr::License(_)));
    /// ```
    pub fn parse(input: &str) -> Result<Self> {
        let entries: Vec<LicenseExpr> = parse_license_string.parse(input).map_err(|e| {
            Error::InvalidLicense(crate::diagnostic::ParseDiagnostic::from_winnow(
                "LICENSE", e,
            ))
        })?;

        Ok(match entries.len() {
            0 => LicenseExpr::All(Vec::new()),
            1 => entries.into_iter().next().unwrap(),
            _ => LicenseExpr::All(entries),
        })
    }

    /// Return a copy with duplicate entries removed at every level (first occurrence wins).
    pub fn dedup(&self) -> Self {
        match self {
            LicenseExpr::License(_) => self.clone(),
            LicenseExpr::All(children) => LicenseExpr::All(dedup_license_children(children)),
            LicenseExpr::AnyOf(children) => LicenseExpr::AnyOf(dedup_license_children(children)),
            LicenseExpr::UseConditional {
                flag,
                negated,
                entries,
            } => LicenseExpr::UseConditional {
                flag: flag.clone(),
                negated: *negated,
                entries: dedup_license_children(entries),
            },
        }
    }
}

fn dedup_license_children(children: &[LicenseExpr]) -> Vec<LicenseExpr> {
    let mut result: Vec<LicenseExpr> = Vec::with_capacity(children.len());
    for child in children {
        if !result.contains(child) {
            result.push(child.dedup());
        }
    }
    result
}

impl fmt::Display for LicenseExpr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            LicenseExpr::License(name) => write!(f, "{name}"),
            LicenseExpr::AnyOf(entries) => {
                write!(f, "|| ( ")?;
                for (i, entry) in entries.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{entry}")?;
                }
                write!(f, " )")
            }
            LicenseExpr::UseConditional {
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
            LicenseExpr::All(entries) => {
                for (i, entry) in entries.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{entry}")?;
                }
                Ok(())
            }
        }
    }
}

// Winnow parsers

fn is_license_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+')
}

fn is_flag_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '+' || c == '@'
}

fn parse_license_name(input: &mut &str) -> ModalResult<LicenseExpr> {
    take_while(1.., is_license_char)
        .verify(|name: &str| {
            // Validate license name according to PMS 3.1.7
            !name.starts_with(['-', '.', '+'])
        })
        .map(|name: &str| LicenseExpr::License(name.to_string()))
        .parse_next(input)
}

fn parse_any_of(input: &mut &str) -> ModalResult<LicenseExpr> {
    preceded(
        "||",
        preceded(
            multispace0,
            cut_err(delimited('(', parse_license_entries, (multispace0, ')')))
                .context(StrContext::Label("'||' group")),
        ),
    )
    .map(LicenseExpr::AnyOf)
    .parse_next(input)
}

fn parse_use_conditional(input: &mut &str) -> ModalResult<LicenseExpr> {
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
    let entries = cut_err(delimited('(', parse_license_entries, (multispace0, ')')))
        .context(StrContext::Label("USE conditional group"))
        .parse_next(input)?;
    Ok(LicenseExpr::UseConditional {
        flag,
        negated,
        entries,
    })
}

fn parse_paren_group(input: &mut &str) -> ModalResult<Vec<LicenseExpr>> {
    delimited(
        '(',
        parse_license_entries,
        cut_err((multispace0, ')')).context(StrContext::Label("closing ')'")),
    )
    .parse_next(input)
}

fn parse_license_entry(input: &mut &str) -> ModalResult<Vec<LicenseExpr>> {
    dispatch! {peek(any);
        '|' => parse_any_of.map(|e| vec![e]),
        '(' => parse_paren_group,
        _ => alt((
            parse_use_conditional.map(|e| vec![e]),
            parse_license_name.map(|e| vec![e]),
        )),
    }
    .parse_next(input)
}

fn parse_license_entries(input: &mut &str) -> ModalResult<Vec<LicenseExpr>> {
    repeat(0.., preceded(multispace0, parse_license_entry))
        .fold(
            Vec::new,
            |mut acc: Vec<LicenseExpr>, batch: Vec<LicenseExpr>| {
                acc.extend(batch);
                acc
            },
        )
        .parse_next(input)
}

pub(crate) fn parse_license_string(input: &mut &str) -> ModalResult<Vec<LicenseExpr>> {
    let entries = parse_license_entries(input)?;
    multispace0.parse_next(input)?;
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_license() {
        let expr = LicenseExpr::parse("MIT").unwrap();
        assert_eq!(expr, LicenseExpr::License("MIT".to_string()));
    }

    #[test]
    fn parse_multiple_licenses() {
        let expr = LicenseExpr::parse("MIT BSD-2").unwrap();
        match expr {
            LicenseExpr::All(entries) => {
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0], LicenseExpr::License("MIT".to_string()));
                assert_eq!(entries[1], LicenseExpr::License("BSD-2".to_string()));
            }
            _ => unreachable!("expected All"),
        }
    }

    #[test]
    fn parse_any_of() {
        let expr = LicenseExpr::parse("|| ( MIT Apache-2.0 )").unwrap();
        match expr {
            LicenseExpr::AnyOf(entries) => {
                assert_eq!(entries.len(), 2);
            }
            _ => unreachable!("expected AnyOf"),
        }
    }

    #[test]
    fn parse_use_conditional() {
        let expr = LicenseExpr::parse("ssl? ( OpenSSL )").unwrap();
        match expr {
            LicenseExpr::UseConditional {
                flag,
                negated,
                entries,
            } => {
                assert_eq!(flag, "ssl");
                assert!(!negated);
                assert_eq!(entries.len(), 1);
            }
            _ => unreachable!("expected UseConditional"),
        }
    }

    #[test]
    fn parse_complex() {
        let expr = LicenseExpr::parse("Apache-2.0-with-LLVM-exceptions UoI-NCSA").unwrap();
        match expr {
            LicenseExpr::All(entries) => {
                assert_eq!(entries.len(), 2);
                assert_eq!(
                    entries[0],
                    LicenseExpr::License("Apache-2.0-with-LLVM-exceptions".to_string())
                );
                assert_eq!(entries[1], LicenseExpr::License("UoI-NCSA".to_string()));
            }
            _ => unreachable!("expected All"),
        }
    }

    #[test]
    fn parse_empty() {
        let expr = LicenseExpr::parse("").unwrap();
        assert_eq!(expr, LicenseExpr::All(Vec::new()));
    }

    #[test]
    fn display_single() {
        let expr = LicenseExpr::License("MIT".to_string());
        assert_eq!(expr.to_string(), "MIT");
    }

    #[test]
    fn display_any_of() {
        let expr = LicenseExpr::AnyOf(vec![
            LicenseExpr::License("MIT".to_string()),
            LicenseExpr::License("Apache-2.0".to_string()),
        ]);
        assert_eq!(expr.to_string(), "|| ( MIT Apache-2.0 )");
    }

    #[test]
    fn display_round_trip() {
        let input = "|| ( MIT Apache-2.0 )";
        let expr = LicenseExpr::parse(input).unwrap();
        let reparsed = LicenseExpr::parse(&expr.to_string()).unwrap();
        assert_eq!(expr, reparsed);
    }

    #[test]
    fn invalid_license_starting_with_dot() {
        assert!(LicenseExpr::parse(".license").is_err());
    }

    #[test]
    fn invalid_license_starting_with_hyphen() {
        assert!(LicenseExpr::parse("-GPL").is_err());
    }

    #[test]
    fn invalid_license_starting_with_plus() {
        assert!(LicenseExpr::parse("+MIT").is_err());
    }

    #[test]
    fn valid_license_with_underscore() {
        let expr = LicenseExpr::parse("MIT_with_underscore").unwrap();
        assert_eq!(
            expr,
            LicenseExpr::License("MIT_with_underscore".to_string())
        );
    }

    #[test]
    fn valid_license_with_hyphen_not_first() {
        let expr = LicenseExpr::parse("GPL-2+").unwrap();
        assert_eq!(expr, LicenseExpr::License("GPL-2+".to_string()));
    }

    #[test]
    fn valid_use_conditional_with_at() {
        let expr = LicenseExpr::parse("flag@name? ( MIT )").unwrap();
        match expr {
            LicenseExpr::UseConditional {
                flag,
                negated,
                entries,
            } => {
                assert_eq!(flag, "flag@name");
                assert!(!negated);
                assert_eq!(entries.len(), 1);
            }
            _ => unreachable!("expected UseConditional"),
        }
    }

    #[test]
    fn invalid_use_conditional_flag_starting_with_at() {
        assert!(LicenseExpr::parse("@flag? ( MIT )").is_err());
    }
}
