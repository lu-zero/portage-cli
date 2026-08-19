use std::fmt;
use std::hash::Hash;
use std::str::FromStr;

use gentoo_interner::Interned;
use winnow::combinator::cut_err;
use winnow::error::StrContext;
use winnow::prelude::*;

use crate::cpn::{Cpn, parse_category, parse_package};
use crate::error::{Error, Result};
use crate::parsers::{find_last_hyphen_digit, parse_ident_with_dot_star};
use crate::version::{Version, parse_version, parse_version_no_raw};

/// Category/Package/Version (Cpv)
///
/// A versioned package atom — a [`Cpn`] paired with a [`Version`], such as
/// `dev-lang/rust-1.75.0`. The version is separated from the package name at
/// the **last** hyphen followed by a digit (per PMS).
///
/// See [PMS 3.2](https://projects.gentoo.org/pms/9/pms.html#version-specifications)
/// for the version syntax and
/// [PMS 3.3](https://projects.gentoo.org/pms/9/pms.html#version-comparison)
/// for the version comparison algorithm.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "builder", derive(bon::Builder))]
pub struct Cpv {
    /// The unversioned category/package name
    pub cpn: Cpn,
    /// The version component (e.g. `1.75.0`, `3.11.0_rc2_p1-r1`)
    ///
    /// See [`Version`] and [PMS 3.2] for the full version format.
    ///
    /// [PMS 3.2]: https://projects.gentoo.org/pms/9/pms.html#version-specifications
    pub version: Version,
}

impl Cpv {
    /// Create a new Cpv from a [`Cpn`] and a [`Version`]
    pub fn new(cpn: Cpn, version: Version) -> Self {
        Cpv { cpn, version }
    }

    /// Create a [`Cpv`] from separate category, package, and version parts
    ///
    /// Both `category` and `package` are interned. Prefer this over
    /// constructing a `format!("{category}/{pf}")` string just to parse it.
    pub fn from_parts(
        category: impl AsRef<str>,
        package: impl AsRef<str>,
        version: Version,
    ) -> Self {
        Cpv::new(Cpn::new(category, package), version)
    }

    /// Parse a `category/package-version` string into a [`Cpv`]
    ///
    /// Returns an error if the string does not conform to the PMS format or
    /// naming rules.
    pub fn parse(input: &str) -> Result<Self> {
        parse_cpv_with_raw
            .parse(input)
            .map_err(|e| Error::InvalidCpv(format!("{}: {}", input, e)))
    }

    /// Try to create from a string
    ///
    /// Alias for [`Cpv::parse`].
    pub fn try_new(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

impl fmt::Display for Cpv {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}-{}", self.cpn, self.version)
    }
}

impl PartialOrd for Cpv {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Cpv {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.cpn.cmp(&other.cpn) {
            std::cmp::Ordering::Equal => self.version.cmp(&other.version),
            other => other,
        }
    }
}

impl FromStr for Cpv {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

// Winnow parsers

/// Parse a `category/package-version` string (without storing the raw version)
///
/// Package names can contain hyphens, so the version boundary is found at the
/// last hyphen followed by a digit (per PMS).
pub(crate) fn parse_cpv(input: &mut &str) -> ModalResult<Cpv> {
    parse_cpv_impl(input, |i| parse_version_no_raw(i).map(|(v, _)| v))
}

pub(crate) fn parse_cpv_with_glob(input: &mut &str) -> ModalResult<(Cpv, bool)> {
    (parse_category, '/', cut_err(parse_ident_with_dot_star))
        .verify_map(|(category, _, pkg_ver): (Interned<_>, char, &str)| {
            let version_pos = find_last_hyphen_digit(pkg_ver)?;
            let pkg_str = &pkg_ver[..version_pos];
            let ver_str = &pkg_ver[version_pos + 1..];

            let package = parse_package.parse(pkg_str).ok()?;
            let (version, glob) = parse_version_no_raw.parse(ver_str).ok()?;

            Some((
                Cpv {
                    cpn: Cpn { category, package },
                    version,
                },
                glob,
            ))
        })
        .context(StrContext::Label("cpv"))
        .parse_next(input)
}

fn parse_cpv_impl(
    input: &mut &str,
    mut version_parser: impl Fn(&mut &str) -> ModalResult<Version>,
) -> ModalResult<Cpv> {
    (parse_category, '/', cut_err(parse_ident_with_dot_star))
        .verify_map(move |(category, _, pkg_ver): (Interned<_>, char, &str)| {
            let version_pos = find_last_hyphen_digit(pkg_ver)?;
            let pkg_str = &pkg_ver[..version_pos];
            let ver_str = &pkg_ver[version_pos + 1..];

            let package = parse_package.parse(pkg_str).ok()?;
            let version = version_parser.parse(ver_str).ok()?;

            Some(Cpv {
                cpn: Cpn { category, package },
                version,
            })
        })
        .context(StrContext::Label("cpv"))
        .parse_next(input)
}

fn parse_cpv_with_raw(input: &mut &str) -> ModalResult<Cpv> {
    parse_cpv_impl(input, parse_version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cpv_parsing() {
        let cpv = Cpv::parse("dev-lang/rust-1.75.0").unwrap();
        assert_eq!(cpv.cpn.category, "dev-lang");
        assert_eq!(cpv.cpn.package, "rust");
        assert_eq!(cpv.version.numbers[0], 1);
        assert_eq!(cpv.version.numbers[1], 75);
        assert_eq!(cpv.version.numbers[2], 0);
        assert_eq!(cpv.to_string(), "dev-lang/rust-1.75.0");
    }

    #[test]
    fn test_cpv_with_revision() {
        let cpv = Cpv::parse("dev-lang/rust-1.75.0-r1").unwrap();
        assert_eq!(cpv.version.revision.0, 1);
        assert_eq!(cpv.to_string(), "dev-lang/rust-1.75.0-r1");
    }

    #[test]
    fn test_cpv_comparison() {
        let cpv1 = Cpv::parse("dev-lang/rust-1.75.0").unwrap();
        let cpv2 = Cpv::parse("dev-lang/rust-1.76.0").unwrap();
        assert!(cpv1 < cpv2);

        let cpv3 = Cpv::parse("dev-lang/rust-1.75.0-r1").unwrap();
        assert!(cpv1 < cpv3);
    }

    #[test]
    #[cfg(feature = "builder")]
    fn test_cpv_builder() {
        let cpn = Cpn::new("dev-lang", "rust");
        let version = Version::new(&[1, 75, 0]);
        let cpv = Cpv::builder().cpn(cpn).version(version).build();
        assert_eq!(cpv.cpn.category, "dev-lang");
        assert_eq!(cpv.cpn.package, "rust");
        assert_eq!(cpv.version.numbers.as_slice(), vec![1, 75, 0].as_slice());
        assert_eq!(cpv.to_string(), "dev-lang/rust-1.75.0");
    }
}
