//! Error type for every `cv-state` operation.

/// Errors surfaced by `state.db` operations.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    /// Underlying SQLite failure.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// JSON (de)serialization of a stored column failed.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// The database was written by a newer `cv` (its `user_version` is beyond the ladder).
    /// Migrations are forward-only and state.db is primary data, so we refuse rather than guess.
    #[error("state.db schema version {found} is newer than this build supports ({supported})")]
    NewerSchema {
        /// Version found on disk.
        found: i64,
        /// Highest version this build knows.
        supported: i64,
    },
    /// The database predates the 0.1.1 baseline (D24): its `user_version` is below the
    /// baseline and the pre-baseline migration chain no longer ships.
    #[error(
        "state.db schema version {found} predates the 0.1.1 baseline (v{baseline}); upgrade it with a pre-0.1.1 cv build first"
    )]
    PreBaseline {
        /// Version found on disk.
        found: i64,
        /// The baseline version this build creates and accepts.
        baseline: i64,
    },
    /// No job with this id exists.
    #[error("no such job: {0}")]
    NoSuchJob(String),
    /// The job exists but is not in the state the operation requires.
    #[error("job {id} is {}, expected {expected}", .status.as_str())]
    WrongStatus {
        /// Job id.
        id: String,
        /// Actual status.
        status: crate::JobStatus,
        /// Status the operation required.
        expected: &'static str,
    },
    /// A stored enum column held a value this build does not recognise.
    #[error("corrupt column {column}: {value:?}")]
    Corrupt {
        /// Column name.
        column: &'static str,
        /// Offending value.
        value: String,
    },
}

/// Crate result alias.
pub type Result<T> = std::result::Result<T, StateError>;
