use std::path::PathBuf;

/// Error type for portage-repo operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// I/O error with associated filesystem path.
    #[error("I/O error at {path}: {source}")]
    Io {
        /// The filesystem path that caused the error.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// Invalid or unparsable `metadata/layout.conf`.
    #[error("invalid layout.conf: {0}")]
    InvalidLayout(String),

    /// Invalid or unparsable `Manifest` file.
    #[error("invalid Manifest: {0}")]
    InvalidManifest(String),

    /// The path does not point to a valid ebuild repository.
    #[error("not a valid repository: {0}")]
    InvalidRepository(PathBuf),

    /// Error parsing a package atom.
    #[error("atom parse error: {0}")]
    Atom(#[from] portage_atom::Error),

    /// Error from the metadata cache parser. Kept structured (not
    /// stringified) so a `SRC_URI`/`LICENSE`/etc. parse failure's
    /// [`portage_metadata::ParseDiagnostic`] survives to whatever actually
    /// displays this error — see [`Error::parse_diagnostic`].
    #[error("metadata error: {0}")]
    Metadata(#[from] portage_metadata::Error),

    /// Invalid profile directory or contents.
    #[error("invalid profile: {0}")]
    InvalidProfile(String),

    /// Error from the embedded bash shell.
    #[error("shell error: {0}")]
    Shell(String),

    /// Hash or size mismatch when verifying a file against a Manifest entry.
    #[error("manifest verify failed for {path}: {reason}")]
    ManifestVerifyFailed {
        /// Path to the file that failed verification.
        path: PathBuf,
        /// Human-readable description of the mismatch.
        reason: String,
    },

    /// Invalid or unparsable `metadata.xml` file.
    #[error("invalid metadata.xml: {0}")]
    InvalidMetadataXml(String),

    /// [`crate::RepositoryBuilder`] was opened without configuring a secondary cache.
    #[error("repository builder: secondary metadata cache not configured")]
    BuilderMissingSecondary,
}

impl Error {
    /// The structured parse diagnostic behind this error, if it wraps one of
    /// portage-metadata's four grammar-parse variants. Call
    /// [`portage_metadata::ParseDiagnostic::render`] on the result for the
    /// full miette code frame, at the point of actually displaying it.
    pub fn parse_diagnostic(&self) -> Option<&portage_metadata::ParseDiagnostic> {
        match self {
            Error::Metadata(e) => e.parse_diagnostic(),
            _ => None,
        }
    }
}

/// Result type for portage-repo operations.
pub type Result<T> = std::result::Result<T, Error>;
