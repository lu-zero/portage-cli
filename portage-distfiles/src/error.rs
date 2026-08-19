/// Error type for distfile fetch and verification operations
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP {status} fetching {url}")]
    Http { url: String, status: u16 },

    #[error("network error fetching {url}: {source}")]
    Network { url: String, source: reqwest::Error },

    #[error("I/O error at {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        source: std::io::Error,
    },

    #[error("manifest verification failed for {filename}: {reason}")]
    Verify { filename: String, reason: String },

    #[error("all fetch attempts failed for {filename}")]
    AllFailed { filename: String },

    #[error("fetch command exited with status {code}: {command}")]
    Command { command: String, code: i32 },

    #[error("fetch command failed to start: {source}")]
    CommandSpawn { source: std::io::Error },

    #[error("manifest error: {0}")]
    Manifest(String),

    #[error("failed to parse mirror list: {0}")]
    MirrorParse(String),

    #[error("binhost {url} returned 304 (Not Modified) with no local cache to revalidate")]
    StaleNotModified { url: String },
}

pub type Result<T> = std::result::Result<T, Error>;
