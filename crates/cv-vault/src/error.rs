//! Vault errors.

use crate::hash::ContentHash;

/// Everything `cv-vault` can fail with.
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    /// CAS mismatch: the file's current hash is not the one the caller read (I2). Nothing was
    /// written. `None` means "absent".
    #[error("conflict at {path}: expected {expected:?}, found {found:?}")]
    Conflict {
        path: String,
        expected: Option<ContentHash>,
        found: Option<ContentHash>,
    },
    /// The destination already exists.
    #[error("already exists: {0}")]
    Exists(String),
    /// The source does not exist.
    #[error("not found: {0}")]
    NotFound(String),
    /// Not a clean vault-relative `/` path (or a path the daemon never writes).
    #[error("invalid vault path: {0:?}")]
    InvalidPath(String),
    /// Live mode under the protected live vault without the cut-over marker (I1).
    #[error(
        "live mode refused: {root} is under the protected root {protected} and no cut-over marker is present"
    )]
    ProtectedRoot { root: String, protected: String },
    /// The scratch directory failed validation.
    #[error("scratch dir {scratch} refused: {reason}")]
    ScratchRefused { scratch: String, reason: String },
    #[error("io at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("index: {0}")]
    Index(#[from] cv_index::Error),
    #[error("state: {0}")]
    State(#[from] cv_state::StateError),
    #[error("watcher: {0}")]
    Watch(#[from] notify::Error),
    #[error("landlock: {0}")]
    Landlock(String),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// The writer thread is gone (panicked or shut down).
    #[error("writer thread is gone")]
    WriterGone,
}

/// `Result` with [`VaultError`].
pub type Result<T> = std::result::Result<T, VaultError>;

pub(crate) fn io(path: &std::path::Path, source: std::io::Error) -> VaultError {
    VaultError::Io {
        path: path.display().to_string(),
        source,
    }
}
