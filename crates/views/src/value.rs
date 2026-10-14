//! Runtime values of the expression language and their coercions.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};

use crate::db::FileInfo;
use crate::result::Cell;

/// A runtime value. `File` is a file handle (`file`, `this`, `file(path)`, `files(..)` items);
/// `Note` is the frontmatter namespace of a file (`note.x`).
#[derive(Debug, Clone)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    /// A local date-time; `true` = date only.
    Date(NaiveDateTime, bool),
    Link {
        path: String,
        label: String,
    },
    List(Arc<Vec<Value>>),
    Object(Arc<BTreeMap<String, Value>>),
    File(Arc<FileInfo>),
    Note(Arc<FileInfo>),
    /// The `formula` namespace of the current row.
    Formulas,
}

impl Value {
    pub fn list(v: Vec<Value>) -> Self {
        Self::List(Arc::new(v))
    }

    pub fn date(d: NaiveDate) -> Self {
        Self::Date(d.and_time(NaiveTime::MIN), true)
    }

    /// Bases truthiness: null, false, 0, NaN, "" and empty lists are false.
    pub fn truthy(&self) -> bool {
        match self {
            Self::Null => false,
            Self::Bool(b) => *b,
            Self::Num(n) => *n != 0.0 && !n.is_nan(),
            Self::Str(s) => !s.is_empty(),
            Self::List(l) => !l.is_empty(),
            _ => true,
        }
    }

    /// Text form (`toString()`, concatenation, `join`). Null is "".
    pub fn to_text(&self) -> String {
        match self {
            Self::Null | Self::Formulas => String::new(),
            Self::Bool(b) => b.to_string(),
            Self::Num(n) => fmt_num(*n),
            Self::Str(s) => s.clone(),
            Self::Date(d, date_only) => fmt_date(d, *date_only),
            Self::Link { label, .. } => label.clone(),
            Self::List(l) => l.iter().map(Self::to_text).collect::<Vec<_>>().join(", "),
            Self::Object(o) => serde_json::to_string(
                &o.iter()
                    .map(|(k, v)| (k.clone(), v.to_text()))
                    .collect::<BTreeMap<_, _>>(),
            )
            .unwrap_or_default(),
            Self::File(f) | Self::Note(f) => f.path.clone(),
        }
    }

    /// Numeric coercion (`number()`, arithmetic, summaries): null → 0, bool → 0/1, text parsed
    /// (unparseable → NaN), dates → epoch milliseconds.
    pub fn to_num(&self) -> f64 {
        match self {
            Self::Null => 0.0,
            Self::Bool(b) => f64::from(u8::from(*b)),
            Self::Num(n) => *n,
            Self::Str(s) => {
                let t = s.trim();
                if t.is_empty() {
                    0.0
                } else {
                    t.parse().unwrap_or(f64::NAN)
                }
            }
            #[allow(clippy::cast_precision_loss)]
            Self::Date(d, _) => d.and_utc().timestamp_millis() as f64,
            _ => f64::NAN,
        }
    }

    /// The path a value designates (file handles, links, text), for `file()`, `hasLink`, equality.
    pub fn as_path(&self) -> Option<String> {
        match self {
            Self::File(f) | Self::Note(f) => Some(f.path.clone()),
            Self::Link { path, .. } => Some(path.clone()),
            Self::Str(s) => Some(s.clone()),
            _ => None,
        }
    }

    /// Converts a frontmatter JSON value; ISO-looking strings become dates.
    pub fn from_json(v: &serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => Self::Null,
            serde_json::Value::Bool(b) => Self::Bool(*b),
            serde_json::Value::Number(n) => n.as_f64().map_or(Self::Null, Self::Num),
            serde_json::Value::String(s) => match parse_date(s) {
                Some((d, only)) => Self::Date(d, only),
                None => Self::Str(s.clone()),
            },
            serde_json::Value::Array(a) => Self::list(a.iter().map(Self::from_json).collect()),
            serde_json::Value::Object(o) => Self::Object(Arc::new(
                o.iter()
                    .map(|(k, v)| (k.clone(), Self::from_json(v)))
                    .collect(),
            )),
        }
    }

    /// The display cell. A file handle renders as a link labelled with its name.
    pub fn to_cell(&self) -> Cell {
        match self {
            Self::Null | Self::Formulas => Cell::Null,
            Self::Bool(b) => Cell::Bool { v: *b },
            Self::Num(n) if n.is_finite() => Cell::Number { v: *n },
            Self::Num(_) => Cell::Null,
            Self::Str(s) => Cell::Text { v: s.clone() },
            Self::Date(d, only) => Cell::Date {
                v: fmt_date(d, *only),
            },
            Self::Link { path, label } => Cell::Link {
                path: path.clone(),
                label: label.clone(),
            },
            Self::List(l) => Cell::List {
                v: l.iter().map(Self::to_cell).collect(),
            },
            Self::Object(_) => Cell::Text { v: self.to_text() },
            Self::File(f) | Self::Note(f) => Cell::Link {
                path: f.path.clone(),
                label: f.name.clone(),
            },
        }
    }
}

/// Integers print without a fraction; others with up to 6 significant decimals trimmed.
pub fn fmt_num(n: f64) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n.fract() == 0.0 && n.abs() < 1e15 {
        return format!("{n:.0}");
    }
    let s = format!("{n:.6}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn fmt_date(d: &NaiveDateTime, date_only: bool) -> String {
    if date_only {
        d.format("%Y-%m-%d").to_string()
    } else {
        d.format("%Y-%m-%dT%H:%M:%S").to_string()
    }
}

/// Parses `YYYY-MM-DD`, `YYYY-MM-DD[T ]HH:MM[:SS]` and RFC 3339 (taken as local wall time).
pub fn parse_date(s: &str) -> Option<(NaiveDateTime, bool)> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 10 || !b[..4].iter().all(u8::is_ascii_digit) || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    if b.len() == 10 {
        return NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .ok()
            .map(|d| (d.and_time(NaiveTime::MIN), true));
    }
    for f in [
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M:%S%.f",
    ] {
        if let Ok(d) = NaiveDateTime::parse_from_str(s, f) {
            return Some((d, false));
        }
    }
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| (d.naive_local(), false))
}

fn as_date(v: &Value) -> Option<NaiveDateTime> {
    match v {
        Value::Date(d, _) => Some(*d),
        Value::Str(s) => parse_date(s).map(|(d, _)| d),
        _ => None,
    }
}

/// `==`: same-type equality; dates compare with ISO text; file handles/links by path; a number
/// equals a numeric string only if both parse equal.
pub fn loose_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Null, _) | (_, Value::Null) => false,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Num(x), Value::Num(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Date(..), _) | (_, Value::Date(..)) => {
            matches!((as_date(a), as_date(b)), (Some(x), Some(y)) if x == y)
        }
        (Value::List(x), Value::List(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| loose_eq(p, q))
        }
        (
            Value::File(_) | Value::Note(_) | Value::Link { .. },
            Value::File(_) | Value::Note(_) | Value::Link { .. } | Value::Str(_),
        )
        | (Value::Str(_), Value::File(_) | Value::Note(_) | Value::Link { .. }) => {
            a.as_path() == b.as_path()
        }
        _ => false,
    }
}

/// Ordering for `< <= > >=`: numbers, text, dates (text parsed as ISO), booleans; else none.
pub fn partial_cmp(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => x.partial_cmp(y),
        (Value::Date(..), _) | (_, Value::Date(..)) => Some(as_date(a)?.cmp(&as_date(b)?)),
        (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
        (Value::Num(x), Value::Str(_)) => x.partial_cmp(&b.to_num()),
        (Value::Str(_), Value::Num(y)) => a.to_num().partial_cmp(y),
        _ => None,
    }
}

fn rank(v: &Value) -> u8 {
    match v {
        Value::Null | Value::Formulas => 0,
        Value::Bool(_) => 1,
        Value::Num(_) => 2,
        Value::Date(..) => 3,
        _ => 4,
    }
}

/// Total order for sorting and grouping: null first, then bool < number < date < text-like
/// (case-insensitive text, then exact).
pub fn sort_cmp(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => x.total_cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Date(x, _), Value::Date(y, _)) => x.cmp(y),
        _ => {
            let (ra, rb) = (rank(a), rank(b));
            if ra != rb {
                return ra.cmp(&rb);
            }
            if ra == 0 {
                return Ordering::Equal;
            }
            let (x, y) = (a.to_text(), b.to_text());
            x.to_lowercase()
                .cmp(&y.to_lowercase())
                .then_with(|| x.cmp(&y))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coercions() {
        assert!(!Value::Null.truthy());
        assert!(!Value::list(vec![]).truthy());
        assert!(Value::Str("x".into()).truthy());
        assert_eq!(Value::Num(3.0).to_text(), "3");
        assert_eq!(Value::Num(2.5).to_text(), "2.5");
        assert_eq!(Value::Str(" 12 ".into()).to_num(), 12.0);
        assert!(Value::Str("x".into()).to_num().is_nan());
        assert!(matches!(
            Value::from_json(&serde_json::json!("2026-10-01")),
            Value::Date(_, true)
        ));
        assert!(matches!(
            Value::from_json(&serde_json::json!("2026-10-01T08:30")),
            Value::Date(_, false)
        ));
        assert!(matches!(
            Value::from_json(&serde_json::json!("hello")),
            Value::Str(_)
        ));
        let d = Value::from_json(&serde_json::json!("2026-10-01"));
        assert!(loose_eq(&d, &Value::Str("2026-10-01".into())));
        assert_eq!(
            partial_cmp(&Value::Str("2026-09-30".into()), &d),
            Some(Ordering::Less)
        );
        assert_eq!(sort_cmp(&Value::Null, &Value::Num(1.0)), Ordering::Less);
        assert_eq!(
            sort_cmp(&Value::Str("b".into()), &Value::Str("A".into())),
            Ordering::Greater
        );
    }
}
