//! Index errors.

/// Everything `cv-index` can fail with.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    /// The DB was built by a different schema: drop and rebuild (D14), never migrate.
    #[error("index schema mismatch: found {found:?}, expected {expected}")]
    SchemaMismatch { found: Option<u32>, expected: u32 },
    /// A path that is not a clean vault-relative `/`-separated path.
    #[error("invalid vault path: {0:?}")]
    InvalidPath(String),
    #[error("not indexed: {0}")]
    NotFound(String),
    #[error("already indexed: {0}")]
    AlreadyExists(String),
    #[error("stored prop is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// `Result` with [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn io(path: &std::path::Path, source: std::io::Error) -> Error {
    Error::Io {
        path: path.display().to_string(),
        source,
    }
}
