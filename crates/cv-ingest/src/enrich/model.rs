//! The model answer types and their lenient parse. Decisions recorded here:
//! - **Span kinds**: `sponsor|cta|chrome|footer|banner` map to [`SpanKind`] variants; any other string is
//!   kept as [`SpanKind::Other`] and folded like the rest (the model only lists removable spans).

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

/// A named entity.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Entity {
    pub name: String,
    /// The model's `type` (person, org, …); may be empty.
    pub kind: String,
    pub aliases: Vec<String>,
    pub mentions: u64,
}

/// A key claim with its supporting verbatim quote (may be empty).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Claim {
    pub claim: String,
    pub quote: String,
}

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
    pub confidence: f64,
}

/// The merged structure + cleanup answer (spike prompts; `scores` ignored).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EnrichOutput {
    pub summary: Option<String>,
    pub entities: Vec<Entity>,
    pub key_claims: Vec<Claim>,
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
    /// Render the top block and chapter headings; `false` = fold only (short bodies).
    pub top_block: bool,
}

/// What [`enrich`] did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EnrichReport {
    pub anchors_resolved: usize,
    pub anchors_dropped: usize,
    /// Disjoint fold groups added by this run.
    pub spans_folded: usize,
    /// Lines moved into the fold by this run (spans + banners).
    pub lines_folded: usize,
    /// Of `lines_folded`, lines folded as banners.
    pub banners_folded: usize,
    pub fold_refused_over_half: bool,
    pub top_block: bool,
    pub headings_inserted: usize,
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

    out.summary = opt_string(obj.get("summary"), bad);
    out.content_ends_after = opt_string(obj.get("content_ends_after"), bad);
    out.entities = items(obj.get("entities"), bad, |o| {
        let name = squeeze(o.get("name")?.as_str()?);
        if name.is_empty() {
            return None;
        }
        Some(Entity {
            name,
            kind: o
                .get("type")
                .and_then(Value::as_str)
                .map(squeeze)
                .unwrap_or_default(),
            aliases: o
                .get("aliases")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            mentions: o.get("mentions").and_then(Value::as_u64).unwrap_or(0),
        })
    });
    out.key_claims = items(obj.get("key_claims"), bad, |o| {
        let claim = squeeze(o.get("claim")?.as_str()?);
        if claim.is_empty() {
            return None;
        }
        let quote = match o.get("quote") {
            None | Some(Value::Null) => String::new(),
            Some(q) => q.as_str()?.to_owned(),
        };
        Some(Claim { claim, quote })
    });
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
            confidence: o.get("confidence").and_then(Value::as_f64).unwrap_or(1.0),
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
