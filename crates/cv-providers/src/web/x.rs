//! X oEmbed client (plan `v0-automation-pipelines` WP-5).
//!
//! Port of Crucible `requestXOembed` (`https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/utils/xApi.ts:114-148`,
//! payload parse `:79-99`): unauthenticated `GET <base>?url=<canonical>&omit_script=true&dnt=true`.
//! Classification: 404/403 → [`FetchError::NotFound`] (the post is gone — the step tombstones it,
//! never the breaker); 429/5xx/network → `Transient`; a 200 body that is not JSON or lacks
//! `url`/`html` → `Transient` (Crucible `server-error`: the endpoint answered, just not usably).
//! Dropped as Obsidian-induced: `requestUrl`'s `throw: false` dance.

use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use super::{FetchError, FetchRequest, WebClient};

/// Breaker / usage key.
pub const SERVICE: &str = "x-oembed";
/// The live endpoint.
pub const DEFAULT_BASE: &str = "https://publish.x.com/oembed";

/// The oEmbed fields the metadata note uses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OEmbed {
    pub url: String,
    pub author_name: String,
    pub author_url: String,
    pub html: String,
    /// oEmbed `type`.
    pub kind: String,
    pub version: String,
}

impl From<OEmbed> for cv_ingest::x::XPost {
    fn from(o: OEmbed) -> Self {
        Self {
            author_name: o.author_name,
            author_url: o.author_url,
            html: o.html,
            kind: o.kind,
            version: o.version,
        }
    }
}

/// 403 is a deleted/private post, like 404 (`xApi.ts:125-127`).
fn classify(
    service: &str,
    status: u16,
    _retry_after: Option<Duration>,
    _body: &[u8],
) -> Option<FetchError> {
    (status == 403).then(|| FetchError::NotFound {
        service: service.to_owned(),
        status,
    })
}

#[derive(Deserialize)]
struct Raw {
    url: Option<Value>,
    html: Option<Value>,
    author_name: Option<Value>,
    author_url: Option<Value>,
    #[serde(rename = "type")]
    kind: Option<Value>,
    version: Option<Value>,
}

fn string(v: Option<Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) => Some(s),
        _ => None,
    }
}

/// Parses a 200 body (`parseXOembedPayload`): unknown keys ignored, non-string optional fields
/// become empty, a missing/non-string `url` or `html` is malformed (→ `Transient`).
pub fn parse(body: &[u8]) -> Result<OEmbed, FetchError> {
    let bad = || FetchError::Transient {
        service: SERVICE.to_owned(),
        retry_after: None,
        // Retried like a network error (the pre-WP-R9 string fallback mapped it to Timeout).
        kind: crate::failure::FailureKind::Timeout,
        detail: "malformed oEmbed response".into(),
    };
    let r: Raw = serde_json::from_slice(body).map_err(|_| bad())?;
    let (Some(url), Some(html)) = (string(r.url), string(r.html)) else {
        return Err(bad());
    };
    Ok(OEmbed {
        url,
        html,
        author_name: string(r.author_name).unwrap_or_default(),
        author_url: string(r.author_url).unwrap_or_default(),
        kind: string(r.kind).unwrap_or_default(),
        version: string(r.version).unwrap_or_default(),
    })
}

/// Fetches the oEmbed of `status_url` (a canonical status URL) from `base` (tests point it at a
/// local server; production passes [`DEFAULT_BASE`]).
pub async fn oembed(
    client: &WebClient,
    base: &str,
    status_url: &str,
) -> Result<OEmbed, FetchError> {
    let url = reqwest::Url::parse_with_params(
        base,
        &[
            ("url", status_url),
            ("omit_script", "true"),
            ("dnt", "true"),
        ],
    )
    .map_err(|e| FetchError::Malformed {
        service: SERVICE.to_owned(),
        detail: format!("invalid url: {e}"),
    })?
    .to_string();
    let mut req = FetchRequest::get(SERVICE, &url);
    req.classify = Some(classify);
    let body = client.fetch(req).await?;
    parse(&body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::WebConfig;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn serve(status: u16, body: &'static str) -> String {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = l.accept().await {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let r = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(r.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        format!("http://{addr}/oembed")
    }

    #[tokio::test]
    async fn decodes_and_classifies() {
        let c = WebClient::new(WebConfig::default()).unwrap();
        let u = "https://x.com/a/status/1";
        let base = serve(
            200,
            r#"{"url":"https://x.com/a/status/1","author_name":"A","author_url":"https://x.com/a","html":"<p>hi</p>","type":"rich","version":"1.0","extra":3}"#,
        )
        .await;
        let o = oembed(&c, &base, u).await.unwrap();
        assert_eq!((o.author_name.as_str(), o.kind.as_str()), ("A", "rich"));
        assert_eq!(o.html, "<p>hi</p>");
        for st in [403, 404] {
            let e = oembed(&c, &serve(st, "").await, u).await.unwrap_err();
            assert!(matches!(e, FetchError::NotFound { status, .. } if status == st));
        }
        for st in [429, 503] {
            let e = oembed(&c, &serve(st, "").await, u).await.unwrap_err();
            assert!(matches!(e, FetchError::Transient { .. }), "{st}");
        }
        let e = oembed(&c, &serve(200, r#"{"html":"x"}"#).await, u)
            .await
            .unwrap_err();
        assert!(matches!(e, FetchError::Transient { .. }));
    }
}
