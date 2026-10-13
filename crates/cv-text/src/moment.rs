//! Moment.js format subset translated to chrono strftime, plus next-daily-date resolution.
//!
//! The subset is the one Crucible actually uses or documents (`https://github.com/dulrich/obsidian-crucible/blob/master/src/utils.ts:100-104`
//! `YYYY-MM-DD`, `HH:mm`, `YYYY-MM-DDTHH:mm:ss`; `src/periods.ts:67` `YYYY-MM`, `GGGG-[W]WW`;
//! `src/settings.ts:132` example `MMMM YYYY`) widened to the common single-field variants of the
//! same families. Any other letter run is a typed [`MomentFormatError`] — Moment would silently
//! emit it as a literal, which here would be garbage in a file name or heading.

use chrono::format::{Item, StrftimeItems};
use chrono::{Duration, NaiveDate, NaiveDateTime};
use std::fmt::Write as _;

/// A Moment format string that cannot be translated.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MomentFormatError {
    #[error("unsupported Moment format token `{token}` in `{format}`")]
    UnknownToken { token: String, format: String },
    #[error("unterminated `[` literal in Moment format `{format}`")]
    UnterminatedLiteral { format: String },
    #[error("formatting `{format}` failed")]
    Format { format: String },
}

fn map_token(letter: char, count: usize) -> Option<&'static str> {
    Some(match (letter, count) {
        ('Y', 4) => "%Y",
        ('Y', 2) => "%y",
        ('M', 1) => "%-m",
        ('M', 2) => "%m",
        ('M', 3) => "%b",
        ('M', 4) => "%B",
        ('D', 1) => "%-d",
        ('D', 2) => "%d",
        ('d', 1) => "%w",
        ('d', 3) => "%a",
        ('d', 4) => "%A",
        ('H', 1) => "%-H",
        ('H', 2) => "%H",
        ('h', 1) => "%-I",
        ('h', 2) => "%I",
        ('m', 1) => "%-M",
        ('m', 2) => "%M",
        ('s', 1) => "%-S",
        ('s', 2) => "%S",
        ('A', 1) => "%p",
        ('a', 1) => "%P",
        ('G', 4) => "%G",
        ('W', 1) => "%-V",
        ('W', 2) => "%V",
        _ => return None,
    })
}

/// Translate a Moment format string to a chrono strftime string.
pub fn moment_to_strftime(format: &str) -> Result<String, MomentFormatError> {
    let chars: Vec<char> = format.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == 'T' {
            // Moment emits the ISO date/time separator literally (used by `{{now}}`).
            out.push('T');
            i += 1;
        } else if c == '[' {
            let Some(rel) = chars[i + 1..].iter().position(|&x| x == ']') else {
                return Err(MomentFormatError::UnterminatedLiteral {
                    format: format.to_owned(),
                });
            };
            for &lit in &chars[i + 1..i + 1 + rel] {
                push_literal(&mut out, lit);
            }
            i += rel + 2;
        } else if c.is_ascii_alphabetic() {
            let mut j = i;
            while j < chars.len() && chars[j] == c {
                j += 1;
            }
            let count = j - i;
            match map_token(c, count) {
                Some(spec) => out.push_str(spec),
                None => {
                    return Err(MomentFormatError::UnknownToken {
                        token: chars[i..j].iter().collect(),
                        format: format.to_owned(),
                    });
                }
            }
            i = j;
        } else {
            push_literal(&mut out, c);
            i += 1;
        }
    }
    Ok(out)
}

fn push_literal(out: &mut String, c: char) {
    if c == '%' {
        out.push_str("%%");
    } else {
        out.push(c);
    }
}

/// Format `dt` with a Moment format string.
pub fn format_moment(dt: &NaiveDateTime, format: &str) -> Result<String, MomentFormatError> {
    let spec = moment_to_strftime(format)?;
    let items: Vec<Item<'_>> = StrftimeItems::new(&spec).collect();
    if items.iter().any(|it| matches!(it, Item::Error)) {
        return Err(MomentFormatError::Format {
            format: format.to_owned(),
        });
    }
    let mut out = String::new();
    write!(out, "{}", dt.format_with_items(items.into_iter())).map_err(|_| {
        MomentFormatError::Format {
            format: format.to_owned(),
        }
    })?;
    Ok(out)
}

/// Strictly parse `input` as a date in Moment `format` (Moment `moment(input, format, true)`).
///
/// Strictness is enforced by a round-trip: the parsed date must re-format to exactly `input`, so
/// `2026-1-5` does not match `YYYY-MM-DD` and trailing text is rejected. Returns `Ok(None)` when
/// the input does not match (or the format does not pin a full date, e.g. `GGGG-[W]WW`).
pub fn parse_moment_date_strict(
    input: &str,
    format: &str,
) -> Result<Option<NaiveDate>, MomentFormatError> {
    let spec = moment_to_strftime(format)?;
    let Ok(date) = NaiveDate::parse_from_str(input, &spec) else {
        return Ok(None);
    };
    let back = format_moment(&date.and_time(chrono::NaiveTime::MIN), format)?;
    Ok((back == input).then_some(date))
}

/// Target date for the `daily-next` capture: the day after the source daily note, never earlier
/// than today. A source basename that doesn't strictly parse with `date_format` (or no source)
/// counts as today, so the result is tomorrow.
///
/// Ported from `https://github.com/dulrich/obsidian-crucible/blob/master/src/captures.ts:175` (`resolveNextDailyDate`). Dropped:
/// nothing Obsidian-induced; `now` is an explicit argument instead of a Moment instance.
pub fn resolve_next_daily_date(
    source_basename: Option<&str>,
    date_format: &str,
    now: NaiveDateTime,
) -> Result<NaiveDate, MomentFormatError> {
    let today = now.date();
    let source = match source_basename {
        Some(b) => parse_moment_date_strict(b, date_format)?.unwrap_or(today),
        None => today,
    };
    let next = source + Duration::days(1);
    Ok(if next < today { today } else { next })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FMT: &str = "YYYY-MM-DD";

    fn now(y: i32, m: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap()
    }

    fn next(src: Option<&str>, n: NaiveDateTime) -> String {
        resolve_next_daily_date(src, FMT, n)
            .unwrap()
            .format("%Y-%m-%d")
            .to_string()
    }

    // Vectors from https://github.com/dulrich/obsidian-crucible/blob/master/tests/captureForwardNextDaily.test.mjs (resolver tests).
    #[test]
    fn resolver_source_today_is_tomorrow() {
        assert_eq!(
            next(Some("2026-10-13"), now(2026, 10, 13, 15, 30)),
            "2026-10-14"
        );
    }

    #[test]
    fn resolver_past_source_is_today() {
        let n = now(2026, 10, 13, 15, 30);
        assert_eq!(next(Some("2026-10-01"), n), "2026-10-13");
        assert_eq!(next(Some("2026-10-12"), n), "2026-10-13");
    }

    #[test]
    fn resolver_future_source_plus_one() {
        assert_eq!(
            next(Some("2026-10-20"), now(2026, 10, 13, 15, 30)),
            "2026-10-21"
        );
    }

    #[test]
    fn resolver_non_daily_or_absent_is_tomorrow() {
        let n = now(2026, 10, 13, 15, 30);
        assert_eq!(next(Some("Project notes"), n), "2026-10-14");
        assert_eq!(next(Some("2026-10-20 extra"), n), "2026-10-14");
        assert_eq!(next(None, n), "2026-10-14");
        assert_eq!(next(Some("2026-1-5"), n), "2026-10-14");
    }

    #[test]
    fn resolver_month_and_year_rollover() {
        assert_eq!(
            next(Some("2026-12-31"), now(2026, 12, 31, 9, 0)),
            "2027-01-01"
        );
        assert_eq!(
            next(Some("2026-10-31"), now(2026, 10, 31, 9, 0)),
            "2026-11-01"
        );
    }

    #[test]
    fn formats_used_subset() {
        let dt = NaiveDate::from_ymd_opt(2026, 1, 5)
            .unwrap()
            .and_hms_opt(7, 4, 9)
            .unwrap();
        assert_eq!(format_moment(&dt, "YYYY-MM-DD").unwrap(), "2026-01-05");
        assert_eq!(format_moment(&dt, "HH:mm").unwrap(), "07:04");
        assert_eq!(
            format_moment(&dt, "YYYY-MM-DDTHH:mm:ss").unwrap(),
            "2026-01-05T07:04:09"
        );
        assert_eq!(format_moment(&dt, "YYYY-MM").unwrap(), "2026-01");
        assert_eq!(format_moment(&dt, "GGGG-[W]WW").unwrap(), "2026-W02");
        assert_eq!(format_moment(&dt, "MMMM YYYY").unwrap(), "January 2026");
        assert_eq!(
            format_moment(&dt, "ddd D MMM YY, h:mm A").unwrap(),
            "Mon 5 Jan 26, 7:04 AM"
        );
        assert_eq!(format_moment(&dt, "[100%] YYYY").unwrap(), "100% 2026");
    }

    #[test]
    fn unknown_token_is_typed_error() {
        let dt = now(2026, 1, 1, 0, 0);
        assert!(
            matches!(format_moment(&dt, "Do MMMM"), Err(MomentFormatError::UnknownToken { ref token, .. }) if token == "o")
        );
        assert!(matches!(
            format_moment(&dt, "YYY"),
            Err(MomentFormatError::UnknownToken { .. })
        ));
        assert!(matches!(
            format_moment(&dt, "[W"),
            Err(MomentFormatError::UnterminatedLiteral { .. })
        ));
    }

    #[test]
    fn week_format_does_not_parse_as_date() {
        assert_eq!(
            parse_moment_date_strict("2026-W02", "GGGG-[W]WW").unwrap(),
            None
        );
    }
}
