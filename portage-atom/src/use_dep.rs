use std::fmt;
use std::str::FromStr;

use gentoo_interner::{DefaultInterner, Interned};
use winnow::combinator::{alt, cut_err, delimited, opt, preceded, separated, terminated};
use winnow::error::StrContext;
use winnow::prelude::*;

use crate::error::{Error, Result};

/// Default value for a USE flag that is not defined by the dependency package
///
/// When a package does not define a particular USE flag in its IUSE, the
/// default annotation specifies what value the package manager should assume.
///
/// See [PMS 8.3.4](https://projects.gentoo.org/pms/9/pms.html#style-and-style-use-dependencies).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UseDefault {
    /// `(+)` — assume the flag is enabled if not defined by the package
    Enabled,
    /// `(-)` — assume the flag is disabled if not defined by the package
    Disabled,
}

impl fmt::Display for UseDefault {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            UseDefault::Enabled => write!(f, "(+)"),
            UseDefault::Disabled => write!(f, "(-)"),
        }
    }
}

/// The kind of constraint a USE dependency expresses
///
/// See [PMS 8.3.4](https://projects.gentoo.org/pms/9/pms.html#style-and-style-use-dependencies).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UseDepKind {
    /// `[flag]` — the dependency's flag must be enabled
    Enabled,
    /// `[-flag]` — the dependency's flag must be disabled
    Disabled,
    /// `[flag?]` — if the *parent's* flag is enabled, the dependency's flag
    /// must also be enabled; otherwise unconstrained
    Conditional,
    /// `[!flag?]` — if the *parent's* flag is disabled, the dependency's flag
    /// must be enabled; otherwise unconstrained
    ConditionalInverse,
    /// `[flag=]` — the dependency's flag must match the parent's flag state
    /// (both enabled or both disabled)
    Equal,
    /// `[!flag=]` — the dependency's flag must be the opposite of the
    /// parent's flag state
    EqualInverse,
}

impl fmt::Display for UseDepKind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            UseDepKind::Enabled => Ok(()),
            UseDepKind::Disabled => write!(f, "-"),
            UseDepKind::Conditional => write!(f, "?"),
            UseDepKind::ConditionalInverse => write!(f, "!?"),
            UseDepKind::Equal => write!(f, "="),
            UseDepKind::EqualInverse => write!(f, "!="),
        }
    }
}

/// A single USE flag constraint within a dependency atom
///
/// Appears inside brackets in dependency strings, e.g. `[ssl,-debug,python?]`.
/// Each `UseDep` constrains one flag on the dependency package, optionally
/// relative to the parent package's flag state.
///
/// See [PMS 8.3.4](https://projects.gentoo.org/pms/9/pms.html#style-and-style-use-dependencies).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "builder", derive(bon::Builder))]
pub struct UseDep {
    /// The USE flag name (e.g. `ssl`, `debug`, `python_targets_python3_12`)
    #[cfg_attr(feature = "builder", builder(into))]
    pub flag: Interned<DefaultInterner>,
    /// The kind of constraint this dependency expresses
    pub kind: UseDepKind,
    /// Optional default value (`(+)` or `(-)`) for when the flag is not
    /// defined by the dependency package
    pub default: Option<UseDefault>,
}

impl UseDep {
    /// Create a new USE dependency without a default annotation
    ///
    /// The flag name is interned automatically.
    pub fn new(flag: impl AsRef<str>, kind: UseDepKind) -> Self {
        UseDep {
            flag: Interned::intern(flag.as_ref()),
            kind,
            default: None,
        }
    }

    /// Create a new USE dependency with a default annotation (`(+)` or `(-)`)
    ///
    /// The flag name is interned automatically.
    pub fn with_default(flag: impl AsRef<str>, kind: UseDepKind, default: UseDefault) -> Self {
        UseDep {
            flag: Interned::intern(flag.as_ref()),
            kind,
            default: Some(default),
        }
    }

    /// Parse a single USE dependency (without surrounding brackets)
    ///
    /// Accepts forms like `ssl`, `-debug`, `python?`, `!flag=`, `ssl(+)`.
    pub fn parse(input: &str) -> Result<Self> {
        parse_use_dep_item
            .parse(input)
            .map_err(|e| Error::InvalidUseDep(format!("{}: {}", input, e)))
    }
}

impl fmt::Display for UseDep {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.kind {
            UseDepKind::Disabled => write!(f, "-")?,
            UseDepKind::ConditionalInverse | UseDepKind::EqualInverse => write!(f, "!")?,
            _ => {}
        }

        write!(f, "{}", self.flag)?;

        // PMS 8.3.4: default immediately follows the flag name, before ?/=
        if let Some(default) = self.default {
            write!(f, "{}", default)?;
        }

        match self.kind {
            UseDepKind::Conditional | UseDepKind::ConditionalInverse => write!(f, "?")?,
            UseDepKind::Equal | UseDepKind::EqualInverse => write!(f, "=")?,
            _ => {}
        }

        Ok(())
    }
}

impl PartialOrd for UseDep {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for UseDep {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.flag.cmp(&other.flag)
    }
}

impl FromStr for UseDep {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

// Winnow parsers

/// Parse USE flag name
///
/// PMS 3.1.4: must begin with alphanumeric character.
fn parse_use_flag(input: &mut &str) -> ModalResult<Interned<DefaultInterner>> {
    use crate::parsers::parse_ident_with_at;

    parse_ident_with_at
        .verify(|s: &str| s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()))
        .map(|s: &str| Interned::intern(s))
        .parse_next(input)
}

/// Parse USE default
fn parse_use_default(input: &mut &str) -> ModalResult<UseDefault> {
    alt((
        "(+)".value(UseDefault::Enabled),
        "(-)".value(UseDefault::Disabled),
    ))
    .parse_next(input)
}

/// Parse single USE dependency item
pub(crate) fn parse_use_dep_item(input: &mut &str) -> ModalResult<UseDep> {
    alt((
        // !flag? - inverse conditional
        (preceded('!', parse_use_flag), opt(parse_use_default), '?').map(|(flag, default, _)| {
            UseDep {
                flag,
                kind: UseDepKind::ConditionalInverse,
                default,
            }
        }),
        // !flag= - inverse equal
        (preceded('!', parse_use_flag), opt(parse_use_default), '=').map(|(flag, default, _)| {
            UseDep {
                flag,
                kind: UseDepKind::EqualInverse,
                default,
            }
        }),
        // -flag - disabled
        (preceded('-', parse_use_flag), opt(parse_use_default)).map(|(flag, default)| UseDep {
            flag,
            kind: UseDepKind::Disabled,
            default,
        }),
        // flag? - conditional
        (parse_use_flag, opt(parse_use_default), '?').map(|(flag, default, _)| UseDep {
            flag,
            kind: UseDepKind::Conditional,
            default,
        }),
        // flag= - equal
        (parse_use_flag, opt(parse_use_default), '=').map(|(flag, default, _)| UseDep {
            flag,
            kind: UseDepKind::Equal,
            default,
        }),
        // flag - enabled
        (parse_use_flag, opt(parse_use_default)).map(|(flag, default)| UseDep {
            flag,
            kind: UseDepKind::Enabled,
            default,
        }),
    ))
    .parse_next(input)
}

/// Parse USE dependencies (with brackets)
pub(crate) fn parse_use_deps(input: &mut &str) -> ModalResult<Vec<UseDep>> {
    delimited(
        '[',
        cut_err(terminated(
            separated(0.., parse_use_dep_item, ','),
            opt(','),
        )),
        cut_err(']'),
    )
    .context(StrContext::Label("use deps"))
    .parse_next(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_use_dep_enabled() {
        let dep = UseDep::parse("ssl").unwrap();
        assert_eq!(dep.flag, "ssl");
        assert_eq!(dep.kind, UseDepKind::Enabled);
        assert_eq!(dep.to_string(), "ssl");
    }

    #[test]
    fn test_use_dep_disabled() {
        let dep = UseDep::parse("-debug").unwrap();
        assert_eq!(dep.flag, "debug");
        assert_eq!(dep.kind, UseDepKind::Disabled);
        assert_eq!(dep.to_string(), "-debug");
    }

    #[test]
    fn test_use_dep_conditional() {
        let dep = UseDep::parse("python?").unwrap();
        assert_eq!(dep.flag, "python");
        assert_eq!(dep.kind, UseDepKind::Conditional);
        assert_eq!(dep.to_string(), "python?");
    }

    #[test]
    fn test_use_dep_with_default() {
        let dep = UseDep::parse("ssl(+)").unwrap();
        assert_eq!(dep.flag, "ssl");
        assert_eq!(dep.kind, UseDepKind::Enabled);
        assert_eq!(dep.default, Some(UseDefault::Enabled));
        assert_eq!(dep.to_string(), "ssl(+)");
    }

    #[test]
    fn test_use_deps_list() {
        let deps = parse_use_deps.parse("[ssl,-debug,python?]").unwrap();
        assert_eq!(deps.len(), 3);
        assert_eq!(deps[0].flag, "ssl");
        assert_eq!(deps[1].flag, "debug");
        assert_eq!(deps[1].kind, UseDepKind::Disabled);
        assert_eq!(deps[2].flag, "python");
        assert_eq!(deps[2].kind, UseDepKind::Conditional);
    }

    // Issue 1: Empty USE dep brackets []
    #[test]
    fn test_empty_use_deps() {
        let deps = parse_use_deps.parse("[]").unwrap();
        assert!(deps.is_empty());
    }

    // Issue 2: USE dep defaults (+) and (-)
    #[test]
    fn test_use_dep_with_defaults() {
        let dep = UseDep::parse("unicode(+)").unwrap();
        assert_eq!(dep.flag, "unicode");
        assert_eq!(dep.kind, UseDepKind::Enabled);
        assert_eq!(dep.default, Some(UseDefault::Enabled));
        assert_eq!(dep.to_string(), "unicode(+)");

        let dep = UseDep::parse("unicode(-)").unwrap();
        assert_eq!(dep.flag, "unicode");
        assert_eq!(dep.kind, UseDepKind::Enabled);
        assert_eq!(dep.default, Some(UseDefault::Disabled));
        assert_eq!(dep.to_string(), "unicode(-)");

        let dep = UseDep::parse("icu(+)").unwrap();
        assert_eq!(dep.flag, "icu");
        assert_eq!(dep.kind, UseDepKind::Enabled);
        assert_eq!(dep.default, Some(UseDefault::Enabled));
        assert_eq!(dep.to_string(), "icu(+)");
    }

    // Issue 4: Trailing comma in USE dep list
    #[test]
    fn test_use_deps_with_trailing_comma() {
        let deps = parse_use_deps.parse("[introspection?,]").unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].flag, "introspection");
        assert_eq!(deps[0].kind, UseDepKind::Conditional);
    }

    #[test]
    fn test_use_flag_name_validation() {
        // PMS 3.1.4: "A USE flag name may contain any of the characters [A-Za-z0-9+_@-].
        // It must begin with an alphanumeric character."

        // Valid flag names
        assert!(UseDep::parse("ssl").is_ok());
        assert!(UseDep::parse("python_targets_python3_12").is_ok());
        assert!(UseDep::parse("unicode+").is_ok());
        assert!(UseDep::parse("icu@").is_ok());
        assert!(UseDep::parse("-flag").is_ok()); // -flag is valid: flag starts with 'f' (alphanumeric)

        // Invalid: flag name itself starts with non-alphanumeric
        assert!(UseDep::parse("@flag").is_err()); // @flag is invalid: flag starts with '@'
        assert!(UseDep::parse("-\u{40}flag").is_err()); // -@flag is invalid: flag starts with '@'
        assert!(UseDep::parse("-_flag").is_err()); // -_flag is invalid: flag starts with '_'
    }

    #[test]
    #[cfg(feature = "builder")]
    fn test_use_dep_builder() {
        let dep = UseDep::builder()
            .flag("ssl")
            .kind(UseDepKind::Enabled)
            .default(UseDefault::Enabled)
            .build();
        assert_eq!(dep.flag, "ssl");
        assert_eq!(dep.kind, UseDepKind::Enabled);
        assert_eq!(dep.default, Some(UseDefault::Enabled));
        assert_eq!(dep.to_string(), "ssl(+)");
    }
}
