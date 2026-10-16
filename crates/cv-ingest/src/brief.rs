//! Daily Brief line builders (plan `v0-automation-pipelines` WP-9), pure and typed.
//!
//! Ports `https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/workflows/DailyBriefLiteWorkflow.ts`
//! (`formatFxLine`, `formatWeatherLine`, the inline `*lookup failed (…)*` line and the
//! `**FX Rates** _(updated …)_` / `**Weather**` block assembly, `:55-100`) and the
//! weather-code table of `utils/weather.ts:22-44`. Nothing Obsidian-induced to drop.

/// One brief entry: a fetched value or the failure reason.
pub type Entry<T> = Result<T, String>;

/// A fetched FX rate as the brief shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct FxLine {
    /// Display label (`USD/MXN` by default).
    pub label: String,
    /// Units of quote per base.
    pub rate: f64,
    /// Reference date.
    pub as_of: String,
}

/// A fetched forecast as the brief shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct WeatherLine {
    /// Place label.
    pub label: String,
    /// High, °C.
    pub high_c: f64,
    /// Low, °C.
    pub low_c: f64,
    /// WMO weather code.
    pub code: i64,
}

/// Crucible's `WEATHER_CODES` label (`code N` when unmapped).
#[must_use]
pub fn weather_label(code: i64) -> String {
    let s = match code {
        0 => "clear",
        1 => "mostly clear",
        2 => "partly cloudy",
        3 => "overcast",
        45 => "fog",
        48 => "rime fog",
        51 => "light drizzle",
        53 => "drizzle",
        55 => "heavy drizzle",
        61 => "light rain",
        63 => "rain",
        65 => "heavy rain",
        71 => "light snow",
        73 => "snow",
        75 => "heavy snow",
        80 | 81 => "rain showers",
        82 => "violent showers",
        95 => "thunderstorm",
        96 => "thunderstorm w/ hail",
        99 => "thunderstorm w/ heavy hail",
        _ => return format!("code {code}"),
    };
    s.to_owned()
}

/// JS `Math.round` (ties toward +∞), with `-0` printed as `0` like a JS template literal.
fn js_round(x: f64) -> f64 {
    (x + 0.5).floor() + 0.0
}

/// `- LABEL: 18.23 _(as of 2026-10-14)_`.
#[must_use]
pub fn fx_line(l: &FxLine) -> String {
    format!("- {}: {:.2} _(as of {})_", l.label, l.rate, l.as_of)
}

/// `- LABEL: ↑25°C / ↓12°C, light rain`.
#[must_use]
pub fn weather_line(w: &WeatherLine) -> String {
    format!(
        "- {}: ↑{}°C / ↓{}°C, {}",
        w.label,
        js_round(w.high_c),
        js_round(w.low_c),
        weather_label(w.code)
    )
}

/// The inline partial-failure line: `- LABEL: *lookup failed (REASON)*`.
#[must_use]
pub fn failed_line(label: &str, reason: &str) -> String {
    format!("- {label}: *lookup failed ({reason})*")
}

/// The brief body: the FX block (`**FX Rates** _(updated STAMP)_` + lines) and the weather block
/// (`**Weather**` + lines), each present only when it has entries, joined by a blank line.
/// Failed entries render as [`failed_line`] under the entry's `label`.
#[must_use]
pub fn brief_body(
    stamp: &str,
    fx: &[(String, Entry<FxLine>)],
    weather: &[(String, Entry<WeatherLine>)],
) -> String {
    let mut blocks = Vec::new();
    if !fx.is_empty() {
        let mut b = vec![format!("**FX Rates** _(updated {stamp})_")];
        b.extend(fx.iter().map(|(label, e)| match e {
            Ok(l) => fx_line(l),
            Err(r) => failed_line(label, r),
        }));
        blocks.push(b.join("\n"));
    }
    if !weather.is_empty() {
        let mut b = vec!["**Weather**".to_owned()];
        b.extend(weather.iter().map(|(label, e)| match e {
            Ok(w) => weather_line(w),
            Err(r) => failed_line(label, r),
        }));
        blocks.push(b.join("\n"));
    }
    blocks.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_match_crucible_text() {
        let fx = FxLine {
            label: "USD/MXN".into(),
            rate: 18.2345,
            as_of: "2026-10-14".into(),
        };
        assert_eq!(fx_line(&fx), "- USD/MXN: 18.23 _(as of 2026-10-14)_");
        let w = WeatherLine {
            label: "Bolzano, IT".into(),
            high_c: 24.5,
            low_c: -2.5,
            code: 61,
        };
        assert_eq!(weather_line(&w), "- Bolzano, IT: ↑25°C / ↓-2°C, light rain");
        assert_eq!(weather_label(7), "code 7");
        let body = brief_body(
            "2026-10-15 06:00 +00:00",
            &[
                ("USD/MXN".into(), Ok(fx)),
                ("EUR/USD".into(), Err("HTTP 500".into())),
            ],
            &[("Bolzano, IT".into(), Ok(w))],
        );
        assert_eq!(
            body,
            "**FX Rates** _(updated 2026-10-15 06:00 +00:00)_\n- USD/MXN: 18.23 _(as of 2026-10-14)_\n- EUR/USD: *lookup failed (HTTP 500)*\n\n**Weather**\n- Bolzano, IT: ↑25°C / ↓-2°C, light rain"
        );
        assert_eq!(brief_body("s", &[], &[]), "");
    }
}
