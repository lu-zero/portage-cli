/// Error type for portage-metadata parsing and operations.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    /// Invalid EAPI value.
    #[error("invalid EAPI: {0}")]
    InvalidEapi(String),

    /// Invalid keyword string.
    #[error("invalid keyword: {0}")]
    InvalidKeyword(String),

    /// Invalid IUSE flag entry.
    #[error("invalid IUSE entry: {0}")]
    InvalidIUse(String),

    /// Invalid phase function name.
    #[error("invalid phase: {0}")]
    InvalidPhase(String),

    /// Invalid SRC_URI expression.
    ///
    /// Structured (not pre-rendered): the application renders the inner
    /// [`crate::diagnostic::ParseDiagnostic`] as a miette code frame at
    /// display time.
    #[error("{0}")]
    InvalidSrcUri(crate::diagnostic::ParseDiagnostic),

    /// Invalid LICENSE expression. See [`Error::InvalidSrcUri`]'s doc.
    #[error("{0}")]
    InvalidLicense(crate::diagnostic::ParseDiagnostic),

    /// Invalid REQUIRED_USE expression. See [`Error::InvalidSrcUri`]'s doc.
    #[error("{0}")]
    InvalidRequiredUse(crate::diagnostic::ParseDiagnostic),

    /// Invalid RESTRICT or PROPERTIES expression. See [`Error::InvalidSrcUri`]'s doc.
    #[error("{0}")]
    InvalidRestrict(crate::diagnostic::ParseDiagnostic),

    /// Error parsing a metadata cache entry.
    #[error("invalid cache entry: {0}")]
    InvalidCacheEntry(String),

    /// Missing mandatory field in a cache entry.
    #[error("missing required field: {0}")]
    MissingField(String),

    /// Error from the portage-atom dependency parser.
    #[error("dependency parse error: {0}")]
    DepError(String),

    /// Invalid SLOT value (does not conform to PMS 3.1.3).
    #[error("invalid SLOT: {0}")]
    InvalidSlot(String),
}

impl Error {
    /// The structured parse diagnostic behind this error, if it's one of
    /// the four grammar variants that carry one.
    ///
    /// UI code treats the result as a [`miette::Diagnostic`] and renders a
    /// code frame at display time.
    pub fn parse_diagnostic(&self) -> Option<&crate::diagnostic::ParseDiagnostic> {
        match self {
            Error::InvalidSrcUri(d)
            | Error::InvalidLicense(d)
            | Error::InvalidRequiredUse(d)
            | Error::InvalidRestrict(d) => Some(d),
            _ => None,
        }
    }
}

/// Result type for portage-metadata operations.
pub type Result<T> = std::result::Result<T, Error>;
