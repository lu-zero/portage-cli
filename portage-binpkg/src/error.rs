//! Errors for GPKG read/write operations.

use std::path::PathBuf;

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors produced while building or reading a binary package.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// I/O error while reading or writing GPKG files.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// An external tool (`tar`/`zstd`) exited non-zero.
    #[error("{tool} failed with exit code {code}")]
    Tool {
        /// Name of the external tool.
        tool: &'static str,
        /// Non-zero exit code returned by the tool.
        code: i32,
    },

    /// A path lacked an expected component (parent / file name).
    #[error("invalid path: {0}")]
    BadPath(PathBuf),

    /// The container is missing a required member or is otherwise malformed.
    #[error("corrupt or incomplete GPKG: {0}")]
    Corrupt(String),

    /// No `Packages` index file exists yet at this `PKGDIR`.
    #[error("no Packages index at {} — run `em maint binhost` first", .0.display())]
    NoIndex(PathBuf),

    /// `PKGDIR` itself does not exist.
    #[error("PKGDIR does not exist: {}", .0.display())]
    NoPkgdir(PathBuf),

    /// FEATURES=binpkg-request-signature but the container carries no
    /// signature at all.
    #[error("no OpenPGP signature on {} (FEATURES=binpkg-request-signature)", .0.display())]
    SignatureRequired(PathBuf),

    /// A signature was present but failed to verify, or a key/signing
    /// operation failed.
    ///
    /// String-wrapped rather than `#[from]`-wrapping the `pgp` crate's own
    /// error type directly, so this crate's public error surface doesn't
    /// hard-couple to `pgp`'s exact error shape across versions.
    #[error("OpenPGP signature error: {0}")]
    Signature(String),
}
