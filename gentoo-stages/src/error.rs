use thiserror::Error;

/// Stage3 fetching and management errors.
#[derive(Debug, Error)]
pub enum Error {
    /// The host architecture could not be determined.
    #[error("Cannot guess the host architecture: {0}")]
    Arch(#[from] gentoo_core::Error),

    /// An HTTP request for stage3 metadata or an image failed.
    #[error("HTTP error: {0}")]
    HttpError(#[from] reqwest::Error),

    /// The stage3 metadata listing could not be parsed.
    #[error("Failed to parse stage3 metadata: {0}")]
    ParseError(String),

    /// The requested stage3 variant is not offered for this architecture.
    #[error("Stage3 variant not found: {0}")]
    VariantNotFound(String),

    /// No stage3 image matched the requested criteria.
    #[error("Stage3 image not found")]
    NotFound,

    /// A filesystem operation failed while downloading or storing the image.
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    /// The downloaded image size did not match the size advertised in the metadata.
    #[error("Size mismatch: expected {expected} bytes, got {got}")]
    SizeMismatch {
        /// Size the metadata advertised, in bytes.
        expected: u64,
        /// Size actually downloaded, in bytes.
        got: u64,
    },
}
