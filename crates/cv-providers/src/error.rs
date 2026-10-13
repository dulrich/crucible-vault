//! Provider error type.

/// Every failure a provider call can surface. Messages carry no prompt or note text.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("providers config: {0}")]
    Config(String),
    #[error("secret: {0}")]
    Secret(String),
    #[error("{api} returned HTTP {status}: {body}")]
    Http {
        api: &'static str,
        status: u16,
        body: String,
    },
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
    #[error("request task failed: {0}")]
    Task(String),
}

pub type Result<T> = std::result::Result<T, ProviderError>;
