//! Error type for the search feed.

/// Why the companion did not answer (`SearchServiceUnavailableErrorKind`, `src/search/types.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailableKind {
    /// Transport failure below HTTP.
    Refused,
    /// The client gave up waiting.
    Timeout,
    /// A 5xx answer.
    ServerError,
    /// `/health` answered `ok: false`.
    NotOk,
}

/// Every failure of the feed. Messages carry no note text.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    /// The companion is down or not ready — retryable (defer).
    #[error("search companion unavailable ({kind:?}): {message}")]
    Unavailable {
        /// Cause class.
        kind: UnavailableKind,
        /// Detail.
        message: String,
    },
    /// The companion speaks another schema than 7 — a hard failure (never a degraded session).
    #[error("search companion schema {found:?} is not the required 7")]
    Schema {
        /// Reported schema, if any.
        found: Option<i64>,
    },
    /// A non-5xx error status: a request bug, never retried.
    #[error("search companion {path} returned HTTP {status}")]
    Http {
        /// Endpoint.
        path: String,
        /// Status.
        status: u16,
    },
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
    /// A response or input did not have the expected shape.
    #[error("malformed: {0}")]
    Malformed(String),
}
