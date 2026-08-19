use thiserror::Error;

/// Error type for portage-atom-pubgrub
#[derive(Debug, Clone, Error)]
pub enum Error {
    /// A blocker conflict was detected in the solution
    #[error("{strength} blocker conflict: {pkg} blocks {blocker}")]
    BlockerConflict {
        /// The package declaring the blocker
        pkg: String,
        /// The blocker atom string
        blocker: String,
        /// Whether this is a weak (!) or strong (!!) blocker
        strength: &'static str,
    },

    /// A USE-dep constraint was violated in the solution
    #[error("USE-dep conflict: {0}: {1}")]
    UseDepConflict(String, String),

    /// A repository constraint was violated in the solution
    #[error("repo constraint conflict: {0}: {1}")]
    RepoConstraintConflict(String, String),
}

/// Result type for portage-atom-pubgrub operations
pub type Result<T> = std::result::Result<T, Error>;
