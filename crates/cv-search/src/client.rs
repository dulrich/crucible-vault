//! HTTP client for the search companion (port of `https://github.com/dulrich/obsidian-crucible/blob/master/src/search/client.ts`,
//! contract `docs/search-companion.md:222-240`).
//!
//! **Shadow safety is structural.** The client is split in two:
//! - [`CompanionReader`] — `GET /health`, `POST /v1/paths`, `POST /v1/files/state`: reads only;
//! - [`CompanionWriter`] — `POST /v1/chunks/upsert`, `POST /v1/chunks/delete`: obtainable only
//!   through [`CompanionReader::into_writer`], which consumes a [`LiveProof`]. A proof is minted
//!   by [`LiveProof::from_shadow_flag`] from the vault's shadow flag, and only when it says live.
//!
//! Every request carries `vaultId` (the companion keys storage on `vaultId + chunkId` /
//! `vaultId + path`, `docs/search-companion.md:240`). Timeouts are bounded: 15 s for the health
//! probe (`SEARCH_BACKGROUND_PROBE_TIMEOUT_MS`, `client.ts:13`), 60 s for every index operation
//! (`SEARCH_SERVICE_INDEX_TIMEOUT_MS`, `client.ts:25`), 5 s to connect.
//!
//! Error mapping follows `client.ts:316-360`: transport failure / timeout / 5xx → retryable
//! [`SearchError::Unavailable`]; any other non-2xx → [`SearchError::Http`] (a request bug,
//! never retried). The schema gate is stricter than Crucible's (`client.ts:119-128`, which
//! returns a degraded `ok: false` session): a reported schema other than
//! [`REQUIRED_SCHEMA_VERSION`] — or none at all — is [`SearchError::Schema`], a hard failure.
//!
//! Dropped (Obsidian-induced): `requestUrl` + its `throw: false` workaround, the interactive
//! `fetch` transport and its CORS fallback latch, `resetIndex` and `/v1/search` (not a feed
//! concern), and the `CompanionAvailabilityGate` latch (the daemon's service-health breaker owns
//! deferral).

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::chunker::SearchChunk;
use crate::error::{SearchError, UnavailableKind};
use crate::js::{js, write_json_string};

/// The companion schema this build speaks (`SEARCH_REQUIRED_SCHEMA_VERSION`).
pub const REQUIRED_SCHEMA_VERSION: i64 = 7;
/// Crucible's default `searchServiceUrl` (`src/types.ts:910`).
pub const DEFAULT_COMPANION_URL: &str = "http://127.0.0.1:4801";
/// Health probe budget.
pub const HEALTH_TIMEOUT: Duration = Duration::from_secs(15);
/// Index-operation budget.
pub const INDEX_TIMEOUT: Duration = Duration::from_secs(60);
/// Chunks per upsert request (`SearchManager` flushes ~500 chunks per request).
pub const UPSERT_BATCH_CHUNKS: usize = 500;

/// `GET /health`, the fields the feed reads.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    /// `ok !== false`.
    #[serde(default = "yes")]
    pub ok: bool,
    /// Companion schema version.
    pub schema_version: Option<i64>,
    /// Model that produced the stored vectors (when every embedded chunk agrees).
    pub embedding_model: Option<String>,
    /// Stored vector width.
    pub embedding_dim: Option<u64>,
    /// Companion message.
    pub message: Option<String>,
}

fn yes() -> bool {
    true
}

/// One `POST /v1/paths` row.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedPath {
    /// Path.
    #[serde(default)]
    pub path: String,
    /// Majority content hash.
    pub content_hash: Option<String>,
    /// Chunks stored.
    #[serde(default)]
    pub chunk_count: u64,
    /// Chunks with vectors.
    #[serde(default)]
    pub embedded_count: u64,
}

/// One `POST /v1/files/state` row.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileState {
    /// Path.
    #[serde(default)]
    pub path: String,
    /// Stored content hash.
    pub content_hash: Option<String>,
    /// Chunks stored.
    pub chunk_count: Option<u64>,
    /// Every chunk embedded (fail-closed on the companion side).
    pub has_embeddings: Option<bool>,
    /// The single embedding space of the path's vectors, if unambiguous.
    pub embedding_space: Option<String>,
}

#[derive(Deserialize)]
struct PathsResponse {
    #[serde(default)]
    paths: Vec<IndexedPath>,
}

#[derive(Deserialize)]
struct FilesResponse {
    #[serde(default)]
    files: Vec<FileState>,
}

/// Proof that the vault runs live (not shadow). The only way to reach the mutating half.
#[derive(Debug)]
pub struct LiveProof(());

impl LiveProof {
    /// `Some` only when `is_shadow` is `false`. Callers pass the vault filesystem's own flag
    /// (`VaultFs::is_shadow`), read fail-closed (unknown → treat as shadow).
    #[must_use]
    pub fn from_shadow_flag(is_shadow: bool) -> Option<Self> {
        (!is_shadow).then_some(Self(()))
    }
}

/// The read-only half.
#[derive(Debug, Clone)]
pub struct CompanionReader {
    http: reqwest::Client,
    base: String,
    vault_id: String,
}

impl CompanionReader {
    /// A reader for `base_url` (trailing `/` dropped) and `vault_id`.
    pub fn new(base_url: &str, vault_id: &str) -> Result<Self, SearchError> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| SearchError::Malformed(format!("http client: {e}")))?;
        let base = if base_url.trim().is_empty() {
            DEFAULT_COMPANION_URL
        } else {
            base_url.trim()
        };
        Ok(Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            vault_id: vault_id.to_owned(),
        })
    }

    /// The vault id every request carries.
    #[must_use]
    pub fn vault_id(&self) -> &str {
        &self.vault_id
    }

    async fn send(
        &self,
        path: &str,
        body: Option<String>,
        timeout: Duration,
    ) -> Result<Value, SearchError> {
        let url = format!("{}{path}", self.base);
        let req = match body {
            Some(b) => self
                .http
                .post(&url)
                .header("content-type", "application/json")
                .body(b),
            None => self.http.get(&url),
        };
        let resp = req.timeout(timeout).send().await.map_err(|e| {
            let kind = if e.is_timeout() {
                UnavailableKind::Timeout
            } else {
                UnavailableKind::Refused
            };
            SearchError::Unavailable {
                kind,
                message: format!("{path} unreachable: {e}"),
            }
        })?;
        let status = resp.status().as_u16();
        let text = resp.text().await.map_err(|e| SearchError::Unavailable {
            kind: UnavailableKind::Refused,
            message: format!("{path} body: {e}"),
        })?;
        if status >= 500 {
            return Err(SearchError::Unavailable {
                kind: UnavailableKind::ServerError,
                message: format!("{path} returned {status}"),
            });
        }
        if !(200..300).contains(&status) {
            return Err(SearchError::Http {
                path: path.to_owned(),
                status,
            });
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|e| SearchError::Malformed(format!("{path}: {e}")))
    }

    /// `GET /health` with the schema-7 gate: a schema other than 7 (or none) →
    /// [`SearchError::Schema`]; `ok: false` → [`SearchError::Unavailable`] (`NotOk`).
    pub async fn health(&self) -> Result<Health, SearchError> {
        let v = self.send("/health", None, HEALTH_TIMEOUT).await?;
        let h: Health = serde_json::from_value(v)
            .map_err(|e| SearchError::Malformed(format!("/health: {e}")))?;
        if h.schema_version != Some(REQUIRED_SCHEMA_VERSION) {
            return Err(SearchError::Schema {
                found: h.schema_version,
            });
        }
        if !h.ok {
            return Err(SearchError::Unavailable {
                kind: UnavailableKind::NotOk,
                message: format!(
                    "companion reports ok: false ({})",
                    h.message.as_deref().unwrap_or("no message")
                ),
            });
        }
        Ok(h)
    }

    /// `POST /v1/paths`: every indexed path for the vault.
    pub async fn list_paths(&self) -> Result<Vec<IndexedPath>, SearchError> {
        let body = json!({ "vaultId": self.vault_id }).to_string();
        let v = self.send("/v1/paths", Some(body), INDEX_TIMEOUT).await?;
        let r: PathsResponse = serde_json::from_value(v)
            .map_err(|e| SearchError::Malformed(format!("/v1/paths: {e}")))?;
        Ok(r.paths.into_iter().filter(|p| !p.path.is_empty()).collect())
    }

    /// `POST /v1/files/state` for `paths` (deduplicated, blank dropped; batched).
    pub async fn file_states(
        &self,
        paths: &[String],
    ) -> Result<HashMap<String, FileState>, SearchError> {
        let mut unique: Vec<&String> = paths.iter().filter(|p| !p.trim().is_empty()).collect();
        unique.sort();
        unique.dedup();
        let mut out = HashMap::new();
        for batch in unique.chunks(UPSERT_BATCH_CHUNKS) {
            let body = json!({ "vaultId": self.vault_id, "paths": batch }).to_string();
            let v = self
                .send("/v1/files/state", Some(body), INDEX_TIMEOUT)
                .await?;
            let r: FilesResponse = serde_json::from_value(v)
                .map_err(|e| SearchError::Malformed(format!("/v1/files/state: {e}")))?;
            for f in r.files.into_iter().filter(|f| !f.path.is_empty()) {
                out.insert(f.path.clone(), f);
            }
        }
        Ok(out)
    }

    /// The mutating half — requires a [`LiveProof`].
    #[must_use]
    pub fn into_writer(self, _proof: LiveProof) -> CompanionWriter {
        CompanionWriter { reader: self }
    }
}

/// The mutating half (live mode only).
#[derive(Debug, Clone)]
pub struct CompanionWriter {
    reader: CompanionReader,
}

impl CompanionWriter {
    /// The read half.
    #[must_use]
    pub fn reader(&self) -> &CompanionReader {
        &self.reader
    }

    /// `POST /v1/chunks/upsert` — a full per-path replace on the companion (`upsert.mjs:192`),
    /// so stale chunk ids of every path sent are dropped. Batched by whole paths.
    pub async fn upsert_chunks(&self, chunks: &[SearchChunk]) -> Result<(), SearchError> {
        let mut start = 0;
        while start < chunks.len() {
            let mut end = (start + UPSERT_BATCH_CHUNKS).min(chunks.len());
            // Never split one path across requests (the companion's atomicity is per request).
            while end < chunks.len() && chunks[end].path == chunks[end - 1].path {
                end += 1;
            }
            let mut body = String::from("{\"vaultId\":");
            write_json_string(&mut body, &js(&self.reader.vault_id));
            body.push_str(",\"chunks\":[");
            for (i, c) in chunks[start..end].iter().enumerate() {
                if i > 0 {
                    body.push(',');
                }
                body.push_str(&c.to_json());
            }
            body.push_str("]}");
            self.reader
                .send("/v1/chunks/upsert", Some(body), INDEX_TIMEOUT)
                .await?;
            start = end;
        }
        Ok(())
    }

    /// `POST /v1/chunks/delete` — drops every chunk of `paths` for the vault.
    pub async fn delete_paths(&self, paths: &[String]) -> Result<(), SearchError> {
        for batch in paths.chunks(UPSERT_BATCH_CHUNKS) {
            let body = json!({ "vaultId": self.reader.vault_id, "paths": batch }).to_string();
            self.reader
                .send("/v1/chunks/delete", Some(body), INDEX_TIMEOUT)
                .await?;
        }
        Ok(())
    }
}
