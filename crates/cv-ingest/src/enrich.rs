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
//! - **Fold layout**: disjoint groups are separated by a lone `>` line; each original line is
//!   prefixed `> ` (a blank line becomes `>`). A blank line precedes each close marker so the
//!   marker is not a lazy continuation of the callout (it would render as text inside it).
//! - **Group metadata** (plan `intake-removals-sources-rescan` WP-3): between the open marker and
//!   the `> [!removed]-` header, one `%% cv-fold lines=N kind=K at=… gap=G join=J [| anchor] %%`
//!   line per group written since, in group order — a standalone comment line, hidden in Reading
//!   (`editor_model::comments`) and outside the callout, so the callout renders as before. The
//!   metadata groups own the *tail* of the fold content, last line ↔ last group, each exactly
//!   `lines=N` content lines (a blank line inside a group no longer splits it) with one lone `>`
//!   before it when anything precedes; content before them is old layout. A fold without metadata
//!   (written before WP-3) stays valid and is read, carried and re-rendered exactly as before;
//!   metadata whose counts do not fit makes the whole fold read as old layout. `kind` is the kind
//!   of the group's first line (the first span claiming a line wins; banners are `banner`). The
//!   anchor is text-derived, never a line index: `at=after occ=O | <squeezed text>` = after the
//!   O-th surviving line with that text (top to bottom), or `at=start`; then past `gap` blank
//!   lines; `join=1` = the writer's doubled-blank collapse dropped the blank after the group and
//!   Keep restores it. Values are `%XX`-escaped (`%`, `` ` ``, and in `kind` whitespace, `|`,
//!   `=`). Identity ([`FoldGroup::id`]) is an FNV-1a hash of the squeezed lines plus `-N` for
//!   repeats. [`resolve_groups`] (Keep/Delete) and [`trim_lines`] are the review operations; a
//!   resolve rewrites only the fold blocks it touched.

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
    /// The prior fold(s)' `%% cv-fold … %%` group metadata lines, in order (new layout only).
    prior_meta: Vec<String>,
    had_generated: bool,
}

fn strip_generated(rest: &str) -> Stripped {
    let raw: Vec<&str> = rest.split('\n').collect();
    let mut lines = Vec::with_capacity(raw.len());
    let mut prior_fold = Vec::new();
    let mut prior_meta = Vec::new();
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
                    prior_meta.extend(
                        inner[..h]
                            .iter()
                            .filter(|l| parse_meta(l).is_some())
                            .map(|s| (*s).to_owned()),
                    );
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
        prior_meta,
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

    let (spans, mut kinds, resolved) = resolve_fold(output, &sq);
    rep.merge(&resolved);
    let (fold, capped) = banners_and_cap(spans, &mut kinds, banners, &sq);
    rep.merge(&capped);
    let plan = plan_headings(output, opts, lines, &sq, &fold);
    rep.merge(&plan.report);
    let (top, top_rep) = render_top(output, opts, &sq, &plan.toc);
    rep.merge(&top_rep);
    let (fold_block, fold_rep) = render_fold(
        &plan_groups(lines, &sq, &fold, &kinds),
        &st.prior_meta,
        &st.prior_fold,
        lines,
    );
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

/// Phase 1: the line indices the model's spans and `content_ends_after` fold, and each folded
/// line's span kind (the first span claiming a line wins).
fn resolve_fold(output: &EnrichOutput, sq: &[String]) -> (BTreeSet<usize>, Kinds, EnrichReport) {
    let mut rep = EnrichReport::default();
    let mut fold: BTreeSet<usize> = BTreeSet::new();
    let mut kinds = Kinds::new();
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
                for i in s..=e {
                    kinds.entry(i).or_insert_with(|| span.kind.clone());
                }
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
                    for i in f..=l {
                        kinds.entry(i).or_insert(SpanKind::Footer);
                    }
                }
            }
            None => rep.anchors_dropped += 1,
        }
    }
    (fold, kinds, rep)
}

/// Phase 2: add the banner lines to `fold`, then drop the whole fold when it would take more than
/// half of the non-blank lines.
fn banners_and_cap(
    mut fold: BTreeSet<usize>,
    kinds: &mut Kinds,
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
    for &i in &banner_lines {
        kinds
            .entry(i)
            .or_insert_with(|| SpanKind::Other(BANNER_KIND.to_owned()));
    }

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

/// Phase 5's piece: one new fold group — its line indices, kind and position anchor.
struct NewGroup {
    idx: Vec<usize>,
    kind: SpanKind,
    anchor: Anchor,
}

/// Phase 5's piece: the folded groups (maximal runs of folded lines) with the anchor each leaves
/// behind. Mirrors [`assemble`]'s walk (inserted headings never sit between a group and its anchor
/// line, so they are ignored) to learn which surviving line precedes each group, how many blank
/// lines sit between, and whether the doubled-blank collapse dropped the blank after the group.
fn plan_groups(
    lines: &[String],
    sq: &[String],
    fold: &BTreeSet<usize>,
    kinds: &Kinds,
) -> Vec<NewGroup> {
    let blank = |k: usize| lines[k].trim().is_empty();
    let mut out: Vec<usize> = Vec::with_capacity(lines.len());
    let mut groups = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if !fold.contains(&i) {
            out.push(i);
            i += 1;
            continue;
        }
        let s = i;
        while i < lines.len() && fold.contains(&i) {
            i += 1;
        }
        let idx: Vec<usize> = (s..i).collect();
        let join = out.last().is_none_or(|&k| blank(k))
            && lines.get(i).is_some_and(|l| l.trim().is_empty())
            && i + 1 < lines.len();
        if join {
            i += 1;
        }
        let p = out.len();
        let anchor = match out.iter().rposition(|&k| !blank(k)) {
            Some(q) => {
                let t = sq[out[q]].clone();
                let occ = out[..=q].iter().filter(|&&k| sq[k] == t).count();
                Anchor {
                    after: Some((t, occ)),
                    gap: p - q - 1,
                    join,
                }
            }
            None => Anchor {
                after: None,
                gap: p,
                join,
            },
        };
        let kind = kinds
            .get(&s)
            .cloned()
            .unwrap_or_else(|| SpanKind::Other(String::new()));
        groups.push(NewGroup { idx, kind, anchor });
    }
    groups
}

/// Phase 5: the trailing `> [!removed]-` fold block lines (empty when nothing is folded): prior
/// metadata, then one `%% cv-fold … %%` line per new group, the header, the prior content carried
/// verbatim, then the new groups.
fn render_fold(
    groups: &[NewGroup],
    prior_meta: &[String],
    prior_fold: &[String],
    lines: &[String],
) -> (Vec<String>, EnrichReport) {
    let rep = EnrichReport {
        spans_folded: groups.len(),
        ..EnrichReport::default()
    };
    let mut fold_block: Vec<String> = Vec::new();
    if !groups.is_empty() || !prior_fold.is_empty() {
        fold_block.push(MARK_OPEN.to_owned());
        fold_block.extend(prior_meta.iter().cloned());
        fold_block.extend(
            groups
                .iter()
                .map(|g| render_meta(g.idx.len(), &g.kind, &g.anchor)),
        );
        fold_block.push(FOLD_HEADER.to_owned());
        fold_block.extend(prior_fold.iter().cloned());
        let mut any = !prior_fold.is_empty();
        for g in groups {
            if any {
                fold_block.push(">".to_owned());
            }
            any = true;
            fold_block.extend(g.idx.iter().map(|&i| quote(&lines[i])));
        }
        fold_block.push(String::new()); // ends the callout (see the top block)
        fold_block.push(MARK_CLOSE.to_owned());
    }
    (fold_block, rep)
}

/// A removed line as stored in the fold (`> line`, a blank one as a bare `>`).
fn quote(l: &str) -> String {
    if l.trim().is_empty() {
        ">".to_owned()
    } else {
        format!("> {l}")
    }
}

/// A fold content line back to the original line (inverse of [`quote`]; hand-written lines without
/// the `> ` prefix are kept as they are).
fn unquote(l: &str) -> &str {
    if l.trim_end() == ">" {
        ""
    } else {
        l.strip_prefix("> ").unwrap_or(l)
    }
}

// ---------------------------------------------------------------------------------------------
// Fold groups: metadata, listing, resolve, trim
// ---------------------------------------------------------------------------------------------

/// Kind of the lines folded as banners (no model span).
const BANNER_KIND: &str = "banner";
/// Name of the group metadata comment (`%% cv-fold … %%`).
const META_NAME: &str = "cv-fold";

/// Folded line index → the kind of the span that claimed it first.
type Kinds = std::collections::BTreeMap<usize, SpanKind>;

impl SpanKind {
    /// The kind string as the model writes it (`sponsor`, …; [`SpanKind::Other`] verbatim).
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Sponsor => "sponsor",
            Self::Cta => "cta",
            Self::Chrome => "chrome",
            Self::Footer => "footer",
            Self::Other(s) => s,
        }
    }
}

/// Where a fold group sat: after the `occ`-th surviving line whose squeezed text is the given
/// string (`None` = the start of the body), past `gap` blank lines; `join` = the blank line that
/// followed the group was collapsed by the writer and comes back with it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Anchor {
    after: Option<(String, usize)>,
    gap: usize,
    join: bool,
}

/// A parsed `%% cv-fold … %%` line; `n == 0` marks a malformed one (the fold then reads as old
/// layout).
#[derive(Debug, Clone)]
struct Meta {
    n: usize,
    kind: Option<SpanKind>,
    anchor: Option<Anchor>,
}

/// Percent-escape `%` and `` ` `` (never a `%%` inside the comment, never a code span), and with
/// `token` also whitespace, `|` and `=` (a single key=value token).
fn esc(s: &str, token: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '%' || c == '`' || (token && (c.is_whitespace() || c == '|' || c == '=')) {
            let mut b = [0u8; 4];
            for byte in c.encode_utf8(&mut b).bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn unesc(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = (b[i] == b'%')
            .then(|| s.get(i + 1..i + 3))
            .flatten()
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(v) => {
                out.push(v);
                i += 3;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn render_meta(n: usize, kind: &SpanKind, a: &Anchor) -> String {
    let at = match &a.after {
        None => "at=start".to_owned(),
        Some((_, occ)) => format!("at=after occ={occ}"),
    };
    let text = a
        .after
        .as_ref()
        .map(|(t, _)| format!(" | {}", esc(t, false)))
        .unwrap_or_default();
    format!(
        "%% {META_NAME} lines={n} kind={} {at} gap={} join={}{text} %%",
        esc(kind.as_str(), true),
        a.gap,
        u8::from(a.join),
    )
}

/// `Some` when `line` is a `%% cv-fold … %%` metadata line (well-formed or not).
fn parse_meta(line: &str) -> Option<Meta> {
    let inner = blocks::single_comment(line.trim())?;
    let rest = inner.strip_prefix(META_NAME)?;
    if !(rest.is_empty() || rest.starts_with(' ')) {
        return None;
    }
    let (fields, text) = rest.split_once(" | ").unwrap_or((rest, ""));
    let mut m = Meta {
        n: 0,
        kind: None,
        anchor: None,
    };
    let (mut at, mut occ, mut gap, mut join) = ("", 0usize, 0usize, false);
    for tok in fields.split_whitespace() {
        let Some((k, v)) = tok.split_once('=') else {
            continue;
        };
        match k {
            "lines" => m.n = v.parse().unwrap_or(0),
            "kind" => m.kind = Some(SpanKind::from_str(&unesc(v))),
            "at" => at = v,
            "occ" => occ = v.parse().unwrap_or(0),
            "gap" => gap = v.parse().unwrap_or(0),
            "join" => join = v == "1",
            _ => {}
        }
    }
    let text = squeeze(&unesc(text));
    m.anchor = match at {
        "start" => Some(Anchor {
            after: None,
            gap,
            join,
        }),
        "after" if occ > 0 && !text.is_empty() => Some(Anchor {
            after: Some((text, occ)),
            gap,
            join,
        }),
        _ => None,
    };
    Some(m)
}

/// One group of a parsed fold block.
#[derive(Debug, Clone)]
struct PGroup {
    /// The metadata line (new layout); `None` for an old-layout group.
    meta: Option<String>,
    /// Content lines as stored (`> `-prefixed), without separators.
    raw: Vec<String>,
    kind: Option<SpanKind>,
    anchor: Option<Anchor>,
}

impl PGroup {
    fn lines(&self) -> Vec<String> {
        self.raw.iter().map(|l| unquote(l).to_owned()).collect()
    }
    fn has_text(&self) -> bool {
        self.raw.iter().any(|l| !unquote(l).trim().is_empty())
    }
}

/// A well-formed `%% cv-enrich %%` block holding a `> [!removed]-` callout.
#[derive(Debug, Clone)]
struct PFold {
    open: usize,
    close: usize,
    /// Non-metadata lines between the open marker and the header (kept on rebuild).
    pre: Vec<String>,
    header: String,
    groups: Vec<PGroup>,
}

/// Old-layout content: groups split at lone `>` lines (the pre-metadata reader rule).
fn old_groups(content: &[&str], groups: &mut Vec<PGroup>) {
    let mut cur: Vec<String> = Vec::new();
    let mut flush = |cur: &mut Vec<String>| {
        let g = PGroup {
            meta: None,
            raw: std::mem::take(cur),
            kind: None,
            anchor: None,
        };
        if g.has_text() {
            groups.push(g);
        }
    };
    for l in content {
        if l.trim_end() == ">" {
            flush(&mut cur);
        } else {
            cur.push((*l).to_owned());
        }
    }
    flush(&mut cur);
}

/// The new-layout groups own the tail of the content, last metadata line ↔ last group, each
/// `lines=` long and preceded by a lone `>` separator when anything precedes it. Returns the
/// length of the old-layout prefix and each group's range, or `None` when the counts do not fit.
fn split_new(content: &[&str], metas: &[Meta]) -> Option<(usize, Vec<(usize, usize)>)> {
    let mut end = content.len();
    let mut ranges = Vec::with_capacity(metas.len());
    for m in metas.iter().rev() {
        if m.n == 0 || m.n > end {
            return None;
        }
        let s = end - m.n;
        ranges.push((s, end));
        end = s;
        if end > 0 {
            if content[end - 1].trim_end() != ">" {
                return None;
            }
            end -= 1;
        }
    }
    ranges.reverse();
    Some((end, ranges))
}

fn parse_folds(raw: &[&str]) -> Vec<PFold> {
    let mut folds = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let close = (raw[i].trim() == MARK_OPEN)
            .then(|| blocks::managed_close(raw, i))
            .flatten();
        let Some(j) = close else {
            i += 1;
            continue;
        };
        let inner = &raw[i + 1..j];
        if let Some(h) = inner.iter().position(|l| l.trim_end() == FOLD_HEADER) {
            let mut content = &inner[h + 1..];
            while content.last().is_some_and(|l| l.trim().is_empty()) {
                content = &content[..content.len() - 1];
            }
            let mut pre = Vec::new();
            let mut metas: Vec<(String, Meta)> = Vec::new();
            for l in &inner[..h] {
                match parse_meta(l) {
                    Some(m) => metas.push(((*l).to_owned(), m)),
                    None => pre.push((*l).to_owned()),
                }
            }
            let ms: Vec<Meta> = metas.iter().map(|(_, m)| m.clone()).collect();
            let mut groups = Vec::new();
            match (!ms.is_empty()).then(|| split_new(content, &ms)).flatten() {
                Some((old, ranges)) => {
                    old_groups(&content[..old], &mut groups);
                    for ((line, m), (s, e)) in metas.into_iter().zip(ranges) {
                        let g = PGroup {
                            meta: Some(line),
                            raw: content[s..e].iter().map(|l| (*l).to_owned()).collect(),
                            kind: m.kind,
                            anchor: m.anchor,
                        };
                        if g.has_text() {
                            groups.push(g);
                        }
                    }
                }
                None => old_groups(content, &mut groups),
            }
            folds.push(PFold {
                open: i,
                close: j,
                pre,
                header: inner[h].to_owned(),
                groups,
            });
        }
        i = j + 1;
    }
    folds
}

/// FNV-1a (64-bit) folded to 32 bits: `cv-ingest` has no hash crate on its normal edges.
fn fnv1a(s: &str) -> u32 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    #[allow(clippy::cast_possible_truncation)]
    let folded = (h ^ (h >> 32)) as u32;
    folded
}

/// Group identities in order: 8 hex digits of the squeezed lines' hash, `-2`, `-3`, … for a
/// repeat of the same content.
fn group_ids<'a>(groups: impl Iterator<Item = &'a PGroup>) -> Vec<String> {
    let mut seen: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    groups
        .map(|g| {
            let key: Vec<String> = g.raw.iter().map(|l| squeeze(unquote(l))).collect();
            let base = format!("{:08x}", fnv1a(&key.join("\n")));
            let n = seen.entry(base.clone()).or_insert(0);
            *n += 1;
            if *n == 1 { base } else { format!("{base}-{n}") }
        })
        .collect()
}

/// One group of a note's removed-content fold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoldGroup {
    /// Stable identity: hash of the squeezed lines, plus `-N` for the N-th identical group.
    pub id: String,
    /// The span kind (`None` for a group written before group metadata existed).
    pub kind: Option<SpanKind>,
    /// The original body lines, in order (a blank line is `""`).
    pub lines: Vec<String>,
    /// Whether the group carries a position anchor (Keep can restore it in place).
    pub anchored: bool,
}

/// The fold groups of a frontmatter-free note body, in document order (every fold block; old
/// and new layout; malformed metadata reads the fold as old layout).
#[must_use]
pub fn fold_groups(body: &str) -> Vec<FoldGroup> {
    let raw: Vec<&str> = body.split('\n').collect();
    let folds = parse_folds(&raw);
    let all: Vec<&PGroup> = folds.iter().flat_map(|f| &f.groups).collect();
    let ids = group_ids(all.iter().copied());
    all.into_iter()
        .zip(ids)
        .map(|(g, id)| FoldGroup {
            id,
            kind: g.kind.clone(),
            lines: g.lines(),
            anchored: g.anchor.is_some(),
        })
        .collect()
}

/// Whether a frontmatter-free note body carries a generated block: a well-formed managed
/// top block (`> [!abstract]-`) or `> [!removed]-` fold. Built on the one block parser
/// (`strip_generated`); an unterminated marker is ordinary text.
#[must_use]
pub fn has_generated_block(body: &str) -> bool {
    strip_generated(body).had_generated
}

/// The removed spans of a note body's `> [!removed]-` fold(s), in order, one per fold group
/// ([`fold_groups`]): each group's lines with trailing whitespace trimmed, joined, trimmed; empty
/// spans are dropped. Old-layout content (no `%% cv-fold %%` metadata) splits at every lone `>`
/// line, so a blank line inside such a group splits it into two spans; a new-layout group is one
/// span even when it holds blank lines. Metadata lines sit before the header and never reach a
/// span.
#[must_use]
pub fn removed_spans(body: &str) -> Vec<String> {
    let raw: Vec<&str> = body.split('\n').collect();
    parse_folds(&raw)
        .iter()
        .flat_map(|f| &f.groups)
        .filter_map(|g| {
            let t = g
                .lines()
                .iter()
                .map(|l| l.trim_end())
                .collect::<Vec<_>>()
                .join("\n");
            let t = t.trim();
            (!t.is_empty()).then(|| t.to_owned())
        })
        .collect()
}

/// What to do with a fold group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The lines leave the note.
    Delete,
    /// The lines go back into the body.
    Keep,
}

/// Which fold groups [`resolve_groups`] applies to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupTarget {
    /// One group by its [`FoldGroup::id`].
    One(String),
    /// Every group of every fold block.
    All,
}

/// Where a kept group's lines went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// At its anchor (where it was cut from).
    Anchor,
    /// At the end of the body, before the fold block (no anchor, or it no longer resolves).
    End,
}

/// One group [`resolve_groups`] acted on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedGroup {
    pub id: String,
    pub kind: Option<SpanKind>,
    pub lines: Vec<String>,
    pub verdict: Verdict,
    /// `Some` for [`Verdict::Keep`].
    pub placement: Option<Placement>,
}

/// The body after [`resolve_groups`] and the groups it resolved (document order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub text: String,
    pub groups: Vec<ResolvedGroup>,
}

/// Why a fold or trim operation refused (the body is unchanged).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoldError {
    /// No fold group has this identity (stale view, or already resolved).
    UnknownGroup(String),
    /// The trim range is empty (`first > last`).
    EmptyRange,
    /// The trim range runs past the body's last line.
    OutOfBounds { last: usize, lines: usize },
    /// The trim range touches a generated `%% cv-enrich %%` block or heading marker.
    Generated,
}

impl fmt::Display for FoldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownGroup(id) => write!(f, "no fold group {id}"),
            Self::EmptyRange => write!(f, "empty line range"),
            Self::OutOfBounds { last, lines } => {
                write!(f, "line {last} is past the body's {lines} lines")
            }
            Self::Generated => write!(f, "range touches a generated cv-enrich block"),
        }
    }
}

impl std::error::Error for FoldError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cls {
    /// Ordinary body line (anchor candidate).
    Plain,
    /// Generated: top block, inserted-heading marker + heading + its blank.
    Hidden,
    /// Placeholder for fold block `n` of [`parse_folds`].
    Fold(usize),
}

/// The body as segments: plain lines, generated lines, and one placeholder per fold block.
fn segments(raw: &[&str], folds: &[PFold]) -> Vec<(Cls, String)> {
    let mut segs = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let t = raw[i].trim();
        if let Some(b) = folds.iter().position(|f| f.open == i) {
            segs.push((Cls::Fold(b), String::new()));
            i = folds[b].close + 1;
            continue;
        }
        let close = (t == MARK_OPEN)
            .then(|| blocks::managed_close(raw, i))
            .flatten();
        if let Some(j) = close {
            segs.extend(raw[i..=j].iter().map(|l| (Cls::Hidden, (*l).to_owned())));
            i = j + 1;
            continue;
        }
        if t == MARK_HEADING {
            segs.push((Cls::Hidden, raw[i].to_owned()));
            i += 1;
            if i < raw.len() && heading_text(raw[i]).is_some() {
                segs.push((Cls::Hidden, raw[i].to_owned()));
                i += 1;
                if i < raw.len() && raw[i].trim().is_empty() {
                    segs.push((Cls::Hidden, raw[i].to_owned()));
                    i += 1;
                }
            }
            continue;
        }
        segs.push((Cls::Plain, raw[i].to_owned()));
        i += 1;
    }
    segs
}

fn plain_blank(s: &(Cls, String)) -> bool {
    s.0 == Cls::Plain && s.1.trim().is_empty()
}

/// Insert position (and missing blank lines to pad) for an anchored group, or `None` when the
/// anchor does not resolve. Rules: `after` = the `occ`-th plain line (top to bottom) with that
/// squeezed text; `start` = after a leading generated top block and its blank. Then step over up
/// to `gap` plain blank lines, never into the blank run in front of a fold block (pad instead).
fn anchor_pos(segs: &[(Cls, String)], a: &Anchor) -> Option<(usize, usize)> {
    let base = match &a.after {
        Some((t, occ)) => {
            segs.iter()
                .enumerate()
                .filter(|(_, s)| s.0 == Cls::Plain && squeeze(&s.1) == *t)
                .nth(occ.checked_sub(1)?)?
                .0
                + 1
        }
        None => {
            let lead = segs.iter().take_while(|s| s.0 == Cls::Hidden).count();
            if lead > 0 && segs.get(lead).is_some_and(plain_blank) {
                lead + 1
            } else {
                lead
            }
        }
    };
    let mut j = base;
    let mut adv = 0;
    while adv < a.gap && segs.get(j).is_some_and(plain_blank) {
        let next = segs[j..].iter().find(|s| !plain_blank(s));
        if next.is_none_or(|s| matches!(s.0, Cls::Fold(_))) {
            break;
        }
        j += 1;
        adv += 1;
    }
    Some((j, a.gap - adv))
}

/// The fallback position: before the blank run in front of fold block `b`.
fn end_pos(segs: &[(Cls, String)], b: usize) -> usize {
    let mut j = segs
        .iter()
        .position(|s| s.0 == Cls::Fold(b))
        .unwrap_or(segs.len());
    while j > 0 && plain_blank(&segs[j - 1]) {
        j -= 1;
    }
    j
}

fn rebuild_fold(f: &PFold, close: &str) -> Vec<String> {
    let mut out = vec![MARK_OPEN.to_owned()];
    out.extend(f.pre.iter().cloned());
    out.extend(f.groups.iter().filter_map(|g| g.meta.clone()));
    out.push(f.header.clone());
    // Old-layout groups first: the metadata owns the tail of the content.
    let (old, new): (Vec<&PGroup>, Vec<&PGroup>) = f.groups.iter().partition(|g| g.meta.is_none());
    for (k, g) in old.iter().chain(new.iter()).enumerate() {
        if k > 0 {
            out.push(">".to_owned());
        }
        out.extend(g.raw.iter().cloned());
    }
    out.push(String::new());
    out.push(close.to_owned());
    out
}

/// Render segments back to text: untouched folds verbatim, touched ones rebuilt, emptied ones
/// removed with the blank lines around them (a single blank separates what follows; the body ends
/// with one newline).
fn render_segments(
    segs: &[(Cls, String)],
    raw: &[&str],
    folds: &[PFold],
    touched: &[bool],
) -> String {
    let mut out: Vec<String> = Vec::with_capacity(segs.len());
    let mut dropped = false;
    for (cls, text) in segs {
        match cls {
            Cls::Fold(b) => {
                let f = &folds[*b];
                if !touched[*b] {
                    out.extend(raw[f.open..=f.close].iter().map(|l| (*l).to_owned()));
                } else if f.groups.is_empty() {
                    while out.last().is_some_and(|l| l.trim().is_empty()) {
                        out.pop();
                    }
                    dropped = true;
                } else {
                    out.extend(rebuild_fold(f, raw[f.close]));
                }
            }
            _ if dropped && text.trim().is_empty() => {}
            _ => {
                if dropped && !out.is_empty() {
                    out.push(String::new());
                }
                dropped = false;
                out.push(text.clone());
            }
        }
    }
    if dropped && !out.is_empty() {
        out.push(String::new());
    }
    out.join("\n")
}

/// Resolve fold groups of a frontmatter-free note body. [`Verdict::Delete`] drops the group's
/// lines; [`Verdict::Keep`] puts them back at the group's anchor, or — no anchor (old layout) or
/// it no longer resolves — at the end of the body just before the fold block. A fold block left
/// without groups is removed entirely (markers, metadata, header and the blank lines around it).
/// Untouched fold blocks stay byte-identical. Keep for several groups runs last-to-first so each
/// anchor is looked up in the body it was computed against.
///
/// # Errors
/// [`FoldError::UnknownGroup`] when `target` names no current group; nothing is returned then.
pub fn resolve_groups(
    body: &str,
    target: &GroupTarget,
    verdict: Verdict,
) -> Result<Resolution, FoldError> {
    let raw: Vec<&str> = body.split('\n').collect();
    let mut folds = parse_folds(&raw);
    // (fold, group-in-fold, id) in document order.
    let refs: Vec<(usize, usize)> = folds
        .iter()
        .enumerate()
        .flat_map(|(b, f)| (0..f.groups.len()).map(move |k| (b, k)))
        .collect();
    let ids = group_ids(refs.iter().map(|&(b, k)| &folds[b].groups[k]));
    let chosen: Vec<usize> = match target {
        GroupTarget::All => (0..refs.len()).collect(),
        GroupTarget::One(id) => vec![
            ids.iter()
                .position(|x| x == id)
                .ok_or_else(|| FoldError::UnknownGroup(id.clone()))?,
        ],
    };
    if chosen.is_empty() {
        return Ok(Resolution {
            text: body.to_owned(),
            groups: Vec::new(),
        });
    }
    let mut segs = segments(&raw, &folds);
    let mut placements: Vec<Option<Placement>> = vec![None; chosen.len()];
    if verdict == Verdict::Keep {
        // Anchored groups last-to-first; the fallbacks afterwards, first-to-last, so they land
        // at the end in document order.
        let mut at_end = Vec::new();
        for (c, &r) in chosen.iter().enumerate().rev() {
            let (b, k) = refs[r];
            let g = &folds[b].groups[k];
            let Some((a, (j, pad))) = g
                .anchor
                .as_ref()
                .and_then(|a| anchor_pos(&segs, a).map(|p| (a, p)))
            else {
                at_end.push(c);
                continue;
            };
            let mut ins: Vec<(Cls, String)> =
                (0..pad).map(|_| (Cls::Plain, String::new())).collect();
            ins.extend(g.lines().into_iter().map(|l| (Cls::Plain, l)));
            if a.join {
                ins.push((Cls::Plain, String::new()));
            }
            placements[c] = Some(Placement::Anchor);
            segs.splice(j..j, ins);
        }
        for &c in at_end.iter().rev() {
            let (b, k) = refs[chosen[c]];
            let at = end_pos(&segs, b);
            // Its own block: one blank after a non-blank preceding line (the blank run before the
            // fold stays after it).
            let sep = at > 0 && !segs[at - 1].1.trim().is_empty();
            let ins: Vec<(Cls, String)> = sep
                .then(String::new)
                .into_iter()
                .chain(folds[b].groups[k].lines())
                .map(|l| (Cls::Plain, l))
                .collect();
            segs.splice(at..at, ins);
            placements[c] = Some(Placement::End);
        }
    }
    let groups: Vec<ResolvedGroup> = chosen
        .iter()
        .zip(&placements)
        .map(|(&r, p)| {
            let g = &folds[refs[r].0].groups[refs[r].1];
            ResolvedGroup {
                id: ids[r].clone(),
                kind: g.kind.clone(),
                lines: g.lines(),
                verdict,
                placement: *p,
            }
        })
        .collect();
    let mut touched = vec![false; folds.len()];
    for &r in chosen.iter().rev() {
        let (b, k) = refs[r];
        touched[b] = true;
        folds[b].groups.remove(k);
    }
    Ok(Resolution {
        text: render_segments(&segs, &raw, &folds, &touched),
        groups,
    })
}

/// The body after [`trim_lines`] and the lines it removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trimmed {
    pub text: String,
    pub removed: Vec<String>,
}

/// Remove body lines `first..=last` outright (0-based, inclusive; lines split at `\n`, a trailing
/// newline counting as a final empty line) from a frontmatter-free note body — the caller
/// strips and reattaches frontmatter, so its line numbers are body-relative.
///
/// # Errors
/// [`FoldError::EmptyRange`] when `first > last`, [`FoldError::OutOfBounds`] when `last` is past
/// the last line, [`FoldError::Generated`] when the range touches a well-formed
/// `%% cv-enrich %%` block (top block or fold) or a `%% cv-enrich heading %%` marker.
pub fn trim_lines(body: &str, first: usize, last: usize) -> Result<Trimmed, FoldError> {
    let raw: Vec<&str> = body.split('\n').collect();
    if first > last {
        return Err(FoldError::EmptyRange);
    }
    if last >= raw.len() {
        return Err(FoldError::OutOfBounds {
            last,
            lines: raw.len(),
        });
    }
    let mut i = 0;
    while i < raw.len() {
        let t = raw[i].trim();
        let close = (t == MARK_OPEN)
            .then(|| blocks::managed_close(&raw, i))
            .flatten();
        let (s, e) = match close {
            Some(j) => (i, j),
            None if t == MARK_HEADING => (i, i),
            None => {
                i += 1;
                continue;
            }
        };
        if s <= last && first <= e {
            return Err(FoldError::Generated);
        }
        i = e + 1;
    }
    let removed = raw[first..=last].iter().map(|l| (*l).to_owned()).collect();
    let kept: Vec<&str> = raw[..first]
        .iter()
        .chain(&raw[last + 1..])
        .copied()
        .collect();
    Ok(Trimmed {
        text: kept.join("\n"),
        removed,
    })
}

/// Move body lines into the note's `[!removed]` fold without a model call (plan
/// `intake-removals-sources-rescan` WP-5: recurring RSS lines), one group per
/// `(first, last, kind)` range — 0-based inclusive body line indices (lines split at `\n`, as
/// [`trim_lines`]), ascending and disjoint, of a frontmatter-free body. The groups are written
/// exactly as [`enrich`] writes them (one `%% cv-fold … %%` metadata line each, `> `-quoted
/// content), appended to the body's last fold block, or to a new fold block at the end of the
/// body; everything else stays byte-identical. [`fold_groups`] lists them and
/// [`resolve_groups`] Keep restores each at its anchor.
///
/// # Errors
/// As [`trim_lines`] for each range; [`FoldError::EmptyRange`] also when ranges overlap or are
/// out of order.
pub fn fold_ranges(body: &str, groups: &[(usize, usize, SpanKind)]) -> Result<String, FoldError> {
    let mut prev: Option<usize> = None;
    for (f, l, _) in groups {
        trim_lines(body, *f, *l)?;
        if prev.is_some_and(|p| *f <= p) {
            return Err(FoldError::EmptyRange);
        }
        prev = Some(*l);
    }
    if groups.is_empty() {
        return Ok(body.to_owned());
    }
    let raw: Vec<&str> = body.split('\n').collect();
    let folds = parse_folds(&raw);
    let mut hidden = vec![false; raw.len()];
    // Per raw line: generated (top block, heading marker + heading + blank, fold blocks).
    let mut i = 0;
    while i < raw.len() {
        let t = raw[i].trim();
        let close = (t == MARK_OPEN)
            .then(|| blocks::managed_close(&raw, i))
            .flatten();
        if let Some(j) = close {
            hidden[i..=j].iter_mut().for_each(|h| *h = true);
            i = j + 1;
            continue;
        }
        if t == MARK_HEADING {
            hidden[i] = true;
            i += 1;
            if i < raw.len() && heading_text(raw[i]).is_some() {
                hidden[i] = true;
                i += 1;
                if i < raw.len() && raw[i].trim().is_empty() {
                    hidden[i] = true;
                    i += 1;
                }
            }
            continue;
        }
        i += 1;
    }
    let blank = |k: usize| raw[k].trim().is_empty();
    let in_fold = |k: usize| folds.iter().any(|f| (f.open..=f.close).contains(&k));
    let mut out: Vec<usize> = Vec::with_capacity(raw.len());
    let mut metas: Vec<String> = Vec::with_capacity(groups.len());
    let mut content: Vec<Vec<String>> = Vec::with_capacity(groups.len());
    let mut i = 0;
    for (f, l, kind) in groups {
        out.extend(i..*f);
        i = l + 1;
        // As `plan_groups` over `strip_generated`'s lines: the blank run in front of a fold block
        // (or the end) is layout, so a group followed only by it never joins.
        let join = out.last().is_none_or(|&k| blank(k))
            && raw.get(i).is_some_and(|x| x.trim().is_empty())
            && (i + 1..raw.len()).any(|k| !blank(k) && !in_fold(k));
        if join {
            i += 1;
        }
        let plain_after = |q: usize| out[q + 1..].iter().filter(|&&k| !hidden[k]).count();
        let anchor = match out.iter().rposition(|&k| !hidden[k] && !blank(k)) {
            Some(q) => {
                let t = squeeze(raw[out[q]]);
                let occ = out[..=q]
                    .iter()
                    .filter(|&&k| !hidden[k] && squeeze(raw[k]) == t)
                    .count();
                Anchor {
                    after: Some((t, occ)),
                    gap: plain_after(q),
                    join,
                }
            }
            None => {
                let lead = out.iter().take_while(|&&k| hidden[k]).count();
                let skip = usize::from(lead > 0 && out.get(lead).is_some_and(|&k| blank(k)));
                Anchor {
                    after: None,
                    gap: out[lead..].iter().filter(|&&k| !hidden[k]).count() - skip,
                    join,
                }
            }
        };
        metas.push(render_meta(l - f + 1, kind, &anchor));
        content.push(raw[*f..=*l].iter().map(|x| quote(x)).collect());
    }
    out.extend(i..raw.len());
    let mut res: Vec<String> = Vec::with_capacity(out.len() + metas.len() * 2 + 8);
    match folds.last() {
        Some(fold) => {
            let header = (fold.open + 1..fold.close)
                .find(|&k| raw[k].trim_end() == FOLD_HEADER)
                .unwrap_or(fold.open + 1);
            let last = (header..fold.close)
                .rev()
                .find(|&k| !raw[k].trim().is_empty())
                .unwrap_or(header);
            for k in out {
                if k == fold.open {
                    // A group taken from just above the fold leaves one blank line, as `enrich`.
                    while res.len() >= 2 && res[res.len() - 2..].iter().all(|l| l.trim().is_empty())
                    {
                        res.pop();
                    }
                }
                if k == header {
                    res.extend(metas.iter().cloned());
                }
                res.push(raw[k].to_owned());
                if k == last {
                    for (n, g) in content.iter().enumerate() {
                        if n > 0 || last != header {
                            res.push(">".to_owned());
                        }
                        res.extend(g.iter().cloned());
                    }
                }
            }
        }
        None => {
            res.extend(out.iter().map(|&k| raw[k].to_owned()));
            while res.last().is_some_and(|l| l.trim().is_empty()) {
                res.pop();
            }
            res.push(String::new());
            res.push(MARK_OPEN.to_owned());
            res.extend(metas);
            res.push(FOLD_HEADER.to_owned());
            for (n, g) in content.into_iter().enumerate() {
                if n > 0 {
                    res.push(">".to_owned());
                }
                res.extend(g);
            }
            res.push(String::new());
            res.push(MARK_CLOSE.to_owned());
            res.push(String::new());
        }
    }
    Ok(res.join("\n"))
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
