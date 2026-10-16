//! Importance scorer (plan `v0-automation-pipelines` WP-10, D5: no ML stack here).
//!
//! [`ImportanceScorer::Stub`] never scores; [`ImportanceScorer::SystemOne`] POSTs the SystemOne
//! router body (`../inference-server/router/AGENTS.md`, patch 0013):
//! `{"model":"<provider>/<model-id>","state":"…","questions":{"importance":"…"}}`.
//!
//! **Provisional response contract** (no response shape is defined upstream yet; for
//! `../eval-tool/` to implement): a JSON object with a numeric top-level `score`. Anything
//! else is [`FetchError::Malformed`]. Callers treat every error as "no score" — a scorer failure
//! never blocks intake.

use std::time::Duration;

use serde_json::{Value, json};

use super::{FetchError, FetchRequest, WebClient};

/// Breaker / log key.
pub const SERVICE: &str = "scorer";
/// Per-request timeout (scoring sits on the intake path).
pub const TIMEOUT: Duration = Duration::from_secs(10);
/// The single question asked.
pub const IMPORTANCE_QUESTION: &str = "<0-4 importance of this item to the user>";

/// The item being scored.
#[derive(Debug, Clone, Copy)]
pub struct ScoreItem<'a> {
    /// `youtube` | `blog`.
    pub kind: &'a str,
    /// Source display name.
    pub source_name: &'a str,
    /// Item title.
    pub title: &'a str,
    /// Item URL.
    pub url: &'a str,
}

/// Why a configuration fell back to the stub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StubReason {
    /// No `[scorer] url`.
    Unconfigured,
    /// `url` set, `model` missing.
    MissingModel,
}

/// The scorer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportanceScorer {
    /// Never scores.
    Stub,
    /// SystemOne-shaped endpoint.
    SystemOne {
        /// Full POST URL.
        url: String,
        /// `<provider>/<model-id>`.
        model: String,
    },
}

impl ImportanceScorer {
    /// From `[scorer] url` / `model`: no url → stub; url without model → stub (reason returned).
    #[must_use]
    pub fn from_config(url: Option<&str>, model: Option<&str>) -> (Self, Option<StubReason>) {
        let url = url.map(str::trim).filter(|s| !s.is_empty());
        let model = model.map(str::trim).filter(|s| !s.is_empty());
        match (url, model) {
            (None, _) => (Self::Stub, Some(StubReason::Unconfigured)),
            (Some(_), None) => (Self::Stub, Some(StubReason::MissingModel)),
            (Some(u), Some(m)) => (
                Self::SystemOne {
                    url: u.to_owned(),
                    model: m.to_owned(),
                },
                None,
            ),
        }
    }

    /// The compact `state` text of `item`.
    #[must_use]
    pub fn state_text(item: &ScoreItem<'_>) -> String {
        format!(
            "kind: {}\nsource: {}\ntitle: {}\nurl: {}",
            item.kind, item.source_name, item.title, item.url
        )
    }

    /// The 0013 request body.
    #[must_use]
    pub fn request_body(model: &str, item: &ScoreItem<'_>) -> Value {
        json!({
            "model": model,
            "state": Self::state_text(item),
            "questions": { "importance": IMPORTANCE_QUESTION },
        })
    }

    /// Decodes the provisional response (`{"score": <number>}`).
    pub fn parse_score(body: &[u8]) -> Result<f64, FetchError> {
        let v: Value = super::decode_json(SERVICE, body)?;
        v.get("score")
            .and_then(Value::as_f64)
            .filter(|s| s.is_finite())
            .ok_or_else(|| FetchError::Malformed {
                service: SERVICE.to_owned(),
                detail: "response has no numeric `score`".into(),
            })
    }

    /// Scores `item`: `Stub` → `Ok(None)`.
    pub async fn score(
        &self,
        client: &WebClient,
        item: &ScoreItem<'_>,
    ) -> Result<Option<f64>, FetchError> {
        let Self::SystemOne { url, model } = self else {
            return Ok(None);
        };
        let mut req = FetchRequest::get(SERVICE, url);
        req.json_body = Some(Self::request_body(model, item));
        req.timeout = Some(TIMEOUT);
        let body = client.fetch(req).await?;
        Self::parse_score(&body).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_body_and_response() {
        assert_eq!(
            ImportanceScorer::from_config(None, Some("m")),
            (ImportanceScorer::Stub, Some(StubReason::Unconfigured))
        );
        assert_eq!(
            ImportanceScorer::from_config(Some("http://x"), None).1,
            Some(StubReason::MissingModel)
        );
        let item = ScoreItem {
            kind: "blog",
            source_name: "B",
            title: "T",
            url: "https://b/p",
        };
        let b = ImportanceScorer::request_body("local/m", &item);
        assert_eq!(b["model"], "local/m");
        assert!(b["state"].as_str().unwrap().contains("title: T"));
        assert_eq!(b["questions"]["importance"], IMPORTANCE_QUESTION);
        assert_eq!(
            ImportanceScorer::parse_score(br#"{"score":3}"#).unwrap(),
            3.0
        );
        assert!(ImportanceScorer::parse_score(br#"{"score":"3"}"#).is_err());
        assert!(ImportanceScorer::parse_score(b"nope").is_err());
    }
}
