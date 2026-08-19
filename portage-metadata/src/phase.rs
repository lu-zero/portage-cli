use std::fmt;
use std::str::FromStr;

use crate::error::{Error, Result};

/// Ebuild phase function
///
/// Phase functions are called by the package manager in a defined order
/// during package build and installation.
///
/// See [PMS 9](https://projects.gentoo.org/pms/9/pms.html#ebuilddefined-functions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    /// `pkg_pretend` — pre-flight checks (EAPI 4+)
    PkgPretend,
    /// `pkg_setup` — environment setup
    PkgSetup,
    /// `src_unpack` — extract source archives
    SrcUnpack,
    /// `src_prepare` — apply patches (EAPI 2+)
    SrcPrepare,
    /// `src_configure` — run configure (EAPI 2+)
    SrcConfigure,
    /// `src_compile` — build the software
    SrcCompile,
    /// `src_test` — run test suite
    SrcTest,
    /// `src_install` — install into image directory
    SrcInstall,
    /// `pkg_preinst` — before merging into live filesystem
    PkgPreinst,
    /// `pkg_postinst` — after merging into live filesystem
    PkgPostinst,
    /// `pkg_prerm` — before removing from live filesystem
    PkgPrerm,
    /// `pkg_postrm` — after removing from live filesystem
    PkgPostrm,
    /// `pkg_config` — optional post-install configuration
    PkgConfig,
    /// `pkg_info` — display package information
    PkgInfo,
    /// `pkg_nofetch` — handle fetch-restricted sources
    PkgNofetch,
}

impl Phase {
    /// Return the short phase name as a `&'static str` (same as `Display`)
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::PkgPretend => "pretend",
            Phase::PkgSetup => "setup",
            Phase::SrcUnpack => "unpack",
            Phase::SrcPrepare => "prepare",
            Phase::SrcConfigure => "configure",
            Phase::SrcCompile => "compile",
            Phase::SrcTest => "test",
            Phase::SrcInstall => "install",
            Phase::PkgPreinst => "preinst",
            Phase::PkgPostinst => "postinst",
            Phase::PkgPrerm => "prerm",
            Phase::PkgPostrm => "postrm",
            Phase::PkgConfig => "config",
            Phase::PkgInfo => "info",
            Phase::PkgNofetch => "nofetch",
        }
    }

    /// Parse a space-separated `DEFINED_PHASES` line into a list of phases
    ///
    /// The special value `-` (used in the cache to mean "no phases defined")
    /// returns an empty list.
    ///
    /// # Examples
    ///
    /// ```
    /// use portage_metadata::Phase;
    ///
    /// let phases = Phase::parse_line("compile configure install").unwrap();
    /// assert_eq!(phases.len(), 3);
    /// assert_eq!(phases[0], Phase::SrcCompile);
    ///
    /// let empty = Phase::parse_line("-").unwrap();
    /// assert!(empty.is_empty());
    /// ```
    pub fn parse_line(input: &str) -> Result<Vec<Phase>> {
        let trimmed = input.trim();
        if trimmed.is_empty() || trimmed == "-" {
            return Ok(Vec::new());
        }
        trimmed
            .split_whitespace()
            .map(|token| token.parse())
            .collect()
    }
}

impl FromStr for Phase {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        // DEFINED_PHASES uses short names (without pkg_/src_ prefix)
        // but we also accept full names for convenience.
        match s {
            "pretend" | "pkg_pretend" => Ok(Phase::PkgPretend),
            "setup" | "pkg_setup" => Ok(Phase::PkgSetup),
            "unpack" | "src_unpack" => Ok(Phase::SrcUnpack),
            "prepare" | "src_prepare" => Ok(Phase::SrcPrepare),
            "configure" | "src_configure" => Ok(Phase::SrcConfigure),
            "compile" | "src_compile" => Ok(Phase::SrcCompile),
            "test" | "src_test" => Ok(Phase::SrcTest),
            "install" | "src_install" => Ok(Phase::SrcInstall),
            "preinst" | "pkg_preinst" => Ok(Phase::PkgPreinst),
            "postinst" | "pkg_postinst" => Ok(Phase::PkgPostinst),
            "prerm" | "pkg_prerm" => Ok(Phase::PkgPrerm),
            "postrm" | "pkg_postrm" => Ok(Phase::PkgPostrm),
            "config" | "pkg_config" => Ok(Phase::PkgConfig),
            "info" | "pkg_info" => Ok(Phase::PkgInfo),
            "nofetch" | "pkg_nofetch" => Ok(Phase::PkgNofetch),
            _ => Err(Error::InvalidPhase(s.to_string())),
        }
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_short_names() {
        assert_eq!("compile".parse::<Phase>().unwrap(), Phase::SrcCompile);
        assert_eq!("configure".parse::<Phase>().unwrap(), Phase::SrcConfigure);
        assert_eq!("install".parse::<Phase>().unwrap(), Phase::SrcInstall);
        assert_eq!("pretend".parse::<Phase>().unwrap(), Phase::PkgPretend);
        assert_eq!("setup".parse::<Phase>().unwrap(), Phase::PkgSetup);
        assert_eq!("unpack".parse::<Phase>().unwrap(), Phase::SrcUnpack);
        assert_eq!("prepare".parse::<Phase>().unwrap(), Phase::SrcPrepare);
        assert_eq!("test".parse::<Phase>().unwrap(), Phase::SrcTest);
        assert_eq!("preinst".parse::<Phase>().unwrap(), Phase::PkgPreinst);
        assert_eq!("postinst".parse::<Phase>().unwrap(), Phase::PkgPostinst);
        assert_eq!("prerm".parse::<Phase>().unwrap(), Phase::PkgPrerm);
        assert_eq!("postrm".parse::<Phase>().unwrap(), Phase::PkgPostrm);
        assert_eq!("config".parse::<Phase>().unwrap(), Phase::PkgConfig);
        assert_eq!("info".parse::<Phase>().unwrap(), Phase::PkgInfo);
        assert_eq!("nofetch".parse::<Phase>().unwrap(), Phase::PkgNofetch);
    }

    #[test]
    fn parse_full_names() {
        assert_eq!("src_compile".parse::<Phase>().unwrap(), Phase::SrcCompile);
        assert_eq!("pkg_setup".parse::<Phase>().unwrap(), Phase::PkgSetup);
        assert_eq!("pkg_pretend".parse::<Phase>().unwrap(), Phase::PkgPretend);
    }

    #[test]
    fn parse_line() {
        let phases = Phase::parse_line("compile configure install").unwrap();
        assert_eq!(phases.len(), 3);
        assert_eq!(phases[0], Phase::SrcCompile);
        assert_eq!(phases[1], Phase::SrcConfigure);
        assert_eq!(phases[2], Phase::SrcInstall);
    }

    #[test]
    fn parse_line_dash() {
        let phases = Phase::parse_line("-").unwrap();
        assert!(phases.is_empty());
    }

    #[test]
    fn parse_empty_line() {
        let phases = Phase::parse_line("").unwrap();
        assert!(phases.is_empty());
    }

    #[test]
    fn display_round_trip() {
        for phase in [
            Phase::PkgPretend,
            Phase::PkgSetup,
            Phase::SrcUnpack,
            Phase::SrcPrepare,
            Phase::SrcConfigure,
            Phase::SrcCompile,
            Phase::SrcTest,
            Phase::SrcInstall,
            Phase::PkgPreinst,
            Phase::PkgPostinst,
            Phase::PkgPrerm,
            Phase::PkgPostrm,
            Phase::PkgConfig,
            Phase::PkgInfo,
            Phase::PkgNofetch,
        ] {
            let s = phase.to_string();
            assert_eq!(s.parse::<Phase>().unwrap(), phase);
        }
    }

    #[test]
    fn invalid_phase() {
        assert!("foo".parse::<Phase>().is_err());
        assert!("".parse::<Phase>().is_err());
    }

    #[test]
    fn real_world_defined_phases() {
        let phases = Phase::parse_line("install test unpack").unwrap();
        assert_eq!(phases.len(), 3);
        assert_eq!(phases[0], Phase::SrcInstall);
        assert_eq!(phases[1], Phase::SrcTest);
        assert_eq!(phases[2], Phase::SrcUnpack);
    }
}
