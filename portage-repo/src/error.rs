use std::path::PathBuf;

/// Error type for portage-repo operations
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// I/O error with associated filesystem path
    #[error("I/O error at {path}: {source}")]
    Io {
        /// The filesystem path that caused the error
        path: PathBuf,
        /// The underlying I/O error
        source: std::io::Error,
    },

    /// Invalid or unparsable `metadata/layout.conf`
    #[error("invalid layout.conf: {0}")]
    InvalidLayout(String),

    /// Invalid or unparsable `Manifest` file
    #[error("invalid Manifest: {0}")]
    InvalidManifest(String),

    /// The path does not point to a valid ebuild repository
    #[error("not a valid repository: {0}")]
    InvalidRepository(PathBuf),

    /// Error parsing a package atom
    #[error("atom parse error: {0}")]
    Atom(#[from] portage_atom::Error),

    /// Error from the metadata cache parser
    ///
    /// Kept structured (not stringified) so a `SRC_URI`/`LICENSE`/etc.
    /// parse failure's [`portage_metadata::ParseDiagnostic`] survives to
    /// whatever actually displays this error — see
    /// [`Error::parse_diagnostic`].
    #[error("metadata error: {0}")]
    Metadata(#[from] portage_metadata::Error),

    /// Invalid profile directory or contents
    #[error("invalid profile: {0}")]
    InvalidProfile(String),

    /// No set of this name is backed by anything this resolver can read
    ///
    /// No `etc/portage/sets/<name>` file and no `sets.conf` section naming
    /// one. A caller that knows the name *is* declared somewhere (a
    /// `sets.conf` section defining a Portage `class`, say) can report it
    /// as unimplemented rather than unknown.
    #[error("no set named @{0}")]
    UnknownSet(String),

    /// Error from the embedded bash shell
    #[error("shell error: {0}")]
    Shell(String),

    /// Hash or size mismatch when verifying a file against a Manifest entry
    #[error("manifest verify failed for {path}: {reason}")]
    ManifestVerifyFailed {
        /// Path to the file that failed verification
        path: PathBuf,
        /// Human-readable description of the mismatch
        reason: String,
    },

    /// Invalid or unparsable `metadata.xml` file
    #[error("invalid metadata.xml: {0}")]
    InvalidMetadataXml(String),

    /// [`crate::RepositoryBuilder`] was opened without configuring a secondary cache
    #[error("repository builder: secondary metadata cache not configured")]
    BuilderMissingSecondary,

    /// Failed to write a sourced cache entry (regen path)
    #[error("cache write failed: {0}")]
    CacheWrite(String),

    /// A source worker terminated unexpectedly
    #[error("source worker failed: {0}")]
    SourceWorker(String),
}

impl Error {
    /// The structured parse diagnostic behind this error, if any
    ///
    /// Present if it wraps one of portage-metadata's four grammar-parse
    /// variants. UI code renders the result as a miette code frame at
    /// display time.
    pub fn parse_diagnostic(&self) -> Option<&portage_metadata::ParseDiagnostic> {
        match self {
            Error::Metadata(e) => e.parse_diagnostic(),
            _ => None,
        }
    }
}

/// Result type for portage-repo operations
pub type Result<T> = std::result::Result<T, Error>;

/// `Ok` when `result` is status 0 and control flow is normal.
///
/// A non-zero status, or a `return` / `exit` / `break` / `continue` that
/// escaped the script, is a shell error named by `what`.
pub(crate) fn require_completed(what: &str, result: brush_core::ExecutionResult) -> Result<()> {
    if result.is_success() && result.is_normal_flow() {
        Ok(())
    } else {
        Err(shell_stopped(what, &result))
    }
}

/// `Ok` when `result` did not `return`, `exit`, `break`, or `continue`.
///
/// A non-zero status with normal flow is not an error. The phase runner
/// ignores a phase function's trailing status and aborts through `die`.
pub(crate) fn require_normal_flow(what: &str, result: &brush_core::ExecutionResult) -> Result<()> {
    if result.is_normal_flow() {
        Ok(())
    } else {
        Err(shell_stopped(what, result))
    }
}

fn shell_stopped(what: &str, result: &brush_core::ExecutionResult) -> Error {
    Error::Shell(stopped_message(what, result))
}

/// How `result` stopped `what`, for a script that had to finish at status 0 with normal flow.
pub(crate) fn stopped_message(what: &str, result: &brush_core::ExecutionResult) -> String {
    let status = u8::from(result.exit_code);
    if result.is_exit() {
        format!("{what} exited {status}")
    } else if result.is_return_or_exit() {
        format!("{what} returned {status}")
    } else if !result.is_normal_flow() {
        format!("{what} interrupted the shell")
    } else {
        format!("{what} returned status {status}")
    }
}
