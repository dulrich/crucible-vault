//! Open-Meteo forecast client (plan `v0-automation-pipelines` WP-9): today's high/low/code.
//!
//! Ports `https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/utils/weather.ts` `fetchWeather` (`:91-118`):
//! `daily=temperature_2m_max,temperature_2m_min,weather_code`, Celsius, one forecast day; a
//! missing `daily` block or incomplete first entries → `Malformed`. Keyless. Geocoding (`:54-89`)
//! is ported as the on-demand [`geocode`] lookup (plan `round-5-reading-pickers-intake` WP-5). Deviation: `timezone=auto` (the place's
//! own zone) replaces Crucible's plugin-wide timezone setting, which has no equivalent here.

use serde::Deserialize;

use super::{FetchError, WebClient, decode_json};

/// Breaker / `api_usage` key.
pub const SERVICE: &str = "open-meteo";
/// The real API base.
pub const DEFAULT_BASE: &str = "https://api.open-meteo.com/v1";

/// Today's forecast for one place.
#[derive(Debug, Clone, PartialEq)]
pub struct DailyForecast {
    /// High, °C.
    pub high_c: f64,
    /// Low, °C.
    pub low_c: f64,
    /// WMO weather code.
    pub code: i64,
}

#[derive(Debug, Deserialize)]
struct Forecast {
    daily: Option<Daily>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Daily {
    temperature_2m_max: Vec<Option<f64>>,
    temperature_2m_min: Vec<Option<f64>>,
    weather_code: Vec<Option<f64>>,
}

fn malformed(detail: &str) -> FetchError {
    FetchError::Malformed {
        service: SERVICE.to_owned(),
        detail: detail.to_owned(),
    }
}

/// Decodes a `/forecast` body.
pub fn parse_forecast(body: &[u8]) -> Result<DailyForecast, FetchError> {
    let f: Forecast = decode_json(SERVICE, body)?;
    let d = f
        .daily
        .ok_or_else(|| malformed("missing 'daily' in response"))?;
    let first = |v: &[Option<f64>]| v.first().copied().flatten().filter(|x| x.is_finite());
    match (
        first(&d.temperature_2m_max),
        first(&d.temperature_2m_min),
        first(&d.weather_code),
    ) {
        (Some(high_c), Some(low_c), Some(code)) => Ok(DailyForecast {
            high_c,
            low_c,
            #[allow(clippy::cast_possible_truncation)]
            code: code as i64,
        }),
        _ => Err(malformed("incomplete daily forecast data")),
    }
}

/// The Open-Meteo geocoding API base (Crucible `weather.ts:54-89` `geocodeLocation`).
pub const GEOCODE_BASE: &str = "https://geocoding-api.open-meteo.com/v1";

/// One geocoding hit.
#[derive(Debug, Clone, PartialEq)]
pub struct GeoResult {
    /// `City, CC` (just `City` when the country code is absent).
    pub label: String,
    /// First-level admin area (state/region), if reported.
    pub admin1: Option<String>,
    /// ISO country code (`US`), possibly empty.
    pub country_code: String,
    /// Latitude.
    pub lat: f64,
    /// Longitude.
    pub lon: f64,
}

#[derive(Debug, Deserialize)]
struct GeoResponse {
    #[serde(default)]
    results: Vec<GeoHit>,
}

#[derive(Debug, Deserialize)]
struct GeoHit {
    name: String,
    #[serde(default)]
    country_code: Option<String>,
    #[serde(default)]
    admin1: Option<String>,
    latitude: f64,
    longitude: f64,
}

/// Decodes a geocoding `/search` body; an absent `results` is an empty list.
pub fn parse_geocode(body: &[u8]) -> Result<Vec<GeoResult>, FetchError> {
    let r: GeoResponse = decode_json(SERVICE, body)?;
    Ok(r.results
        .into_iter()
        .map(|h| {
            let cc = h.country_code.unwrap_or_default().to_uppercase();
            let label = if cc.is_empty() {
                h.name
            } else {
                format!("{}, {cc}", h.name)
            };
            GeoResult {
                label,
                admin1: h.admin1.filter(|a| !a.is_empty()),
                country_code: cc,
                lat: h.latitude,
                lon: h.longitude,
            }
        })
        .collect())
}

/// `GET {base}/search?name=<q>&count=10&language=en&format=json` against the geocoding API
/// rooted at `base` ([`GEOCODE_BASE`] in production). A blank query → empty, no request.
pub async fn geocode(
    client: &WebClient,
    base: &str,
    query: &str,
) -> Result<Vec<GeoResult>, FetchError> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let url = reqwest::Url::parse_with_params(
        &format!("{}/search", base.trim_end_matches('/')),
        &[
            ("name", q),
            ("count", "10"),
            ("language", "en"),
            ("format", "json"),
        ],
    )
    .map_err(|e| malformed(&format!("url: {e}")))?;
    let body = client.get_bytes(SERVICE, url.as_str()).await?;
    parse_geocode(&body)
}

/// The Open-Meteo API rooted at a base URL.
#[derive(Debug, Clone)]
pub struct WeatherApi {
    base: String,
}

impl Default for WeatherApi {
    fn default() -> Self {
        Self::new(DEFAULT_BASE)
    }
}

impl WeatherApi {
    /// An API rooted at `base` (no trailing slash needed).
    #[must_use]
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_owned(),
        }
    }

    /// `GET /forecast` for (`lat`, `lon`) → today's forecast.
    pub async fn today(
        &self,
        client: &WebClient,
        lat: f64,
        lon: f64,
    ) -> Result<DailyForecast, FetchError> {
        let (lat, lon) = (lat.to_string(), lon.to_string());
        let url = reqwest::Url::parse_with_params(
            &format!("{}/forecast", self.base),
            &[
                ("latitude", lat.as_str()),
                ("longitude", lon.as_str()),
                (
                    "daily",
                    "temperature_2m_max,temperature_2m_min,weather_code",
                ),
                ("temperature_unit", "celsius"),
                ("timezone", "auto"),
                ("forecast_days", "1"),
            ],
        )
        .map_err(|e| malformed(&format!("url: {e}")))?;
        let body = client.get_bytes(SERVICE, url.as_str()).await?;
        parse_forecast(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_first_day_and_rejects_incomplete() {
        let b = br#"{"daily":{"time":["2026-10-15"],"temperature_2m_max":[24.6],"temperature_2m_min":[12.4],"weather_code":[61]}}"#;
        let f = parse_forecast(b).unwrap();
        assert_eq!((f.high_c, f.low_c, f.code), (24.6, 12.4, 61));
        assert!(parse_forecast(br#"{"latitude":1}"#).is_err());
        assert!(parse_forecast(br#"{"daily":{"temperature_2m_max":[1.0]}}"#).is_err());
    }

    #[test]
    fn decodes_geocode_hits_and_empty() {
        let b = br#"{"results":[{"name":"Gotham","country_code":"US","admin1":"Exampleland","latitude":40.00,"longitude":-100.00},{"name":"Nowhere","latitude":1.0,"longitude":2.0}]}"#;
        let g = parse_geocode(b).unwrap();
        assert_eq!(g[0].label, "Gotham, US");
        assert_eq!(g[0].admin1.as_deref(), Some("Exampleland"));
        assert_eq!((g[0].lat, g[0].lon), (40.00, -100.00));
        assert_eq!(
            (g[1].label.as_str(), g[1].admin1.as_ref()),
            ("Nowhere", None)
        );
        assert!(
            parse_geocode(br#"{"generationtime_ms":0.1}"#)
                .unwrap()
                .is_empty()
        );
    }
}
