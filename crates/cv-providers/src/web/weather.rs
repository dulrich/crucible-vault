//! Open-Meteo forecast client (plan `v0-automation-pipelines` WP-9): today's high/low/code.
//!
//! Ports `https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/utils/weather.ts` `fetchWeather` (`:91-118`):
//! `daily=temperature_2m_max,temperature_2m_min,weather_code`, Celsius, one forecast day; a
//! missing `daily` block or incomplete first entries → `Malformed`. Keyless. Geocoding is not
//! ported (places carry lat/lon in `pipelines.toml`). Deviation: `timezone=auto` (the place's
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
}
