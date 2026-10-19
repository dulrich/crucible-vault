//! Provider error type.

use crate::failure::{Failure, FailureKind};

/// Every failure a provider call can surface. Messages carry no prompt, note or response-body
/// text (a non-2xx body goes to `tracing::debug!` only).
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("providers config: {0}")]
    Config(String),
    #[error("secret: {0}")]
    Secret(String),
    #[error("{api} returned HTTP {status}")]
    Http { api: &'static str, status: u16 },
    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("{0}")]
    Malformed(String),
    #[error("provider kind {kind} cannot {capability}")]
    Unsupported {
        kind: &'static str,
        capability: &'static str,
    },
    #[error("cli provider: {0}")]
    Cli(String),
    /// A CLI provider ran past its `timeout_seconds` and was killed.
    #[error("cli provider: {command} timed out after {ms}ms")]
    CliTimeout { command: String, ms: u128 },
    #[error("request task failed: {0}")]
    Task(String),
}

impl ProviderError {
    /// The typed failure (B8). Transport: connect → `Connect`, timeout → `Timeout`, any other
    /// transport error → `Server`. HTTP: [`FailureKind::of_status`]. A secret error is `Auth`;
    /// config/decode/capability errors are `Permanent`. Provider calls carry no `retry_after`
    /// (the step's provider defer applies).
    #[must_use]
    pub fn failure(&self) -> Failure {
        let kind = match self {
            ProviderError::Transport(t) if t.is_connect() => FailureKind::Connect,
            ProviderError::Transport(t) if t.is_timeout() => FailureKind::Timeout,
            ProviderError::Transport(_) => FailureKind::Server,
            ProviderError::Http { status, .. } => FailureKind::of_status(*status),
            ProviderError::CliTimeout { .. } => FailureKind::Timeout,
            ProviderError::Secret(_) => FailureKind::Auth,
            ProviderError::Config(_)
            | ProviderError::Malformed(_)
            | ProviderError::Unsupported { .. }
            | ProviderError::Cli(_)
            | ProviderError::Task(_) => FailureKind::Permanent,
        };
        Failure::new(kind)
    }
}

pub type Result<T> = std::result::Result<T, ProviderError>;
