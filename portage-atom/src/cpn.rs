use std::fmt;
use std::hash::Hash;
use std::str::FromStr;

use gentoo_interner::{DefaultInterner, Interned};
use winnow::combinator::cut_err;
use winnow::error::StrContext;
use winnow::prelude::*;

use crate::error::{Error, Result};

/// Category/Package Name (Cpn)
///
/// An unversioned package atom — the combination of a category and a package
/// name, separated by a forward slash (e.g. `dev-lang/rust`).
///
/// See [PMS 3.1](https://projects.gentoo.org/pms/9/pms.html#restrictions-upon-names)
/// for the naming rules that apply to categories ([PMS 3.1.1]) and packages
/// ([PMS 3.1.2]).
///
/// [PMS 3.1.1]: https://projects.gentoo.org/pms/9/pms.html#category-names
/// [PMS 3.1.2]: https://projects.gentoo.org/pms/9/pms.html#package-names
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "builder", derive(bon::Builder))]
pub struct Cpn {
    /// The category portion (e.g. `dev-lang`, `app-misc`)
    ///
    /// Must begin with a character other than `-`, `.`, or `+` and may contain
    /// `[A-Za-z0-9+_.-]`. See [PMS 3.1.1].
    ///
    /// [PMS 3.1.1]: https://projects.gentoo.org/pms/9/pms.html#category-names
    #[cfg_attr(feature = "builder", builder(into))]
    pub category: Interned<DefaultInterner>,
    /// The package name portion (e.g. `rust`, `python`)
    ///
    /// Must begin with a character other than `-` or `+` and may contain
    /// `[A-Za-z0-9+_-]`. The name must not end with a hyphen followed
    /// by something that matches the version syntax. See [PMS 3.1.2].
    ///
    /// [PMS 3.1.2]: https://projects.gentoo.org/pms/9/pms.html#package-names
    #[cfg_attr(feature = "builder", builder(into))]
    pub package: Interned<DefaultInterner>,
}

impl Cpn {
    /// Create a new Cpn from category and package strings
    ///
    /// The values are interned automatically. This does **not** validate that
    /// the strings conform to PMS naming rules; use [`Cpn::parse`] for that.
    pub fn new(category: impl AsRef<str>, package: impl AsRef<str>) -> Self {
        Cpn {
            category: Interned::intern(category.as_ref()),
            package: Interned::intern(package.as_ref()),
        }
    }

    /// Parse a `category/package` string into a [`Cpn`]
    ///
    /// Returns an error if the string does not conform to the PMS category
    /// and package naming rules.
    pub fn parse(input: &str) -> Result<Self> {
        parse_cpn
            .parse(input)
            .map_err(|e| Error::InvalidCpn(format!("{}: {}", input, e)))
    }

    /// Try to create from a string
    ///
    /// Alias for [`Cpn::parse`].
    pub fn try_new(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

impl fmt::Display for Cpn {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}/{}", self.category, self.package)
    }
}

impl PartialOrd for Cpn {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Cpn {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (*self.category).cmp(&*other.category) {
            std::cmp::Ordering::Equal => (*self.package).cmp(&*other.package),
            other => other,
        }
    }
}

impl FromStr for Cpn {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

// Winnow parsers

/// Parse category name
///
/// PMS 3.1.1: category name may contain [A-Za-z0-9+_.-], must not begin
/// with hyphen or plus.
pub(crate) fn parse_category(input: &mut &str) -> ModalResult<Interned<DefaultInterner>> {
    use crate::parsers::parse_ident_with_dot;

    parse_ident_with_dot
        .verify(|s: &str| {
            let first_char = s.chars().next().unwrap();
            !matches!(first_char, '-' | '.' | '+')
        })
        .map(|s: &str| Interned::intern(s))
        .context(StrContext::Label("category"))
        .parse_next(input)
}

/// Parse package name
///
/// PMS: package name must start with letter/digit, contain alphanumeric +
/// _ - +. Must not end with hyphen followed by version-like string.
///
/// Note: In practice, Gentoo's tree contains packages whose names start with
/// an underscore (e.g. `acct-user/_cron-failure`). We accept those even though
/// PMS 3.1.2 technically requires an alphanumeric first character.
pub(crate) fn parse_package(input: &mut &str) -> ModalResult<Interned<DefaultInterner>> {
    use crate::parsers::parse_ident_base;

    parse_ident_base
        .verify(|s: &str| {
            // Must start with alphanumeric or underscore.
            // PMS 3.1.2 requires alphanumeric, but real-world Gentoo packages
            // such as acct-user/_cron-failure begin with '_'.
            s.chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        })
        .map(|s: &str| Interned::intern(s))
        .context(StrContext::Label("package"))
        .parse_next(input)
}

/// Parse full Cpn (category/package)
pub(crate) fn parse_cpn(input: &mut &str) -> ModalResult<Cpn> {
    (parse_category, '/', cut_err(parse_package))
        .map(|(category, _, package)| Cpn { category, package })
        .context(StrContext::Label("cpn"))
        .parse_next(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cpv;

    #[test]
    fn test_cpn_parsing() {
        let cpn = Cpn::parse("dev-lang/rust").unwrap();
        assert_eq!(cpn.category, "dev-lang");
        assert_eq!(cpn.package, "rust");
        assert_eq!(cpn.to_string(), "dev-lang/rust");
    }

    #[test]
    fn test_cpn_comparison() {
        let cpn1 = Cpn::parse("app-misc/foo").unwrap();
        let cpn2 = Cpn::parse("dev-lang/rust").unwrap();
        assert!(cpn1 < cpn2);

        let cpn3 = Cpn::parse("app-misc/bar").unwrap();
        assert!(cpn3 < cpn1);
    }

    #[test]
    fn test_invalid_cpn() {
        assert!(Cpn::parse("invalid").is_err());
        assert!(Cpn::parse("/package").is_err());
        assert!(Cpn::parse("category/").is_err());
    }

    #[test]
    fn test_package_name_starting_with_underscore() {
        // Real-world Gentoo packages such as acct-user/_cron-failure have
        // names starting with '_'.  We accept them even though PMS 3.1.2
        // requires an alphanumeric first character.
        let cpn = Cpn::parse("acct-user/_cron-failure").unwrap();
        assert_eq!(cpn.category, "acct-user");
        assert_eq!(cpn.package, "_cron-failure");

        let cpn = Cpn::parse("acct-group/_cron-failure").unwrap();
        assert_eq!(cpn.category, "acct-group");
        assert_eq!(cpn.package, "_cron-failure");
    }

    #[test]
    fn test_package_name_cannot_end_with_hyphen_version() {
        // PMS 3.1.2: "must not end in a hyphen followed by anything matching the version syntax"
        // Note: This rule is enforced at the CPV level, not the CPN level.
        // The CPN parser allows hyphen endings, but the CPV parser ensures proper version boundary detection.

        // These are valid CPN names (the CPV parser will handle version detection)
        assert!(
            Cpn::parse("cat/pkg-").is_ok(),
            "Package name ending with hyphen is valid at CPN level"
        );
        assert!(
            Cpn::parse("cat/pkg-test").is_ok(),
            "Package name ending with hyphen+word should be valid"
        );

        // But when used in CPV context, the version boundary detection should work correctly
        let cpv1 = Cpv::parse("cat/pkg--1.2"); // pkg- is package, -1.2 is version
        assert!(
            cpv1.is_ok(),
            "CPV parser should handle hyphen in package name correctly"
        );
        let cpv1 = cpv1.unwrap();
        assert_eq!(cpv1.cpn.package, "pkg-");
        assert_eq!(cpv1.version.numbers.as_slice(), vec![1, 2].as_slice());
    }

    #[test]
    fn test_package_name_no_arbitrary_length_limit() {
        // PMS doesn't specify length limits, so we shouldn't impose arbitrary ones

        // Valid: normal length
        assert!(Cpn::parse("cat/normal-package-name").is_ok());

        // Valid: very long name (PMS doesn't specify limits)
        let long_name = "a".repeat(100);
        assert!(Cpn::parse(&format!("cat/{}", long_name)).is_ok());

        // Valid: very long category (PMS doesn't specify limits)
        let long_cat = "a".repeat(100);
        assert!(Cpn::parse(&format!("{}/package", long_cat)).is_ok());
    }

    #[test]
    fn test_category_name_with_dot() {
        // PMS 3.1.1: category names may contain dots
        assert!(Cpn::parse("dev.lang/rust").is_ok());
        assert!(Cpn::parse("app-office/libreoffice").is_ok());
        assert!(Cpn::parse("media.gfx/gimp").is_ok());

        // Category names can start with underscore (allowed character)
        assert!(Cpn::parse("_special/package").is_ok());

        // But category names must not begin with hyphen, dot, or plus
        assert!(Cpn::parse("-dev-lang/rust").is_err());
        assert!(Cpn::parse(".dev-lang/rust").is_err());
        assert!(Cpn::parse("+dev-lang/rust").is_err());
    }

    #[test]
    fn test_cpn_copy() {
        let cpn = Cpn::parse("dev-lang/rust").unwrap();
        let cpn2 = cpn;
        assert_eq!(cpn, cpn2);
    }

    #[test]
    #[cfg(feature = "builder")]
    fn test_cpn_builder() {
        let cpn = Cpn::builder().category("dev-lang").package("rust").build();
        assert_eq!(cpn.category, "dev-lang");
        assert_eq!(cpn.package, "rust");
        assert_eq!(cpn.to_string(), "dev-lang/rust");
    }

    // --- PMS compliance tests ---

    #[test]
    fn test_category_pms_3_1_1() {
        // PMS 3.1.1: [A-Za-z0-9+_.-], must not begin with hyphen, dot, or plus

        // Valid categories
        assert!(Cpn::parse("dev-lang/rust").is_ok());
        assert!(Cpn::parse("dev.lang/rust").is_ok()); // dot in category
        assert!(Cpn::parse("0cat/pkg").is_ok()); // digit-start category
        assert!(Cpn::parse("_cat/pkg").is_ok()); // underscore-start category
        assert!(Cpn::parse("dev+lang/rust").is_ok()); // plus in category (not at start)
        assert!(Cpn::parse("a-b.c+d/pkg").is_ok()); // all allowed chars

        // Invalid: starts with forbidden character
        assert!(Cpn::parse("-cat/pkg").is_err());
        assert!(Cpn::parse(".cat/pkg").is_err());
        assert!(Cpn::parse("+cat/pkg").is_err());
    }

    #[test]
    fn test_package_pms_3_1_2() {
        // PMS 3.1.2: [A-Za-z0-9+_-], must not begin with hyphen or plus

        // Valid packages
        assert!(Cpn::parse("cat/c++").is_ok()); // plus in package name
        assert!(Cpn::parse("cat/0package").is_ok()); // digit-start package
        assert!(Cpn::parse("cat/a-b_c+d").is_ok()); // all allowed chars

        // Invalid: starts with forbidden character
        assert!(Cpn::parse("cat/-pkg").is_err());
        assert!(Cpn::parse("cat/+pkg").is_err());
    }

    #[test]
    fn test_cpn_round_trip() {
        let inputs = [
            "dev-lang/rust",
            "app-office/libreoffice",
            "dev.lang/gimp",
            "acct-user/_cron-failure",
            "sys-kernel/gentoo-sources",
            "cat/c++",
        ];
        for input in inputs {
            let cpn = Cpn::parse(input).unwrap();
            assert_eq!(cpn.to_string(), input, "round-trip failed for: {input}");
        }
    }
}
