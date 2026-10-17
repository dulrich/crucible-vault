//! Frankfurter exchange-rate client (plan `v0-automation-pipelines` WP-9): `GET /latest`.
//!
//! Ports `https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/utils/fx.ts` `fetchFxRate` (`:34-50`): non-200
//! → a classified [`FetchError`]; a missing or non-finite `rates[quote]` → `Malformed`. Keyless.
//! Dropped as Obsidian-induced: the `_=<now>` cache-bust param and `Cache-Control: no-cache`
//! header (they defeat Electron's HTTP cache under `requestUrl`; `reqwest` keeps no cache).

use std::collections::HashMap;

use serde::Deserialize;

use super::{FetchError, WebClient, decode_json};

/// Breaker / `api_usage` key.
pub const SERVICE: &str = "frankfurter";
/// The canonical API base (Crucible: `api.frankfurter.dev/v1`; the old `.app` host 301s).
pub const DEFAULT_BASE: &str = "https://api.frankfurter.dev/v1";

/// One rate (Crucible `FxRate`).
#[derive(Debug, Clone, PartialEq)]
pub struct FxRate {
    /// Base currency.
    pub base: String,
    /// Quote currency.
    pub quote: String,
    /// Units of `quote` per one `base`.
    pub rate: f64,
    /// The reference date (`YYYY-MM-DD`).
    pub as_of: String,
}

#[derive(Debug, Deserialize)]
struct Latest {
    #[serde(default)]
    date: String,
    #[serde(default)]
    rates: HashMap<String, f64>,
}

/// Decodes a `/latest` body for `base`/`quote`.
pub fn parse_latest(base: &str, quote: &str, body: &[u8]) -> Result<FxRate, FetchError> {
    let l: Latest = decode_json(SERVICE, body)?;
    match l.rates.get(quote) {
        Some(r) if r.is_finite() => Ok(FxRate {
            base: base.to_owned(),
            quote: quote.to_owned(),
            rate: *r,
            as_of: l.date,
        }),
        _ => Err(FetchError::Malformed {
            service: SERVICE.to_owned(),
            detail: format!("{base}/{quote}: missing rate in response"),
        }),
    }
}

/// One currency (`/currencies` entry; Crucible `fx.ts:22-32` `fetchCurrencies`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Currency {
    /// ISO code (`USD`).
    pub code: String,
    /// Display name (`United States Dollar`).
    pub name: String,
}

/// Decodes a `/currencies` body (`{code: name}`) into a list sorted by code.
pub fn parse_currencies(body: &[u8]) -> Result<Vec<Currency>, FetchError> {
    let m: std::collections::BTreeMap<String, String> = decode_json(SERVICE, body)?;
    Ok(m.into_iter()
        .map(|(code, name)| Currency { code, name })
        .collect())
}

/// The Frankfurter API rooted at a base URL.
#[derive(Debug, Clone)]
pub struct FxApi {
    base: String,
}

impl Default for FxApi {
    fn default() -> Self {
        Self::new(DEFAULT_BASE)
    }
}

impl FxApi {
    /// An API rooted at `base` (no trailing slash needed).
    #[must_use]
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_owned(),
        }
    }

    /// `GET /currencies` → every supported currency, sorted by code.
    pub async fn currencies(&self, client: &WebClient) -> Result<Vec<Currency>, FetchError> {
        let body = client
            .get_bytes(SERVICE, &format!("{}/currencies", self.base))
            .await?;
        parse_currencies(&body)
    }

    /// `GET /latest?from=BASE&to=QUOTE` → the rate.
    pub async fn latest(
        &self,
        client: &WebClient,
        base: &str,
        quote: &str,
    ) -> Result<FxRate, FetchError> {
        let url = reqwest::Url::parse_with_params(
            &format!("{}/latest", self.base),
            &[("from", base), ("to", quote)],
        )
        .map_err(|e| FetchError::Malformed {
            service: SERVICE.to_owned(),
            detail: format!("url: {e}"),
        })?;
        let body = client.get_bytes(SERVICE, url.as_str()).await?;
        parse_latest(base, quote, &body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_rate_and_rejects_missing() {
        let b = br#"{"amount":1.0,"base":"USD","date":"2026-10-14","rates":{"MXN":18.2345}}"#;
        let r = parse_latest("USD", "MXN", b).unwrap();
        assert_eq!((r.rate, r.as_of.as_str()), (18.2345, "2026-10-14"));
        assert!(matches!(
            parse_latest("USD", "EUR", b),
            Err(FetchError::Malformed { .. })
        ));
    }

    #[test]
    fn decodes_currencies_sorted() {
        let b = br#"{"USD":"United States Dollar","EUR":"Euro","AUD":"Australian Dollar"}"#;
        let c = parse_currencies(b).unwrap();
        let codes: Vec<&str> = c.iter().map(|c| c.code.as_str()).collect();
        assert_eq!(codes, ["AUD", "EUR", "USD"]);
        assert_eq!(c[1].name, "Euro");
        assert!(parse_currencies(b"[1]").is_err());
    }
}
