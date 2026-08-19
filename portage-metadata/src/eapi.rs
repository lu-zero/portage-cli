use std::fmt;
use std::str::FromStr;

use crate::error::{Error, Result};

/// EAPI (Ebuild API) version
///
/// The EAPI controls which features and behaviours are available to an ebuild.
/// Each EAPI builds on the previous one, adding or modifying capabilities.
///
/// See [PMS 2](https://projects.gentoo.org/pms/9/pms.html#eapis).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Eapi {
    /// EAPI 0 — base (legacy)
    Zero,
    /// EAPI 1 — slot deps, IUSE defaults
    One,
    /// EAPI 2 — SRC_URI arrows, USE deps, `src_prepare`/`src_configure`
    Two,
    /// EAPI 3 — `PROPERTIES`, prefix support
    Three,
    /// EAPI 4 — `REQUIRED_USE`, `pkg_pretend`, `DOCS`/`HTML_DOCS`
    Four,
    /// EAPI 5 — sub-slots, slot operators, `??` in REQUIRED_USE
    Five,
    /// EAPI 6 — `eapply`/`eapply_user`, bash 4.2 minimum
    Six,
    /// EAPI 7 — `BDEPEND`, `SYSROOT`/`BROOT`
    Seven,
    /// EAPI 8 — `IDEPEND`, USE-conditional `PROPERTIES`/`RESTRICT`
    Eight,
    /// EAPI 9 — Same features as EAPI 8, plus selective URI restrictions
    ///
    /// See [PMS 2](https://projects.gentoo.org/pms/9/pms.html#eapis).
    Nine,
}

impl Eapi {
    /// Whether this EAPI supports `BDEPEND` (build-host dependencies)
    ///
    /// Introduced in EAPI 7.
    pub fn has_bdepend(&self) -> bool {
        *self >= Eapi::Seven
    }

    /// Whether this EAPI supports `IDEPEND` (install-time dependencies)
    ///
    /// Introduced in EAPI 8.
    pub fn has_idepend(&self) -> bool {
        *self >= Eapi::Eight
    }

    /// Whether this EAPI supports `REQUIRED_USE`
    ///
    /// Introduced in EAPI 4.
    pub fn has_required_use(&self) -> bool {
        *self >= Eapi::Four
    }

    /// Whether this EAPI supports the `??` (at-most-one-of) operator
    /// in `REQUIRED_USE`.
    ///
    /// Introduced in EAPI 5.
    pub fn has_at_most_one_of(&self) -> bool {
        *self >= Eapi::Five
    }

    /// Whether this EAPI supports `src_prepare` and `src_configure` phases
    ///
    /// Introduced in EAPI 2.
    pub fn has_src_prepare(&self) -> bool {
        *self >= Eapi::Two
    }

    /// Whether this EAPI supports the `pkg_pretend` phase
    ///
    /// Introduced in EAPI 4.
    pub fn has_pkg_pretend(&self) -> bool {
        *self >= Eapi::Four
    }

    /// Whether this EAPI supports SRC_URI arrow renaming (`-> filename`)
    ///
    /// Introduced in EAPI 2.
    pub fn has_src_uri_arrows(&self) -> bool {
        *self >= Eapi::Two
    }

    /// Whether this EAPI supports sub-slots and slot operators (`:=`, `:*`)
    ///
    /// Introduced in EAPI 5.
    pub fn has_slot_operators(&self) -> bool {
        *self >= Eapi::Five
    }

    /// Whether this EAPI supports `PROPERTIES`
    ///
    /// Introduced in EAPI 3.
    pub fn has_properties(&self) -> bool {
        *self >= Eapi::Three
    }

    /// Whether this EAPI supports USE-conditional `PROPERTIES` and `RESTRICT`
    ///
    /// Introduced in EAPI 8.
    pub fn has_use_conditional_restrict(&self) -> bool {
        *self >= Eapi::Eight
    }

    /// Whether this EAPI supports selective URI restrictions (`fetch+`/`mirror+` prefixes)
    ///
    /// Introduced in EAPI 8.
    pub fn has_selective_uri_restrictions(&self) -> bool {
        *self >= Eapi::Eight
    }
}

impl fmt::Display for Eapi {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let n = match self {
            Eapi::Zero => "0",
            Eapi::One => "1",
            Eapi::Two => "2",
            Eapi::Three => "3",
            Eapi::Four => "4",
            Eapi::Five => "5",
            Eapi::Six => "6",
            Eapi::Seven => "7",
            Eapi::Eight => "8",
            Eapi::Nine => "9",
        };
        f.write_str(n)
    }
}

impl FromStr for Eapi {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "0" => Ok(Eapi::Zero),
            "1" => Ok(Eapi::One),
            "2" => Ok(Eapi::Two),
            "3" => Ok(Eapi::Three),
            "4" => Ok(Eapi::Four),
            "5" => Ok(Eapi::Five),
            "6" => Ok(Eapi::Six),
            "7" => Ok(Eapi::Seven),
            "8" => Ok(Eapi::Eight),
            "9" => Ok(Eapi::Nine),
            _ => Err(Error::InvalidEapi(s.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_all_eapis() {
        for (s, expected) in [
            ("0", Eapi::Zero),
            ("1", Eapi::One),
            ("2", Eapi::Two),
            ("3", Eapi::Three),
            ("4", Eapi::Four),
            ("5", Eapi::Five),
            ("6", Eapi::Six),
            ("7", Eapi::Seven),
            ("8", Eapi::Eight),
            ("9", Eapi::Nine),
        ] {
            assert_eq!(s.parse::<Eapi>().unwrap(), expected);
        }
    }

    #[test]
    fn display_round_trip() {
        for eapi in [
            Eapi::Zero,
            Eapi::One,
            Eapi::Two,
            Eapi::Three,
            Eapi::Four,
            Eapi::Five,
            Eapi::Six,
            Eapi::Seven,
            Eapi::Eight,
            Eapi::Nine,
        ] {
            assert_eq!(eapi.to_string().parse::<Eapi>().unwrap(), eapi);
        }
    }

    #[test]
    fn invalid_eapi() {
        assert!("10".parse::<Eapi>().is_err());
        assert!("".parse::<Eapi>().is_err());
        assert!("foo".parse::<Eapi>().is_err());
    }

    #[test]
    fn ordering() {
        assert!(Eapi::Zero < Eapi::Eight);
        assert!(Eapi::Seven < Eapi::Eight);
        assert!(Eapi::Eight < Eapi::Nine);
        assert!(Eapi::Four > Eapi::Three);
    }

    #[test]
    fn feature_queries() {
        assert!(!Eapi::Six.has_bdepend());
        assert!(Eapi::Seven.has_bdepend());
        assert!(Eapi::Eight.has_bdepend());
        assert!(Eapi::Nine.has_bdepend());

        assert!(!Eapi::Seven.has_idepend());
        assert!(Eapi::Eight.has_idepend());
        assert!(Eapi::Nine.has_idepend());

        assert!(!Eapi::Three.has_required_use());
        assert!(Eapi::Four.has_required_use());

        assert!(!Eapi::Four.has_at_most_one_of());
        assert!(Eapi::Five.has_at_most_one_of());

        assert!(!Eapi::One.has_src_prepare());
        assert!(Eapi::Two.has_src_prepare());

        assert!(!Eapi::Three.has_pkg_pretend());
        assert!(Eapi::Four.has_pkg_pretend());

        assert!(!Eapi::One.has_src_uri_arrows());
        assert!(Eapi::Two.has_src_uri_arrows());

        assert!(!Eapi::Four.has_slot_operators());
        assert!(Eapi::Five.has_slot_operators());

        assert!(!Eapi::Two.has_properties());
        assert!(Eapi::Three.has_properties());

        assert!(!Eapi::Seven.has_use_conditional_restrict());
        assert!(Eapi::Eight.has_use_conditional_restrict());
        assert!(Eapi::Nine.has_use_conditional_restrict());

        assert!(!Eapi::Seven.has_selective_uri_restrictions());
        assert!(Eapi::Eight.has_selective_uri_restrictions());
        assert!(Eapi::Nine.has_selective_uri_restrictions());
    }
}
