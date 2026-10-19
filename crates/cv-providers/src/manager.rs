//! `ProviderManager`: dispatch by kind, completion concurrency limiting, HTTP transport.
//!
//! Carried from Crucible `https://github.com/dulrich/obsidian-crucible/blob/master/src/providers.ts:111-300` (`ProviderManager`,
//! `HTTP_PROVIDER_CLIENTS`, the rerank fallback route). Completion-class calls ride the
//! per-provider limiter (`src/providers/AGENTS.md:16`); `embed` and native `rerank` are exempt;
//! the completion-based rerank fallback is a completion and is limited. Dropped
//! (Obsidian-induced / inventory #23): `requestUrl`, the session `describeModel`/model-list caches
//! and catalog probes.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::cli::run_cli_completion;
use crate::config::{DEFAULT_MAX_TOKENS, ProviderConfig, ProviderKind};
use crate::error::{ProviderError, Result};
use crate::limiter::{ConcurrencyLimiter, resolve_concurrency_limit};
use crate::secrets::{SecretSource, resolve_api_key};
pub use crate::wire::ImagePart;
use crate::wire::{self, Completion, Embeddings, FinishReason, RerankItem, WireRequest};

/// One completion request. `max_tokens` falls back to the provider's, then 4096 — it is always
/// sent (`src/providers/AGENTS.md:18`).
#[derive(Debug, Clone, Default)]
pub struct CompletionRequest {
    pub system: String,
    pub user: String,
    pub max_tokens: Option<u32>,
    /// Images attached to the user turn (plan `v0-automation-pipelines` WP-6); empty = the
    /// text-only wire shape, byte for byte. A CLI provider cannot take images (`Unsupported`).
    pub images: Vec<ImagePart>,
}

pub struct ProviderManager {
    http: reqwest::Client,
    limiter: Arc<ConcurrencyLimiter>,
    secrets: Arc<dyn SecretSource>,
}

/// Connect timeout of the provider HTTP client (SR8).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Per-request timeout of an HTTP provider whose `timeout_seconds` is unset (SR8).
pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 600;

/// The per-request timeout of `p`: its `timeout_seconds`, else [`DEFAULT_REQUEST_TIMEOUT_SECS`].
#[must_use]
pub fn request_timeout(p: &ProviderConfig) -> Duration {
    Duration::from_secs(
        p.timeout_seconds
            .filter(|s| *s > 0)
            .unwrap_or(DEFAULT_REQUEST_TIMEOUT_SECS),
    )
}

async fn post(
    http: &reqwest::Client,
    api: &'static str,
    timeout: Duration,
    req: WireRequest,
) -> Result<Value> {
    let mut b = http.post(&req.url).json(&req.body).timeout(timeout);
    for (k, v) in &req.headers {
        b = b.header(k, v);
    }
    let resp = b.send().await?;
    let status = resp.status().as_u16();
    let text = resp.text().await?;
    if status != 200 {
        // The body may echo the request (prompt/note text): debug log only, never the error.
        let body: String = text.chars().take(300).collect();
        tracing::debug!(api, status, body = %body, "provider non-200");
        return Err(ProviderError::Http { api, status });
    }
    serde_json::from_str(&text)
        .map_err(|e| ProviderError::Malformed(format!("{api} returned invalid JSON: {e}")))
}

impl ProviderManager {
    pub fn new(secrets: Arc<dyn SecretSource>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()?;
        Ok(Self {
            http,
            limiter: Arc::new(ConcurrencyLimiter::new()),
            secrets,
        })
    }

    pub fn limiter(&self) -> &ConcurrencyLimiter {
        &self.limiter
    }

    /// Completion-class call, gated by the provider's concurrency limit. The in-flight request
    /// owns the slot; a caller that drops/times out this future does not free it early.
    pub async fn complete(
        &self,
        p: &ProviderConfig,
        model: &str,
        req: CompletionRequest,
    ) -> Result<Completion> {
        let max_tokens = req
            .max_tokens
            .or(p.max_tokens)
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_MAX_TOKENS);
        let limit = resolve_concurrency_limit(p);
        let (p2, model) = (p.clone(), model.to_owned());
        if p.kind == ProviderKind::Cli {
            if !req.images.is_empty() {
                return Err(ProviderError::Unsupported {
                    kind: "cli",
                    capability: "take image input",
                });
            }
            let fut = async move {
                let text = run_cli_completion(&p2, &model, &req.system, &req.user).await?;
                Ok(Completion {
                    text,
                    finish_reason: FinishReason::Stop,
                    raw_finish_reason: None,
                })
            };
            return self.limiter.run(&p.id, limit, fut).await;
        }
        let key = resolve_api_key(p, self.secrets.as_ref())?;
        let wire = wire::build_completion(
            p,
            key.as_deref(),
            &model,
            &req.system,
            &req.user,
            &req.images,
            max_tokens,
        )?;
        let http = self.http.clone();
        let kind = p.kind;
        let timeout = request_timeout(p);
        let fut = async move {
            let v = post(&http, "completion API", timeout, wire).await?;
            wire::parse_completion(kind, &v)
        };
        self.limiter.run(&p.id, limit, fut).await
    }

    /// Embeddings — never gated by the completion limiter.
    pub async fn embed(
        &self,
        p: &ProviderConfig,
        model: &str,
        inputs: &[String],
    ) -> Result<Embeddings> {
        let key = resolve_api_key(p, self.secrets.as_ref())?;
        let req = wire::build_embed(p, key.as_deref(), model, inputs)?;
        let v = post(&self.http, "embeddings API", request_timeout(p), req).await?;
        wire::parse_embed(p.kind, &v, inputs.len())
    }

    /// Rerank: native `/rerank` for openai-compatible (exempt from the limiter); every other
    /// kind scores through the completion fallback (`providerRerank.test.mjs:114`).
    pub async fn rerank(
        &self,
        p: &ProviderConfig,
        model: &str,
        query: &str,
        documents: &[String],
    ) -> Result<Vec<RerankItem>> {
        if p.kind == ProviderKind::OpenaiCompatible {
            let key = resolve_api_key(p, self.secrets.as_ref())?;
            let req = wire::build_rerank(p, key.as_deref(), model, query, documents);
            let v = post(&self.http, "rerank API", request_timeout(p), req).await?;
            return wire::normalize_rerank(&v, documents.len());
        }
        let c = self
            .complete(
                p,
                model,
                CompletionRequest {
                    system: wire::RERANK_FALLBACK_SYSTEM_PROMPT.to_owned(),
                    user: wire::rerank_fallback_user_prompt(query, documents),
                    max_tokens: None,
                    images: Vec::new(),
                },
            )
            .await?;
        wire::parse_rerank_completion_text(&c.text, documents.len())
    }
}
