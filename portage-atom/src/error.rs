/// Error type for portage-atom parsing and operations
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    /// Generic parse error
    #[error("parse error: {0}")]
    Parse(String),

    /// The category name does not conform to [PMS 3.1.1]
    ///
    /// [PMS 3.1.1]: https://projects.gentoo.org/pms/9/pms.html#category-names
    #[error("invalid category: {0}")]
    InvalidCategory(String),

    /// The package name does not conform to [PMS 3.1.2]
    ///
    /// [PMS 3.1.2]: https://projects.gentoo.org/pms/9/pms.html#package-names
    #[error("invalid package: {0}")]
    InvalidPackage(String),

    /// The version string does not conform to [PMS 3.2]
    ///
    /// [PMS 3.2]: https://projects.gentoo.org/pms/9/pms.html#version-specifications
    #[error("invalid version: {0}")]
    InvalidVersion(String),

    /// The category/package-version string is malformed
    ///
    /// See [PMS 3.2] for the CPV format.
    ///
    /// [PMS 3.2]: https://projects.gentoo.org/pms/9/pms.html#version-specifications
    #[error("invalid cpv: {0}")]
    InvalidCpv(String),

    /// The category/package string is malformed
    ///
    /// See [PMS 3.1] for the CPN format.
    ///
    /// [PMS 3.1]: https://projects.gentoo.org/pms/9/pms.html#restrictions-upon-names
    #[error("invalid cpn: {0}")]
    InvalidCpn(String),

    /// The dependency atom string is malformed
    ///
    /// See [PMS 8.3] for the dependency specification.
    ///
    /// [PMS 8.3]: https://projects.gentoo.org/pms/9/pms.html#package-dependency-specifications
    #[error("invalid dep: {0}")]
    InvalidDep(String),

    /// The slot dependency portion is malformed
    ///
    /// See [PMS 8.3.3].
    ///
    /// [PMS 8.3.3]: https://projects.gentoo.org/pms/9/pms.html#slot-dependencies
    #[error("invalid slot: {0}")]
    InvalidSlot(String),

    /// A USE-dependency item is malformed
    ///
    /// See [PMS 8.3.4].
    ///
    /// [PMS 8.3.4]: https://projects.gentoo.org/pms/9/pms.html#style-and-style-use-dependencies
    #[error("invalid use dep: {0}")]
    InvalidUseDep(String),

    /// A version comparison operator is not recognised
    ///
    /// See [PMS 8.3.1].
    ///
    /// [PMS 8.3.1]: https://projects.gentoo.org/pms/9/pms.html#operators
    #[error("invalid operator: {0}")]
    InvalidOperator(String),

    /// A dependency string (the contents of a `*DEPEND` variable) is malformed
    ///
    /// See [PMS 8.2].
    ///
    /// [PMS 8.2]: https://projects.gentoo.org/pms/9/pms.html#dependency-specification-format
    #[error("invalid dep string: {0}")]
    InvalidDepString(String),
}

/// Result type for portage-atom operations
pub type Result<T> = std::result::Result<T, Error>;
