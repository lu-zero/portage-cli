use std::fmt;
use std::str::FromStr;

use crate::interner::{DefaultInterner, Interned, Interner};

use crate::error::{Error, Result};

/// Stability level for an architecture keyword
///
/// See [PMS 7.3.3](https://projects.gentoo.org/pms/9/pms.html#keywords).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stability {
    /// The package is stable on this architecture (e.g. `amd64`)
    Stable,
    /// The package is testing/unstable on this architecture (e.g. `~amd64`)
    Testing,
    /// The package is disabled on this architecture (e.g. `-amd64`)
    Disabled,
    /// All architectures are disabled (`-*`)
    DisabledAll,
}

/// A single architecture keyword entry from the `KEYWORDS` variable
///
/// Each keyword consists of an architecture name and a stability level.
///
/// See [PMS 7.3.3](https://projects.gentoo.org/pms/9/pms.html#keywords).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Keyword<I = DefaultInterner>
where
    I: Interner,
{
    /// Architecture (interned)
    pub arch: Interned<I>,
    /// Stability classification
    pub stability: Stability,
}

fn is_valid_arch_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

impl<I: Interner> Keyword<I> {
    fn parse_impl(s: &str) -> Result<Self> {
        if s.is_empty() {
            return Err(Error::InvalidKeyword("empty keyword".to_string()));
        }

        if s == "-*" {
            return Ok(Keyword {
                arch: Interned::intern("*"),
                stability: Stability::DisabledAll,
            });
        }

        if let Some(arch) = s.strip_prefix('~') {
            if arch.is_empty() || !is_valid_arch_name(arch) {
                return Err(Error::InvalidKeyword(s.to_string()));
            }
            Ok(Keyword {
                arch: Interned::intern(arch),
                stability: Stability::Testing,
            })
        } else if let Some(arch) = s.strip_prefix('-') {
            if arch.is_empty() || !is_valid_arch_name(arch) {
                return Err(Error::InvalidKeyword(s.to_string()));
            }
            Ok(Keyword {
                arch: Interned::intern(arch),
                stability: Stability::Disabled,
            })
        } else {
            if !is_valid_arch_name(s) {
                return Err(Error::InvalidKeyword(s.to_string()));
            }
            Ok(Keyword {
                arch: Interned::intern(s),
                stability: Stability::Stable,
            })
        }
    }

    /// Parse a single keyword token
    pub fn parse(s: &str) -> Result<Self> {
        Self::parse_impl(s)
    }
}

impl<I: Interner> fmt::Display for Keyword<I> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let arch = self.arch.as_str();
        match self.stability {
            Stability::Stable => write!(f, "{arch}"),
            Stability::Testing => write!(f, "~{arch}"),
            Stability::Disabled => write!(f, "-{arch}"),
            Stability::DisabledAll => write!(f, "-*"),
        }
    }
}

impl<I: Interner> FromStr for Keyword<I> {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

impl Keyword<DefaultInterner> {
    /// Parse a space-separated `KEYWORDS` line
    ///
    /// # Examples
    ///
    /// ```
    /// use portage_metadata::{Keyword, Stability};
    ///
    /// let kws = Keyword::parse_line("amd64 ~arm64 -x86 -*").unwrap();
    /// assert_eq!(kws.len(), 4);
    /// assert_eq!(kws[0].stability, Stability::Stable);
    /// assert_eq!(kws[1].stability, Stability::Testing);
    /// assert_eq!(kws[2].stability, Stability::Disabled);
    /// assert_eq!(kws[3].stability, Stability::DisabledAll);
    /// ```
    pub fn parse_line(input: &str) -> Result<Vec<Self>> {
        input.split_whitespace().map(Self::parse).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_stable() {
        let kw: Keyword = "amd64".parse().unwrap();
        assert_eq!(kw.arch.as_str(), "amd64");
        assert_eq!(kw.stability, Stability::Stable);
    }

    #[test]
    fn parse_testing() {
        let kw: Keyword = "~arm64".parse().unwrap();
        assert_eq!(kw.arch.as_str(), "arm64");
        assert_eq!(kw.stability, Stability::Testing);
    }

    #[test]
    fn parse_disabled() {
        let kw: Keyword = "-x86".parse().unwrap();
        assert_eq!(kw.arch.as_str(), "x86");
        assert_eq!(kw.stability, Stability::Disabled);
    }

    #[test]
    fn parse_disabled_all() {
        let kw: Keyword = "-*".parse().unwrap();
        assert_eq!(kw.arch.as_str(), "*");
        assert_eq!(kw.stability, Stability::DisabledAll);
    }

    #[test]
    fn parse_line() {
        let kws = Keyword::parse_line("amd64 ~arm64 -x86 -*").unwrap();
        assert_eq!(kws.len(), 4);
        assert_eq!(kws[0].arch.as_str(), "amd64");
        assert_eq!(kws[1].arch.as_str(), "arm64");
        assert_eq!(kws[2].arch.as_str(), "x86");
        assert_eq!(kws[3].stability, Stability::DisabledAll);
    }

    #[test]
    fn parse_empty_line() {
        let kws = Keyword::parse_line("").unwrap();
        assert!(kws.is_empty());
    }

    #[test]
    fn display_round_trip() {
        for s in ["amd64", "~arm64", "-x86", "-*"] {
            let kw: Keyword = s.parse().unwrap();
            assert_eq!(kw.to_string(), s);
        }
    }

    #[test]
    fn invalid_empty() {
        assert!("".parse::<Keyword>().is_err());
    }

    #[test]
    fn invalid_bare_tilde() {
        assert!("~".parse::<Keyword>().is_err());
    }

    #[test]
    fn invalid_bare_dash() {
        assert!("-".parse::<Keyword>().is_err());
    }

    #[test]
    fn invalid_arch_with_exclamation() {
        assert!("~foo!bar".parse::<Keyword>().is_err());
    }

    #[test]
    fn test_arch_name_validation() {
        fn is_valid_arch_name(name: &str) -> bool {
            !name.is_empty()
                && !name.starts_with('-')
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        }

        assert!(is_valid_arch_name("amd64"));
        assert!(is_valid_arch_name("arm_64"));
        assert!(is_valid_arch_name("arm-64"));

        assert!(!is_valid_arch_name("-arch"));
        assert!(!is_valid_arch_name(""));
        assert!(!is_valid_arch_name("arch!name"));
    }

    #[test]
    fn invalid_arch_with_invalid_chars() {
        assert!("foo@bar".parse::<Keyword>().is_err());
    }

    #[test]
    fn valid_arch_with_underscore() {
        let kw: Keyword = "arm_64".parse().unwrap();
        assert_eq!(kw.arch.as_str(), "arm_64");
        assert_eq!(kw.stability, Stability::Stable);
    }

    #[test]
    fn valid_arch_with_hyphen() {
        let kw: Keyword = "arm64-macos".parse().unwrap();
        assert_eq!(kw.arch.as_str(), "arm64-macos");
        assert_eq!(kw.stability, Stability::Stable);
    }

    #[test]
    fn invalid_double_star() {
        assert!("**".parse::<Keyword>().is_err());
    }
}
