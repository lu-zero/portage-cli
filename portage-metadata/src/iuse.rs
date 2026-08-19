use std::fmt;
use std::str::FromStr;

use crate::interner::{DefaultInterner, Interned, Interner};

use crate::error::{Error, Result};

/// Default state for an IUSE flag
///
/// Flags may be prefixed with `+` (enabled by default) or `-` (disabled by
/// default) in the `IUSE` variable.
///
/// See [PMS 7.2](https://projects.gentoo.org/pms/9/pms.html#mandatory-ebuilddefined-variables).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IUseDefault {
    /// `+flag` — enabled by default
    Enabled,
    /// `-flag` — disabled by default
    Disabled,
}

/// A single USE flag entry from the `IUSE` variable
///
/// See [PMS 7.2](https://projects.gentoo.org/pms/9/pms.html#mandatory-ebuilddefined-variables).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IUse<I = DefaultInterner>
where
    I: Interner,
{
    /// Interned USE flag name (without prefix)
    name: <I as Interner>::Key,
    /// Optional default state prefix (`+` or `-`)
    pub default: Option<IUseDefault>,
}

fn is_valid_use_flag_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'+' | b'@' | b'-'))
}

impl<I: Interner> IUse<I> {
    /// The interned USE flag name (without `+`/`-` prefix)
    pub fn flag(&self) -> &<I as Interner>::Key {
        &self.name
    }

    /// The USE flag name
    pub fn name(&self) -> &str {
        I::resolve(&self.name)
    }

    /// Whether this flag carries a `+` (enabled) IUSE default — i.e. it is
    /// on unless the configuration turns it off.
    ///
    /// Used when computing a package's effective build USE.
    pub fn is_enabled_default(&self) -> bool {
        self.default == Some(IUseDefault::Enabled)
    }

    /// Parse a single IUSE token
    pub fn parse(s: &str) -> Result<Self> {
        if s.is_empty() {
            return Err(Error::InvalidIUse("empty IUSE entry".to_string()));
        }

        if let Some(name) = s.strip_prefix('+') {
            if name.is_empty() || !is_valid_use_flag_name(name) {
                return Err(Error::InvalidIUse(s.to_string()));
            }
            Ok(IUse {
                name: I::get_or_intern(name),
                default: Some(IUseDefault::Enabled),
            })
        } else if let Some(name) = s.strip_prefix('-') {
            if name.is_empty() || !is_valid_use_flag_name(name) {
                return Err(Error::InvalidIUse(s.to_string()));
            }
            Ok(IUse {
                name: I::get_or_intern(name),
                default: Some(IUseDefault::Disabled),
            })
        } else {
            if !is_valid_use_flag_name(s) {
                return Err(Error::InvalidIUse(s.to_string()));
            }
            Ok(IUse {
                name: I::get_or_intern(s),
                default: None,
            })
        }
    }
}

/// The flag as an [`Interned`] handle — wraps the key stored at parse time,
/// not a fresh intern lookup.
///
/// Prefer `Interned::from(iu)`/`iu.into()` over `Interned::intern(iu.name())`:
/// the latter round-trips through `resolve` (key → `&str`) and
/// `get_or_intern` (`&str` → key) to arrive back at the same key, paying
/// two interner lookups for a value already in hand.
impl From<&IUse<DefaultInterner>> for Interned<DefaultInterner> {
    fn from(iu: &IUse<DefaultInterner>) -> Self {
        Interned::from_key(iu.name)
    }
}

impl IUse<DefaultInterner> {
    /// Parse a space-separated `IUSE` line into a list of flags
    ///
    /// # Examples
    ///
    /// ```
    /// use portage_metadata::{IUse, IUseDefault};
    ///
    /// let flags = IUse::parse_line("+ssl -debug test").unwrap();
    /// assert_eq!(flags.len(), 3);
    /// assert_eq!(flags[0].name(), "ssl");
    /// assert_eq!(flags[0].default, Some(IUseDefault::Enabled));
    /// assert_eq!(flags[1].name(), "debug");
    /// assert_eq!(flags[1].default, Some(IUseDefault::Disabled));
    /// assert_eq!(flags[2].name(), "test");
    /// assert_eq!(flags[2].default, None);
    /// ```
    pub fn parse_line(input: &str) -> Result<Vec<Self>> {
        input.split_whitespace().map(Self::parse).collect()
    }
}

impl<I: Interner> fmt::Display for IUse<I> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let name = I::resolve(&self.name);
        match self.default {
            Some(IUseDefault::Enabled) => write!(f, "+{name}"),
            Some(IUseDefault::Disabled) => write!(f, "-{name}"),
            None => write!(f, "{name}"),
        }
    }
}

impl<I: Interner> FromStr for IUse<I> {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain() {
        let flag: IUse = "ssl".parse().unwrap();
        assert_eq!(flag.name(), "ssl");
        assert_eq!(flag.default, None);
    }

    #[test]
    fn parse_enabled_default() {
        let flag: IUse = "+ssl".parse().unwrap();
        assert_eq!(flag.name(), "ssl");
        assert_eq!(flag.default, Some(IUseDefault::Enabled));
    }

    #[test]
    fn parse_disabled_default() {
        let flag: IUse = "-debug".parse().unwrap();
        assert_eq!(flag.name(), "debug");
        assert_eq!(flag.default, Some(IUseDefault::Disabled));
    }

    #[test]
    fn parse_line() {
        let flags = IUse::parse_line("+ssl -debug test").unwrap();
        assert_eq!(flags.len(), 3);
    }

    #[test]
    fn parse_empty_line() {
        let flags = IUse::parse_line("").unwrap();
        assert!(flags.is_empty());
    }

    #[test]
    fn display_round_trip() {
        for s in ["+ssl", "-debug", "test"] {
            let flag: IUse = s.parse().unwrap();
            assert_eq!(flag.to_string(), s);
        }
    }

    #[test]
    fn invalid_empty() {
        assert!("".parse::<IUse>().is_err());
    }

    #[test]
    fn invalid_bare_plus() {
        assert!("+".parse::<IUse>().is_err());
    }

    #[test]
    fn invalid_bare_minus() {
        assert!("-".parse::<IUse>().is_err());
    }

    #[test]
    fn complex_flag_names() {
        let flag: IUse = "python_targets_python3_11".parse().unwrap();
        assert_eq!(flag.name(), "python_targets_python3_11");
        assert_eq!(flag.default, None);
    }

    #[test]
    fn test_flag_name_validation() {
        fn is_valid_use_flag_name(name: &str) -> bool {
            let mut bytes = name.bytes();
            let Some(first) = bytes.next() else {
                return false;
            };
            first.is_ascii_alphanumeric()
                && bytes
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'+' | b'@' | b'-'))
        }

        assert!(is_valid_use_flag_name("ssl"));
        assert!(is_valid_use_flag_name("flag_name"));
        assert!(is_valid_use_flag_name("flag-name"));
        assert!(is_valid_use_flag_name("flag@name"));
        assert!(is_valid_use_flag_name("flag+name"));

        assert!(!is_valid_use_flag_name("-flag"));
        assert!(!is_valid_use_flag_name("@flag"));
        assert!(!is_valid_use_flag_name(""));
        assert!(!is_valid_use_flag_name("flag!name"));
    }

    #[test]
    fn invalid_flag_starting_with_at() {
        assert!("@flag".parse::<IUse>().is_err());
    }

    #[test]
    fn invalid_flag_with_exclamation() {
        assert!("foo!bar".parse::<IUse>().is_err());
    }

    #[test]
    fn valid_flag_with_at_character() {
        let flag: IUse = "flag@name".parse().unwrap();
        assert_eq!(flag.name(), "flag@name");
        assert_eq!(flag.default, None);
    }

    #[test]
    fn valid_flag_with_plus_character() {
        let flag: IUse = "flag+name".parse().unwrap();
        assert_eq!(flag.name(), "flag+name");
        assert_eq!(flag.default, None);
    }
}
