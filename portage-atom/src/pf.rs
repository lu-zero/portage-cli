use std::fmt;
use std::str::FromStr;

use gentoo_interner::{DefaultInterner, Interned};
use winnow::Parser;

use crate::cpn::parse_package;
use crate::error::{Error, Result};
use crate::parsers::find_last_hyphen_digit;
use crate::version::{Version, parse_version};

/// Package name + version (`PF`), as defined in [PMS §11.1].
///
/// `PF` is the concatenation of `PN` (package name) and `PVR` (version with
/// optional revision), e.g. `bash-5.3_p9-r2` or `vim-9.1.0000`. It is the
/// directory name used inside `/var/db/pkg/$CATEGORY/` for installed packages.
///
/// Unlike [`Cpv`](crate::Cpv), a `Pf` carries no category — it is the fragment
/// after the `/` in a VDB path. The version boundary is detected at the last
/// hyphen followed by a digit (PMS version syntax rule).
///
/// [PMS §11.1]: https://projects.gentoo.org/pms/9/pms.html#ebuild-environment-variables
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Pf {
    /// The package name (`PN`), interned.
    pub package: Interned<DefaultInterner>,
    /// The version including optional revision (`PVR`).
    pub version: Version,
}

impl Pf {
    /// Parse a `package-version` string (PF format) into a [`Pf`].
    ///
    /// Returns an error if there is no valid version boundary or if the
    /// package name or version does not conform to PMS rules.
    pub fn parse(pf: &str) -> Result<Self> {
        let version_pos = find_last_hyphen_digit(pf)
            .ok_or_else(|| Error::InvalidCpv(format!("no version boundary in PF: {pf}")))?;
        let pkg_str = &pf[..version_pos];
        let ver_str = &pf[version_pos + 1..];

        let package = parse_package
            .parse(pkg_str)
            .map_err(|e| Error::InvalidCpv(format!("invalid package name in PF '{pf}': {e}")))?;
        let version = parse_version
            .parse(ver_str)
            .map_err(|e| Error::InvalidCpv(format!("invalid version in PF '{pf}': {e}")))?;

        Ok(Pf { package, version })
    }
}

impl fmt::Display for Pf {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}-{}", self.package, self.version)
    }
}

impl FromStr for Pf {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

impl PartialEq<str> for Pf {
    fn eq(&self, other: &str) -> bool {
        Pf::parse(other).is_ok_and(|other_pf| other_pf == *self)
    }
}

impl PartialEq<&str> for Pf {
    fn eq(&self, other: &&str) -> bool {
        self == *other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple() {
        let pf = Pf::parse("bash-5.3").unwrap();
        assert_eq!(pf.package.as_ref(), "bash");
        assert_eq!(pf.to_string(), "bash-5.3");
    }

    #[test]
    fn parse_with_revision() {
        let pf = Pf::parse("bash-5.3_p9-r2").unwrap();
        assert_eq!(pf.package.as_ref(), "bash");
        assert_eq!(pf.version.revision().0, 2);
        assert_eq!(pf.to_string(), "bash-5.3_p9-r2");
    }

    #[test]
    fn parse_hyphenated_package() {
        let pf = Pf::parse("dev-python-3.11.0").unwrap();
        assert_eq!(pf.package.as_ref(), "dev-python");
        assert_eq!(pf.to_string(), "dev-python-3.11.0");
    }

    #[test]
    fn no_version_boundary() {
        assert!(Pf::parse("bash").is_err());
        assert!(Pf::parse("bash-release").is_err());
    }

    #[test]
    fn roundtrip_matches_cpv() {
        let cases = ["bash-5.3_p9-r2", "vim-9.1.0000", "rust-1.75.0-r1"];
        for pf_str in cases {
            let pf = Pf::parse(pf_str).unwrap();
            assert_eq!(pf.to_string(), pf_str, "round-trip failed: {pf_str}");
        }
    }
}
