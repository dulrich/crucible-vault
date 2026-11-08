//! Intake auto-enrich model (plan `intake-auto-enrich` WP-2; measurements in
//! `docs/spikes/auto-intake-llm/FINDINGS.md`): parse the model's JSON answer, resolve its text
//! anchors against the note body, and render the edit — a top `> [!abstract]-` block, optional
//! `## <chapter>` headings, and one `> [!removed]-` fold callout at the end of the body.
//!
//! Pure: no I/O, no clock. The daemon step (WP-3) owns provider calls, `enriched:` frontmatter
//! and the forced/non-forced skip decision; banner detection is WP-5's.
//!
//! Decisions recorded here:
//! - **Span kinds**: `sponsor|cta|chrome|footer` map to [`SpanKind`] variants; any other string is
//!   kept as [`SpanKind::Other`] and folded like the rest (the model only lists removable spans).
//! - **Anchors** compare whitespace-squeezed text ([`squeeze`]), never fuzzy. A span's
//!   `first_line` matches a whole line not already claimed by an earlier span (so a repeated chrome
//!   block resolves to its successive occurrences); `last_line` is the first whole-line match at or
//!   after it. `starts_at` is the first line whose squeezed text starts with the anchor, else the
//!   first line containing it. A claim `quote` must occur in the squeezed body (lines joined by one
//!   space), else the claim is dropped. `content_ends_after` is the *last* whole-line match (else
//!   the last line starting with it); every non-blank line after it becomes one `footer` span.
//! - **Banners** ([`BannerRef`]) are the exact embed text; every line consisting solely of that
//!   embed (squeezed equality) folds. Line indices were rejected: they drift across edits.
//! - **Headings**: when the body (outside code fences and outside folded lines) has markdown
//!   headings, chapters are ignored and the ToC links the existing headings in order; otherwise
//!   resolved chapters become `## <title>` headings and the ToC links them. The top block is only
//!   emitted when the model gave a summary, entities, claims or chapters (never a ToC alone).
//!   Chapters with unresolved anchors or duplicate lines are dropped; with existing headings the
//!   chapters are neither resolved nor counted.
//! - **Inserted headings** are preceded by a `%% cv-enrich heading %%` marker line and followed by
//!   one blank line; a re-run strips marker + heading + blank before deciding "has headings".
//! - **Generated blocks** are wrapped in `%% cv-enrich %%` … `%% /cv-enrich %%`; the block whose
//!   callout header is `> [!removed]-` is the fold, the other the top block. A re-run strips both
//!   and carries the old fold's content into the new fold (plan E2: content already folded stays).
//! - **Fold layout**: no per-group labels; disjoint groups are separated by a lone `>` line; each
//!   original line is prefixed `> ` (a blank line becomes `>`). A blank line precedes each close marker so the
//!   marker is not a lazy continuation of the callout (it would render as text inside it).

use std::collections::BTreeSet;
use std::fmt;

use cv_text::blocks;
use cv_text::sections::heading_text;
use serde_json::Value;

/// Marker closing a generated block ([`cv_text::blocks::ENRICH_CLOSE`]).
pub use cv_text::blocks::ENRICH_CLOSE as MARK_CLOSE;
/// Marker line preceding an inserted chapter heading ([`cv_text::blocks::ENRICH_HEADING`]).
pub use cv_text::blocks::ENRICH_HEADING as MARK_HEADING;
/// Marker opening a generated block ([`cv_text::blocks::ENRICH_OPEN`]).
pub use cv_text::blocks::ENRICH_OPEN as MARK_OPEN;
const FOLD_HEADER: &str = "> [!removed]-";
const TOP_HEADER: &str = "> [!abstract]-";

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
    /// Any other kind string the model produced (still folded).
    Other(String),
}

impl SpanKind {
    fn from_str(s: &str) -> Self {
        match s {
            "sponsor" => Self::Sponsor,
            "cta" => Self::Cta,
            "chrome" => Self::Chrome,
            "footer" => Self::Footer,
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

// ---------------------------------------------------------------------------------------------
// Parse
// ---------------------------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------------------------
// Body model
// ---------------------------------------------------------------------------------------------

struct Stripped {
    lines: Vec<String>,
    /// Content lines of the prior fold callout (after its header), as stored (`> `-prefixed).
    prior_fold: Vec<String>,
    had_generated: bool,
}

fn strip_generated(rest: &str) -> Stripped {
    let raw: Vec<&str> = rest.split('\n').collect();
    let mut lines = Vec::with_capacity(raw.len());
    let mut prior_fold = Vec::new();
    let (mut had_top, mut had_fold) = (false, false);
    let mut i = 0;
    while i < raw.len() {
        let t = raw[i].trim();
        // Only a well-formed open…close pair is generated; an unterminated open marker and
        // everything after it stay ordinary body lines (never discard user text).
        let close = (t == MARK_OPEN)
            .then(|| blocks::managed_close(&raw, i))
            .flatten();
        if let Some(j) = close {
            let mut inner = &raw[i + 1..j];
            // The blank line ending the callout is layout, not fold content (older notes lack it).
            while inner.last().is_some_and(|l| l.trim().is_empty()) {
                inner = &inner[..inner.len() - 1];
            }
            match inner.iter().position(|l| l.trim_end() == FOLD_HEADER) {
                Some(h) => {
                    had_fold = true;
                    if !prior_fold.is_empty() {
                        prior_fold.push(">".to_owned());
                    }
                    prior_fold.extend(inner[h + 1..].iter().map(|s| (*s).to_owned()));
                }
                None => had_top = true,
            }
            i = j + 1;
            continue;
        }
        if t == MARK_HEADING {
            // Strip the heading (and its blank) only if it is still a heading line.
            i += 1;
            if i < raw.len() && heading_text(raw[i]).is_some() {
                i += 1;
                if i < raw.len() && raw[i].trim().is_empty() {
                    i += 1;
                }
            }
            continue;
        }
        lines.push(raw[i].to_owned());
        i += 1;
    }
    if had_top {
        let lead = lines.iter().take_while(|l| l.trim().is_empty()).count();
        lines.drain(..lead);
    }
    if had_fold {
        while lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.pop();
        }
        lines.push(String::new());
    }
    Stripped {
        lines,
        prior_fold,
        had_generated: had_top || had_fold,
    }
}

fn link_safe(title: &str) -> String {
    squeeze(&title.replace(['[', ']', '|', '#', '^'], ""))
}

// ---------------------------------------------------------------------------------------------
// Enrich
// ---------------------------------------------------------------------------------------------

/// Apply `output` to the frontmatter-free note `body` (the caller reattaches its frontmatter).
/// Re-running on an enriched note replaces the generated blocks and keeps prior folds. Runs the
/// phases in order — resolve the fold, add banners under the half-body cap, plan headings, render
/// the top block, render the fold block, assemble — and merges their reports.
pub fn enrich(
    body: &str,
    output: &EnrichOutput,
    banners: &[BannerRef],
    opts: EnrichOpts,
) -> EnrichEdit {
    let st = strip_generated(body);
    let lines = &st.lines;
    let sq: Vec<String> = lines.iter().map(|l| squeeze(l)).collect();
    let mut rep = EnrichReport::default();

    let (spans, resolved) = resolve_fold(output, &sq);
    rep.merge(&resolved);
    let (fold, capped) = banners_and_cap(spans, banners, &sq);
    rep.merge(&capped);
    let plan = plan_headings(output, opts, lines, &sq, &fold);
    rep.merge(&plan.report);
    let (top, top_rep) = render_top(output, opts, &sq, &plan.toc);
    rep.merge(&top_rep);
    let (fold_block, fold_rep) = render_fold(&fold, &st.prior_fold, lines);
    rep.merge(&fold_rep);

    if !st.had_generated && top.is_empty() && fold_block.is_empty() && plan.inserts.is_empty() {
        return EnrichEdit {
            text: body.to_owned(),
            report: rep,
        };
    }
    EnrichEdit {
        text: assemble(lines, &fold, &plan.inserts, top, fold_block),
        report: rep,
    }
}

impl EnrichReport {
    /// Adds `other`'s counters and flags to `self` (each phase reports only its own fields).
    fn merge(&mut self, other: &Self) {
        self.anchors_resolved += other.anchors_resolved;
        self.anchors_dropped += other.anchors_dropped;
        self.spans_folded += other.spans_folded;
        self.lines_folded += other.lines_folded;
        self.banners_folded += other.banners_folded;
        self.fold_refused_over_half |= other.fold_refused_over_half;
        self.top_block |= other.top_block;
        self.headings_inserted += other.headings_inserted;
    }
}

/// Phase 1: the line indices the model's spans and `content_ends_after` fold.
fn resolve_fold(output: &EnrichOutput, sq: &[String]) -> (BTreeSet<usize>, EnrichReport) {
    let mut rep = EnrichReport::default();
    let mut fold: BTreeSet<usize> = BTreeSet::new();
    for span in &output.spans {
        let (a, b) = (squeeze(&span.first_line), squeeze(&span.last_line));
        let found = (!a.is_empty() && !b.is_empty())
            .then(|| {
                // Prefer an occurrence no earlier span claimed (repeated chrome); fall back to
                // any occurrence so overlapping spans still resolve (and merge).
                let pair = |s: usize| (s..sq.len()).find(|&e| sq[e] == b).map(|e| (s, e));
                let starts = || (0..sq.len()).filter(|&i| sq[i] == a);
                starts()
                    .filter(|i| !fold.contains(i))
                    .find_map(pair)
                    .or_else(|| starts().find_map(pair))
            })
            .flatten();
        match found {
            Some((s, e)) => {
                rep.anchors_resolved += 1;
                fold.extend(s..=e);
            }
            None => rep.anchors_dropped += 1,
        }
    }
    if let Some(end) = &output.content_ends_after {
        let a = squeeze(end);
        let at = (!a.is_empty())
            .then(|| {
                (0..sq.len())
                    .rev()
                    .find(|&i| sq[i] == a)
                    .or_else(|| (0..sq.len()).rev().find(|&i| sq[i].starts_with(&a)))
            })
            .flatten();
        match at {
            Some(i) => {
                rep.anchors_resolved += 1;
                let nb: Vec<usize> = (i + 1..sq.len()).filter(|&j| !sq[j].is_empty()).collect();
                if let (Some(&f), Some(&l)) = (nb.first(), nb.last()) {
                    fold.extend(f..=l);
                }
            }
            None => rep.anchors_dropped += 1,
        }
    }
    (fold, rep)
}

/// Phase 2: add the banner lines to `fold`, then drop the whole fold when it would take more than
/// half of the non-blank lines.
fn banners_and_cap(
    mut fold: BTreeSet<usize>,
    banners: &[BannerRef],
    sq: &[String],
) -> (BTreeSet<usize>, EnrichReport) {
    let mut rep = EnrichReport::default();
    let mut banner_lines: BTreeSet<usize> = BTreeSet::new();
    for b in banners {
        let a = squeeze(&b.0);
        if a.is_empty() {
            continue;
        }
        banner_lines.extend((0..sq.len()).filter(|&i| sq[i] == a));
    }
    fold.extend(banner_lines.iter().copied());

    let non_blank = sq.iter().filter(|s| !s.is_empty()).count();
    let fold_non_blank = fold.iter().filter(|&&i| !sq[i].is_empty()).count();
    if !fold.is_empty() && fold_non_blank * 2 > non_blank {
        rep.fold_refused_over_half = true;
        fold.clear();
    }
    rep.lines_folded = fold.len();
    rep.banners_folded = banner_lines.iter().filter(|i| fold.contains(i)).count();
    (fold, rep)
}

/// Phase 3's piece: the ToC entries and the chapter headings to insert (line index, title).
struct HeadingPlan {
    toc: Vec<String>,
    inserts: Vec<(usize, String)>,
    report: EnrichReport,
}

/// Phase 3: link the existing headings, or resolve the model's chapters into inserted headings.
fn plan_headings(
    output: &EnrichOutput,
    opts: EnrichOpts,
    lines: &[String],
    sq: &[String],
    fold: &BTreeSet<usize>,
) -> HeadingPlan {
    let mut rep = EnrichReport::default();
    let fence = blocks::fenced_lines(lines);
    let existing: Vec<&str> = (0..lines.len())
        .filter(|&i| !fence[i] && !fold.contains(&i))
        .filter_map(|i| heading_text(&lines[i]))
        .collect();
    let mut inserts: Vec<(usize, String)> = Vec::new();
    let toc: Vec<String> = if !opts.top_block {
        Vec::new()
    } else if !existing.is_empty() {
        existing.iter().map(|h| link_safe(h)).collect()
    } else {
        for ch in &output.chapters {
            let (a, title) = (squeeze(&ch.starts_at), link_safe(&ch.title));
            let at = (!a.is_empty() && !title.is_empty())
                .then(|| {
                    let ok = |i: &usize| !fold.contains(i) && !fence[*i];
                    (0..sq.len())
                        .filter(ok)
                        .find(|&i| sq[i].starts_with(&a))
                        .or_else(|| (0..sq.len()).filter(ok).find(|&i| sq[i].contains(&a)))
                })
                .flatten()
                .filter(|i| !inserts.iter().any(|(j, _)| j == i));
            match at {
                Some(i) => {
                    rep.anchors_resolved += 1;
                    inserts.push((i, title));
                }
                None => rep.anchors_dropped += 1,
            }
        }
        inserts.sort_by_key(|(i, _)| *i);
        inserts.iter().map(|(_, t)| t.clone()).collect()
    };
    rep.headings_inserted = inserts.len();
    HeadingPlan {
        toc,
        inserts,
        report: rep,
    }
}

/// Phase 4: the top `> [!abstract]-` block lines (empty when there is none).
fn render_top(
    output: &EnrichOutput,
    opts: EnrichOpts,
    sq: &[String],
    toc: &[String],
) -> (Vec<String>, EnrichReport) {
    let mut rep = EnrichReport::default();
    let mut top: Vec<String> = Vec::new();
    if !opts.top_block {
        return (top, rep);
    }
    let mut sections: Vec<Vec<String>> = Vec::new();
    if let Some(s) = &output.summary {
        sections.push(vec![squeeze(s)]);
    }
    if !output.entities.is_empty() {
        let ents: Vec<String> = output
            .entities
            .iter()
            .map(|e| {
                if e.kind.is_empty() {
                    e.name.clone()
                } else {
                    format!("{} ({})", e.name, e.kind)
                }
            })
            .collect();
        sections.push(vec![format!("Entities: {}", ents.join(", "))]);
    }
    let joined = sq
        .iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    let mut claims = Vec::new();
    for c in &output.key_claims {
        let q = squeeze(&c.quote);
        if q.is_empty() {
            claims.push(format!("- {}", c.claim));
        } else if joined.contains(&q) {
            rep.anchors_resolved += 1;
            claims.push(format!("- {} — \"{q}\"", c.claim));
        } else {
            rep.anchors_dropped += 1;
        }
    }
    if !claims.is_empty() {
        claims.insert(0, "Key claims:".to_owned());
        sections.push(claims);
    }
    if !toc.is_empty() {
        let mut s = vec!["Contents:".to_owned()];
        s.extend(toc.iter().map(|t| format!("- [[#{t}]]")));
        sections.push(s);
    }
    // A ToC of existing headings alone is not worth a block: require model content.
    let model_content = output.summary.is_some()
        || !output.entities.is_empty()
        || !output.key_claims.is_empty()
        || !output.chapters.is_empty();
    if model_content && !sections.is_empty() {
        rep.top_block = true;
        top.push(MARK_OPEN.to_owned());
        top.push(TOP_HEADER.to_owned());
        for (k, sec) in sections.iter().enumerate() {
            if k > 0 {
                top.push(">".to_owned());
            }
            top.extend(sec.iter().map(|l| format!("> {l}")));
        }
        // A blank line ends the callout: a close marker right after a `> ` line is a lazy
        // continuation of the callout paragraph and renders as text inside it.
        top.push(String::new());
        top.push(MARK_CLOSE.to_owned());
    }
    (top, rep)
}

/// Phase 5: the trailing `> [!removed]-` fold block lines (empty when nothing is folded).
fn render_fold(
    fold: &BTreeSet<usize>,
    prior_fold: &[String],
    lines: &[String],
) -> (Vec<String>, EnrichReport) {
    let mut rep = EnrichReport::default();
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for &i in fold {
        match groups.last_mut() {
            Some(g) if g.last() == Some(&(i - 1)) => g.push(i),
            _ => groups.push(vec![i]),
        }
    }
    rep.spans_folded = groups.len();
    let mut fold_block: Vec<String> = Vec::new();
    if !groups.is_empty() || !prior_fold.is_empty() {
        fold_block.push(MARK_OPEN.to_owned());
        fold_block.push(FOLD_HEADER.to_owned());
        fold_block.extend(prior_fold.iter().cloned());
        for g in &groups {
            if fold_block.len() > 2 {
                fold_block.push(">".to_owned());
            }
            for &i in g {
                let l = &lines[i];
                fold_block.push(if l.trim().is_empty() {
                    ">".to_owned()
                } else {
                    format!("> {l}")
                });
            }
        }
        fold_block.push(String::new()); // ends the callout (see the top block)
        fold_block.push(MARK_CLOSE.to_owned());
    }
    (fold_block, rep)
}

/// The removed spans of a note body's `> [!removed]-` fold(s), in order. Mirrors the writer
/// ([`render_fold`], layout in the module docs): the fold sits inside a `%% cv-enrich %%` block, each
/// removed line is `> `-prefixed (a blank one is a bare `>`), and disjoint groups are separated by a
/// lone `>` line. A span is therefore the unquoted lines between lone `>` lines; a blank line
/// *inside* a removed group is written as the same bare `>`, so it splits that group into two
/// spans (the writer does not distinguish them). Empty spans are dropped.
#[must_use]
pub fn removed_spans(body: &str) -> Vec<String> {
    let raw: Vec<&str> = body.split('\n').collect();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let close = (raw[i].trim() == MARK_OPEN)
            .then(|| blocks::managed_close(&raw, i))
            .flatten();
        let Some(j) = close else {
            i += 1;
            continue;
        };
        if let Some(h) = raw[i + 1..j]
            .iter()
            .position(|l| l.trim_end() == FOLD_HEADER)
        {
            let mut cur: Vec<&str> = Vec::new();
            let mut flush = |cur: &mut Vec<&str>| {
                let t = cur.join("\n");
                if !t.trim().is_empty() {
                    spans.push(t.trim().to_owned());
                }
                cur.clear();
            };
            for l in &raw[i + 1 + h + 1..j] {
                let l = l.trim_end();
                if l == ">" {
                    flush(&mut cur);
                } else {
                    cur.push(l.strip_prefix("> ").unwrap_or(l));
                }
            }
            flush(&mut cur);
        }
        i = j + 1;
    }
    spans
}

/// Phase 6: the body without folded lines, with the inserted headings, the top block first and
/// the fold block last.
fn assemble(
    lines: &[String],
    fold: &BTreeSet<usize>,
    inserts: &[(usize, String)],
    top: Vec<String>,
    fold_block: Vec<String>,
) -> String {
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + top.len() + fold_block.len());
    let mut ins = inserts.iter().peekable();
    let mut i = 0;
    while i < lines.len() {
        if fold.contains(&i) {
            while i < lines.len() && fold.contains(&i) {
                i += 1;
            }
            // Avoid a doubled blank where a folded group sat between two blank lines.
            if out.last().is_none_or(|l: &String| l.trim().is_empty())
                && lines.get(i).is_some_and(|l| l.trim().is_empty())
                && i + 1 < lines.len()
            {
                i += 1;
            }
            continue;
        }
        if let Some((_, t)) = ins.next_if(|(j, _)| *j == i) {
            out.push(MARK_HEADING.to_owned());
            out.push(format!("## {t}"));
            out.push(String::new());
        }
        out.push(lines[i].clone());
        i += 1;
    }
    if !top.is_empty() {
        let lead = out.iter().take_while(|l| l.trim().is_empty()).count();
        out.drain(..lead);
        let mut t = top;
        t.push(String::new());
        t.append(&mut out);
        out = t;
    }
    if !fold_block.is_empty() {
        while out.last().is_some_and(|l| l.trim().is_empty()) {
            out.pop();
        }
        out.push(String::new());
        out.extend(fold_block);
        out.push(String::new());
    }
    out.join("\n")
}

#[cfg(test)]
#[path = "enrich_tests.rs"]
mod tests;
