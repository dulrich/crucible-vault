//! The one typed failure taxonomy of the HTTP stacks:
//! both [`crate::web::FetchError`] and [`crate::ProviderError`] report a
//! [`Failure`], so callers map dispositions from types, never from formatted message text.

use std::time::Duration;

/// What kind of failure a request hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureKind {
    /// The host refused or could not be reached (TCP connect).
    Connect,
    /// The request timed out, HTTP 408, or a non-connect transport error of a web fetch.
    Timeout,
    /// HTTP 429.
    RateLimited,
    /// HTTP 5xx (and a non-connect, non-timeout provider transport error).
    Server,
    /// The service's quota is exhausted (a caller classifier, e.g. YouTube's quota-403).
    Quota,
    /// A missing/unreadable API key.
    Auth,
    /// Will never succeed as sent: 404/410, other 4xx, malformed bodies, config errors.
    Permanent,
}

impl FailureKind {
    /// Worth retrying later (everything but [`FailureKind::Auth`]/[`FailureKind::Permanent`]).
    #[must_use]
    pub fn is_retryable(self) -> bool {
        !matches!(self, FailureKind::Auth | FailureKind::Permanent)
    }

    /// The kind of a non-2xx HTTP `status`.
    #[must_use]
    pub fn of_status(status: u16) -> Self {
        match status {
            408 => FailureKind::Timeout,
            429 => FailureKind::RateLimited,
            500..=599 => FailureKind::Server,
            _ => FailureKind::Permanent,
        }
    }
}

/// A classified failure: its kind and the server-requested delay, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failure {
    /// The kind.
    pub kind: FailureKind,
    /// `Retry-After` (or a quota reset), if known.
    pub retry_after: Option<Duration>,
}

impl Failure {
    /// A failure of `kind` with no server-requested delay.
    #[must_use]
    pub fn new(kind: FailureKind) -> Self {
        Self {
            kind,
            retry_after: None,
        }
    }
}
