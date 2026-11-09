//! Public HTTP fetch surface for the A2 pipelines (plan `v0-automation-pipelines` WP-1).
//!
//! Every call names a `service` (the breaker key, e.g. `youtube`, `x-oembed`, `rss`) and returns
//! a classified [`FetchError`]: network/timeout/5xx/408/429 → [`FetchError::Transient`] (429's
//! `Retry-After` honoured), 404/410 → [`FetchError::NotFound`], other 4xx →
//! [`FetchError::Permanent`]. A caller can override the classification per request with a
//! [`Classifier`] (YouTube's quota-403 → [`FetchError::Quota`], WP-4). Requests to one host are
//! spaced by a minimum interval, and each has a timeout (default 30 s).
//!
//! **Secrets never reach errors or logs**: URLs may carry an API key in the query string
//! (`YOUTUBE_API_KEY`, WP-4), so reqwest errors are stripped of their URL and logs use
//! [`redact_url`]. Replaces Crucible's `requestUrl` wrappers (`https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/utils/{youtubeApi,xApi,blogsApi}.ts`); Obsidian's `requestUrl` (its CORS-free
//! fetch) is the Obsidian-induced part and is dropped.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::header::{HeaderMap, RETRY_AFTER};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::failure::{Failure, FailureKind};

pub mod fx;
pub mod scorer;
pub mod weather;
pub mod x;
pub mod youtube;

/// Default per-request timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// A classified fetch failure. Messages never contain URLs or response bodies.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
    /// Worth retrying later: network error, timeout, 5xx, 408, 429.
    #[error("{service}: transient failure ({detail})")]
    Transient {
        /// Breaker key.
        service: String,
        /// Server-requested delay, if any.
        retry_after: Option<Duration>,
        /// The typed cause (B8): `Connect`, `Timeout`, `RateLimited` or `Server`.
        kind: FailureKind,
        /// Short cause for messages (status or error kind).
        detail: String,
    },
    /// The service's quota is exhausted (only via a [`Classifier`]).
    #[error("{service}: quota exhausted")]
    Quota {
        /// Breaker key.
        service: String,
        /// When the quota resets, if known.
        retry_after: Option<Duration>,
    },
    /// A request that will never succeed as sent (4xx other than 404/408/410/429).
    #[error("{service}: HTTP {status}")]
    Permanent {
        /// Breaker key.
        service: String,
        /// HTTP status.
        status: u16,
    },
    /// 404 / 410: the resource does not exist.
    #[error("{service}: not found (HTTP {status})")]
    NotFound {
        /// Breaker key.
        service: String,
        /// HTTP status.
        status: u16,
    },
    /// A 2xx body that could not be decoded (JSON/UTF-8), or an unbuildable request.
    #[error("{service}: {detail}")]
    Malformed {
        /// Breaker key.
        service: String,
        /// Decode error (no body content).
        detail: String,
    },
}

impl FetchError {
    /// The breaker key this failure belongs to.
    #[must_use]
    pub fn service(&self) -> &str {
        match self {
            FetchError::Transient { service, .. }
            | FetchError::Quota { service, .. }
            | FetchError::Permanent { service, .. }
            | FetchError::NotFound { service, .. }
            | FetchError::Malformed { service, .. } => service,
        }
    }

    /// The typed failure (B8): `Transient` carries its kind, `Quota` → `Quota`,
    /// `NotFound`/`Permanent`/`Malformed` → `Permanent`.
    #[must_use]
    pub fn failure(&self) -> Failure {
        match self {
            FetchError::Transient {
                kind, retry_after, ..
            } => Failure {
                kind: *kind,
                retry_after: *retry_after,
            },
            FetchError::Quota { retry_after, .. } => Failure {
                kind: FailureKind::Quota,
                retry_after: *retry_after,
            },
            FetchError::Permanent { .. }
            | FetchError::NotFound { .. }
            | FetchError::Malformed { .. } => Failure::new(FailureKind::Permanent),
        }
    }
}

/// Per-request classification override: given the service, status, parsed `Retry-After` and the
/// response body, `Some(err)` replaces the default classification of a non-2xx response.
pub type Classifier = fn(&str, u16, Option<Duration>, &[u8]) -> Option<FetchError>;

/// The default classification of a non-2xx `status`.
#[must_use]
pub fn classify_status(service: &str, status: u16, retry_after: Option<Duration>) -> FetchError {
    let service = service.to_owned();
    match status {
        404 | 410 => FetchError::NotFound { service, status },
        408 | 429 | 500..=599 => FetchError::Transient {
            service,
            retry_after,
            kind: FailureKind::of_status(status),
            detail: format!("HTTP {status}"),
        },
        _ => FetchError::Permanent { service, status },
    }
}

/// A URL safe for logs: scheme, host and path only (query and fragment dropped).
#[must_use]
pub fn redact_url(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(u) => format!(
            "{}://{}{}",
            u.scheme(),
            u.host_str().unwrap_or(""),
            u.path()
        ),
        Err(_) => "<invalid url>".to_owned(),
    }
}

/// Parses a `Retry-After` value: delta-seconds or an IMF-fixdate HTTP-date
/// (`Sun, 06 Nov 1994 08:49:37 GMT`), relative to `now`. A past date is zero.
#[must_use]
pub fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let v = value.trim();
    if let Ok(secs) = v.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let at = parse_http_date(v)?;
    let now = i64::try_from(now.duration_since(UNIX_EPOCH).ok()?.as_secs()).ok()?;
    Some(Duration::from_secs(u64::try_from(at - now).unwrap_or(0)))
}

/// IMF-fixdate → Unix seconds.
fn parse_http_date(v: &str) -> Option<i64> {
    let (_, rest) = v.split_once(", ")?;
    let mut parts = rest.split(' ');
    let day: i64 = parts.next()?.parse().ok()?;
    let month = match parts.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: i64 = parts.next()?.parse().ok()?;
    let mut hms = parts.next()?.split(':');
    let h: i64 = hms.next()?.parse().ok()?;
    let m: i64 = hms.next()?.parse().ok()?;
    let s: i64 = hms.next()?.parse().ok()?;
    if parts.next()? != "GMT" || !(1..=31).contains(&day) || h > 23 || m > 59 || s > 60 {
        return None;
    }
    let days = cv_text::days_from_civil(year, month, day);
    Some(days * 86_400 + h * 3600 + m * 60 + s)
}

fn retry_after_of(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| parse_retry_after(v, SystemTime::now()))
}

/// Client settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebConfig {
    /// Per-request timeout.
    pub timeout: Duration,
    /// Minimum spacing between request starts to one host (zero = no gate).
    pub min_host_interval: Duration,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
            min_host_interval: Duration::ZERO,
        }
    }
}

/// One request (method, URL, optional JSON body, optional overrides).
#[derive(Debug, Clone)]
pub struct FetchRequest<'a> {
    /// Breaker key.
    pub service: &'a str,
    /// Full URL (may carry secrets in its query; never logged unredacted).
    pub url: &'a str,
    /// JSON body → POST; `None` → GET.
    pub json_body: Option<serde_json::Value>,
    /// Extra headers.
    pub headers: Vec<(String, String)>,
    /// Overrides [`WebConfig::timeout`].
    pub timeout: Option<Duration>,
    /// Overrides the default status classification.
    pub classify: Option<Classifier>,
}

impl<'a> FetchRequest<'a> {
    /// A GET of `url` for `service`.
    #[must_use]
    pub fn get(service: &'a str, url: &'a str) -> Self {
        Self {
            service,
            url,
            json_body: None,
            headers: Vec::new(),
            timeout: None,
            classify: None,
        }
    }
}

/// The async fetch client: one `reqwest::Client`, a timeout and a per-host interval gate.
#[derive(Debug)]
pub struct WebClient {
    http: reqwest::Client,
    config: WebConfig,
    next_slot: Mutex<HashMap<String, Instant>>,
}

impl WebClient {
    /// A client with `config`.
    pub fn new(config: WebConfig) -> Result<Self, FetchError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("crucible-vault/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| FetchError::Malformed {
                service: "web".into(),
                detail: format!("http client: {}", e.without_url()),
            })?;
        Ok(Self {
            http,
            config,
            next_slot: Mutex::new(HashMap::new()),
        })
    }

    /// Waits until `host` may be requested again and reserves the next slot.
    async fn gate(&self, host: &str) {
        if self.config.min_host_interval.is_zero() {
            return;
        }
        let wait = {
            let mut slots = self
                .next_slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = Instant::now();
            let slot = slots.get(host).copied().filter(|t| *t > now).unwrap_or(now);
            slots.insert(host.to_owned(), slot + self.config.min_host_interval);
            slot.saturating_duration_since(now)
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }

    /// Sends `req`; a 2xx returns the body bytes, anything else a classified [`FetchError`].
    pub async fn fetch(&self, req: FetchRequest<'_>) -> Result<Vec<u8>, FetchError> {
        let service = req.service;
        let url = reqwest::Url::parse(req.url).map_err(|e| FetchError::Malformed {
            service: service.to_owned(),
            detail: format!("invalid url: {e}"),
        })?;
        self.gate(url.host_str().unwrap_or("")).await;
        let mut b = match &req.json_body {
            Some(body) => self.http.post(url).json(body),
            None => self.http.get(url),
        };
        b = b.timeout(req.timeout.unwrap_or(self.config.timeout));
        for (k, v) in &req.headers {
            b = b.header(k, v);
        }
        let resp = b
            .send()
            .await
            .map_err(|e| transport_error(service, req.url, e))?;
        let status = resp.status().as_u16();
        let retry_after = retry_after_of(resp.headers());
        let body = resp
            .bytes()
            .await
            .map_err(|e| transport_error(service, req.url, e))?
            .to_vec();
        if (200..300).contains(&status) {
            return Ok(body);
        }
        tracing::debug!(service, status, url = %redact_url(req.url), "fetch non-2xx");
        if let Some(e) = req
            .classify
            .and_then(|c| c(service, status, retry_after, &body))
        {
            return Err(e);
        }
        Err(classify_status(service, status, retry_after))
    }

    /// A size- and type-limited GET (localize's attachment download): goes through the
    /// same host gate and breaker key (`service`) as [`Self::fetch`]. A non-2xx is classified
    /// as in [`Self::fetch`]. On a 2xx, a `Content-Length` over `max_bytes` fails before the
    /// body is read; `decide` sees the `Content-Type` header (`""` when absent) and returns
    /// `None` to reject the response without reading its body ([`Limited::Rejected`]) or
    /// `Some(t)` to read it; a body over `max_bytes` fails. "Too large" is
    /// [`FetchError::Malformed`] (permanent).
    pub async fn fetch_limited<T>(
        &self,
        req: FetchRequest<'_>,
        max_bytes: u64,
        decide: impl FnOnce(&str) -> Option<T>,
    ) -> Result<Limited<T>, FetchError> {
        let service = req.service;
        let url = reqwest::Url::parse(req.url).map_err(|e| FetchError::Malformed {
            service: service.to_owned(),
            detail: format!("invalid url: {e}"),
        })?;
        self.gate(url.host_str().unwrap_or("")).await;
        let mut b = self
            .http
            .get(url)
            .timeout(req.timeout.unwrap_or(self.config.timeout));
        for (k, v) in &req.headers {
            b = b.header(k, v);
        }
        let resp = b
            .send()
            .await
            .map_err(|e| transport_error(service, req.url, e))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            tracing::debug!(service, status, url = %redact_url(req.url), "fetch non-2xx");
            return Err(classify_status(
                service,
                status,
                retry_after_of(resp.headers()),
            ));
        }
        let too_large = || FetchError::Malformed {
            service: service.to_owned(),
            detail: "response too large".into(),
        };
        if resp.content_length().is_some_and(|n| n > max_bytes) {
            return Err(too_large());
        }
        let ctype = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let Some(t) = decide(&ctype) else {
            return Ok(Limited::Rejected);
        };
        let body = resp
            .bytes()
            .await
            .map_err(|e| transport_error(service, req.url, e))?;
        if body.len() as u64 > max_bytes {
            return Err(too_large());
        }
        Ok(Limited::Body(body.to_vec(), t))
    }

    /// GET → UTF-8 text.
    pub async fn get_text(&self, service: &str, url: &str) -> Result<String, FetchError> {
        let b = self.fetch(FetchRequest::get(service, url)).await?;
        String::from_utf8(b).map_err(|_| FetchError::Malformed {
            service: service.to_owned(),
            detail: "response is not UTF-8".into(),
        })
    }

    /// GET → bytes.
    pub async fn get_bytes(&self, service: &str, url: &str) -> Result<Vec<u8>, FetchError> {
        self.fetch(FetchRequest::get(service, url)).await
    }

    /// GET → JSON.
    pub async fn get_json<T: DeserializeOwned>(
        &self,
        service: &str,
        url: &str,
    ) -> Result<T, FetchError> {
        let b = self.fetch(FetchRequest::get(service, url)).await?;
        decode_json(service, &b)
    }

    /// POST a JSON body → JSON.
    pub async fn post_json<B: Serialize, T: DeserializeOwned>(
        &self,
        service: &str,
        url: &str,
        body: &B,
    ) -> Result<T, FetchError> {
        let json = serde_json::to_value(body).map_err(|e| FetchError::Malformed {
            service: service.to_owned(),
            detail: format!("request body: {e}"),
        })?;
        let mut req = FetchRequest::get(service, url);
        req.json_body = Some(json);
        let b = self.fetch(req).await?;
        decode_json(service, &b)
    }
}

/// The result of [`WebClient::fetch_limited`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Limited<T> {
    /// The body (within the size limit) and the caller's decision value.
    Body(Vec<u8>, T),
    /// `decide` rejected the response from its headers; the body was not read.
    Rejected,
}

/// A transport (reqwest) failure → `Transient`: connect → `Connect`, timeout and any other
/// network error → `Timeout`. The error text is logged without its URL, never returned.
fn transport_error(service: &str, url: &str, e: reqwest::Error) -> FetchError {
    let (kind, detail) = if e.is_timeout() {
        (FailureKind::Timeout, "timeout")
    } else if e.is_connect() {
        (FailureKind::Connect, "connect")
    } else {
        (FailureKind::Timeout, "network")
    };
    tracing::debug!(service, url = %redact_url(url), error = %e.without_url(), "fetch failed");
    FetchError::Transient {
        service: service.to_owned(),
        retry_after: None,
        kind,
        detail: detail.to_owned(),
    }
}

/// Decodes a JSON body (shared by [`WebClient::get_json`]/[`WebClient::post_json`]).
pub fn decode_json<T: DeserializeOwned>(service: &str, body: &[u8]) -> Result<T, FetchError> {
    serde_json::from_slice(body).map_err(|e| FetchError::Malformed {
        service: service.to_owned(),
        detail: format!("invalid JSON: {e}"),
    })
}

#[cfg(test)]
mod tests;
