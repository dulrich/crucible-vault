//! The fold group metadata codec: the `%% cv-fold … %%` line and the position anchor it carries.
//! Decisions recorded here:
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

use cv_text::blocks;

use crate::enrich::{SpanKind, squeeze};

/// Name of the group metadata comment (`%% cv-fold … %%`).
const META_NAME: &str = "cv-fold";

/// Where a fold group sat: after the `occ`-th surviving line whose squeezed text is the given
/// string (`None` = the start of the body), past `gap` blank lines; `join` = the blank line that
/// followed the group was collapsed by the writer and comes back with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Anchor {
    pub(super) after: Option<(String, usize)>,
    pub(super) gap: usize,
    pub(super) join: bool,
}

/// A well-formed `%% cv-fold … %%` line.
#[derive(Debug, Clone)]
pub(crate) struct Meta {
    pub(super) n: std::num::NonZeroUsize,
    pub(super) kind: Option<SpanKind>,
    pub(super) anchor: Option<Anchor>,
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
pub(crate) fn render_meta(n: usize, kind: &SpanKind, a: &Anchor) -> String {
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

/// The `at=after` anchor text of a well-formed `%% cv-fold … %%` line, if it has one.
pub(crate) fn meta_after_text(line: &str) -> Option<String> {
    match parse_meta(line)? {
        Ok(Meta {
            anchor: Some(Anchor { after, .. }),
            ..
        }) => after.map(|(t, _)| t),
        _ => None,
    }
}

/// A `%% cv-fold … %%` line without a positive `lines=` count (its fold reads as old layout).
#[derive(Debug, Clone, Copy)]
pub(crate) struct MalformedMeta;

/// `Some` when `line` is a `%% cv-fold … %%` metadata line: `Ok` when well-formed.
pub(crate) fn parse_meta(line: &str) -> Option<Result<Meta, MalformedMeta>> {
    let inner = blocks::single_comment(line.trim())?;
    let rest = inner.strip_prefix(META_NAME)?;
    if !(rest.is_empty() || rest.starts_with(' ')) {
        return None;
    }
    let (fields, text) = rest.split_once(" | ").unwrap_or((rest, ""));
    let (mut n, mut kind) = (0usize, None);
    let (mut at, mut occ, mut gap, mut join) = ("", 0usize, 0usize, false);
    for tok in fields.split_whitespace() {
        let Some((k, v)) = tok.split_once('=') else {
            continue;
        };
        match k {
            "lines" => n = v.parse().unwrap_or(0),
            "kind" => kind = Some(SpanKind::from_str(&unesc(v))),
            "at" => at = v,
            "occ" => occ = v.parse().unwrap_or(0),
            "gap" => gap = v.parse().unwrap_or(0),
            "join" => join = v == "1",
            _ => {}
        }
    }
    let Some(n) = std::num::NonZeroUsize::new(n) else {
        return Some(Err(MalformedMeta));
    };
    let text = squeeze(&unesc(text));
    let anchor = match at {
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
    Some(Ok(Meta { n, kind, anchor }))
}

/// The anchor a group leaves when it is cut after the surviving lines `kept` (indices into `sq`,
/// the squeezed lines). `hidden(k)` marks a generated line: never an anchor, not counted in `gap`;
/// a `start` anchor also skips one blank line after a leading generated block, as [`anchor_pos`]
/// resolves it.
pub(crate) fn anchor_of(
    kept: &[usize],
    sq: &[String],
    hidden: impl Fn(usize) -> bool,
    join: bool,
) -> Anchor {
    let plain = |ks: &[usize]| ks.iter().filter(|&&k| !hidden(k)).count();
    match kept.iter().rposition(|&k| !hidden(k) && !sq[k].is_empty()) {
        Some(q) => {
            let t = sq[kept[q]].clone();
            let occ = kept[..=q]
                .iter()
                .filter(|&&k| !hidden(k) && sq[k] == t)
                .count();
            Anchor {
                after: Some((t, occ)),
                gap: plain(&kept[q + 1..]),
                join,
            }
        }
        None => {
            let lead = kept.iter().take_while(|&&k| hidden(k)).count();
            let skip = usize::from(lead > 0 && kept.get(lead).is_some_and(|&k| sq[k].is_empty()));
            Anchor {
                after: None,
                gap: plain(&kept[lead..]) - skip,
                join,
            }
        }
    }
}
