//! The model answer types and their lenient parse. Decisions recorded here:
//! - **Span kinds**: `sponsor|cta|chrome|footer|banner` map to [`SpanKind`] variants; any other string is
//!   kept as [`SpanKind::Other`] and folded like the rest (the model only lists removable spans).
//! - **Retired keys**: `summary`, `entities` and `key_claims` (the old top block's content; plan
//!   abstract-removal-confidence-deletion WP-2) are no longer requested; an answer still carrying
//!   them is accepted and they are ignored like any unknown key (never counted in `malformed`).

use std::fmt;

use serde_json::Value;

/// Why a model answer could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrichError {
    /// No `{…}` object in the text.
    NoObject,
    /// The `{…}` slice is not valid JSON.
    InvalidJson(String),
    /// Valid JSON, but not an object.
    NotAnObject,
}

impl fmt::Display for EnrichError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoObject => write!(f, "model output contains no JSON object"),
            Self::InvalidJson(e) => write!(f, "model output is not valid JSON: {e}"),
            Self::NotAnObject => write!(f, "model output JSON is not an object"),
        }
    }
}

impl std::error::Error for EnrichError {}

/// A chapter title anchored at the start of a body line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Chapter {
    pub title: String,
    pub starts_at: String,
}

/// Kind of a removable span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpanKind {
    Sponsor,
    Cta,
    Chrome,
    Footer,
    /// Lines folded as banners (no model span).
    Banner,
    /// Any other kind string the model produced (still folded).
    Other(String),
}

impl SpanKind {
    pub(crate) fn from_str(s: &str) -> Self {
        match s {
            "sponsor" => Self::Sponsor,
            "cta" => Self::Cta,
            "chrome" => Self::Chrome,
            "footer" => Self::Footer,
            "banner" => Self::Banner,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// A removable span delimited by whole-line anchors.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub kind: SpanKind,
    pub first_line: String,
    pub last_line: String,
    /// The model's confidence that the span is removable: `Some` only for a finite number in
    /// 0.0–1.0; missing, non-numeric or out-of-range is `None` and the span always folds (plan
    /// abstract-removal-confidence-deletion WP-3: no default reads as "sure").
    pub confidence: Option<f64>,
}

/// The merged structure + cleanup answer (spike prompts; `scores` ignored).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EnrichOutput {
    pub chapters: Vec<Chapter>,
    pub spans: Vec<Span>,
    pub content_ends_after: Option<String>,
    /// Members/items dropped for having the wrong type (lenient parse; not fatal).
    pub malformed: usize,
}

/// A banner to fold: the exact embed text (`![[…]]` or `![](…)`) as it appears in the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BannerRef(pub String);

/// Options for [`enrich`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnrichOpts {
    /// Insert chapter headings into a heading-less body; `false` = fold only (short bodies).
    pub headings: bool,
}

/// What [`enrich`] did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EnrichReport {
    pub anchors_resolved: usize,
    pub anchors_dropped: usize,
    /// Spans that matched no body line but whose anchors are lines of the prior fold (already
    /// applied by an earlier run; neither resolved nor dropped).
    pub anchors_already_folded: usize,
    /// Disjoint fold groups added by this run.
    pub spans_folded: usize,
    /// Lines moved into the fold by this run (spans + banners).
    pub lines_folded: usize,
    /// Of `lines_folded`, lines folded as banners.
    pub banners_folded: usize,
    pub fold_refused_over_half: bool,
    pub headings_inserted: usize,
    /// Lines removed from the body without a fold copy (confidence tier; never in `lines_folded`).
    pub lines_deleted: usize,
    /// Disjoint groups of deleted lines.
    pub spans_deleted: usize,
}

/// The deletion tier of [`enrich_with`](super::enrich_with): `threshold` 0 (the default) deletes
/// nothing; `labels` (the note's publication's, if any) veto deletion of a Keep-labelled line.
#[derive(Debug, Clone, Copy, Default)]
pub struct Deletion<'a> {
    pub threshold: f64,
    pub labels: Option<&'a crate::boilerplate::Labels>,
}

/// The delete-or-fold decision of one model span: delete when deletion is on (`threshold > 0`) and
/// the span's `score` is a valid confidence at or over it. The single seam a later scorer (e.g. a
/// local classifier) supplies its score through; everything else about deletion stays in the pass.
#[must_use]
pub fn deletes(score: Option<f64>, threshold: f64) -> bool {
    threshold > 0.0 && score.is_some_and(|c| c >= threshold)
}

/// The rendered note and its report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrichEdit {
    pub text: String,
    pub report: EnrichReport,
}

/// Collapse whitespace runs to one space and trim.
pub fn squeeze(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parse a model answer leniently: fences and surrounding prose are skipped (outermost `{…}`),
/// unknown keys ignored, wrong-typed members dropped and counted in `malformed`.
pub fn parse(text: &str) -> Result<EnrichOutput, EnrichError> {
    // A top-level array (or other JSON) before any object is a wrong shape, not "no object".
    if let (Some(a), Some(z)) = (text.find('['), text.rfind(']'))
        && text.find('{').is_none_or(|o| a < o)
        && a < z
        && serde_json::from_str::<Value>(&text[a..=z]).is_ok()
    {
        return Err(EnrichError::NotAnObject);
    }
    let start = text.find('{').ok_or(EnrichError::NoObject)?;
    let end = text.rfind('}').ok_or(EnrichError::NoObject)?;
    if end < start {
        return Err(EnrichError::NoObject);
    }
    let v: Value = serde_json::from_str(&text[start..=end])
        .map_err(|e| EnrichError::InvalidJson(e.to_string()))?;
    let obj = v.as_object().ok_or(EnrichError::NotAnObject)?;
    let mut out = EnrichOutput::default();
    let bad = &mut out.malformed;

    out.content_ends_after = opt_string(obj.get("content_ends_after"), bad);
    out.chapters = items(obj.get("chapters"), bad, |o| {
        Some(Chapter {
            title: o.get("title")?.as_str()?.to_owned(),
            starts_at: o.get("starts_at")?.as_str()?.to_owned(),
        })
    });
    out.spans = items(obj.get("spans"), bad, |o| {
        Some(Span {
            kind: SpanKind::from_str(o.get("kind")?.as_str()?),
            first_line: o.get("first_line")?.as_str()?.to_owned(),
            last_line: o.get("last_line")?.as_str()?.to_owned(),
            confidence: o
                .get("confidence")
                .and_then(Value::as_f64)
                .filter(|c| c.is_finite() && (0.0..=1.0).contains(c)),
        })
    });
    Ok(out)
}

fn opt_string(v: Option<&Value>, bad: &mut usize) -> Option<String> {
    match v {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            let s = s.trim();
            (!s.is_empty()).then(|| s.to_owned())
        }
        Some(_) => {
            *bad += 1;
            None
        }
    }
}

fn items<T>(
    v: Option<&Value>,
    bad: &mut usize,
    f: impl Fn(&serde_json::Map<String, Value>) -> Option<T>,
) -> Vec<T> {
    match v {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|it| {
                let r = it.as_object().and_then(&f);
                if r.is_none() {
                    *bad += 1;
                }
                r
            })
            .collect(),
        Some(_) => {
            *bad += 1;
            Vec::new()
        }
    }
}

impl SpanKind {
    /// The kind string as the model writes it (`sponsor`, …; [`SpanKind::Other`] verbatim).
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Sponsor => "sponsor",
            Self::Cta => "cta",
            Self::Chrome => "chrome",
            Self::Footer => "footer",
            Self::Banner => "banner",
            Self::Other(s) => s,
        }
    }
}
