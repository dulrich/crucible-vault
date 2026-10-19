//! Error type for the search feed.

/// Every failure of the feed. Messages carry no note text.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    /// The embedder is down — retryable (defer on `provider`).
    #[error("embedder {provider} unavailable: {message}")]
    EmbedderUnavailable {
        /// Provider id.
        provider: String,
        /// Detail.
        message: String,
    },
    /// The embedding configuration cannot succeed (unknown provider, 4xx, width drift).
    #[error("embedding configuration: {0}")]
    EmbedConfig(String),
    /// The index records another embedding space than the write uses (D22: a change of space
    /// is a rebuild, never a silent reset).
    #[error("search index embedding space is {recorded}, but the write uses {configured}")]
    SpaceMismatch {
        /// The index's recorded space (`model/dim`).
        recorded: String,
        /// The space of the write (`model/dim`).
        configured: String,
    },
    /// The local `semsearch` index failed (open, SQL, input).
    #[error("search index: {0}")]
    Index(String),
    /// A response or input did not have the expected shape.
    #[error("malformed: {0}")]
    Malformed(String),
}

fn space_label(s: &semsearch::Space) -> String {
    format!("{}/{}d", s.model, s.dim)
}

impl From<semsearch::Error> for SearchError {
    fn from(e: semsearch::Error) -> Self {
        match e {
            semsearch::Error::SpaceMismatch { recorded, got } => Self::SpaceMismatch {
                recorded: space_label(&recorded),
                configured: space_label(&got),
            },
            other => Self::Index(other.to_string()),
        }
    }
}
