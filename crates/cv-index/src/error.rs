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
    /// No valid index generation exists beside the base path: the caller runs `Index::create`.
    #[error("no index generation at {0}")]
    NoIndex(String),
    /// The requested generation's file is absent (retired, or never built): reopen the current
    /// generation.
    #[error("index generation {generation} missing at {path}")]
    GenerationMissing { generation: u64, path: String },
    /// A generation file whose stored `meta.generation` disagrees with its file number.
    #[error("index generation file {file} stores generation {stored}")]
    GenerationMismatch { file: u64, stored: u64 },
    /// `PRAGMA journal_mode` did not report `wal`.
    #[error("index journal mode is {0:?}, expected \"wal\"")]
    JournalMode(String),
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
