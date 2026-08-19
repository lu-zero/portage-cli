use camino::Utf8PathBuf;
use thiserror::Error;

/// Errors that can occur when reading the VDB
#[derive(Error, Debug)]
pub enum Error {
    /// The VDB root directory does not exist or is not a directory
    #[error("VDB root not found: {0}")]
    RootNotFound(Utf8PathBuf),

    /// A package directory is malformed (e.g. missing required files)
    #[error("malformed package directory {path}: {reason}")]
    MalformedPackage {
        /// Path to the malformed package directory (relative to VDB root)
        path: Utf8PathBuf,
        /// Explanation of the malformation (e.g. "missing CONTENTS")
        reason: String,
    },

    /// An I/O error occurred reading a VDB file
    #[error("I/O error reading {path}: {source}")]
    Io {
        /// Path to the file that could not be read
        path: Utf8PathBuf,
        /// The underlying I/O error (source)
        source: std::io::Error,
    },
}
