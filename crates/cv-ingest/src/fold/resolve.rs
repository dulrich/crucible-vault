//! The fold review operations: list the groups of a note body and resolve them (Keep/Delete).

use std::fmt;

use cv_text::blocks;

use super::layout::{PFold, PGroup, group_ids, parse_folds, rebuild_fold};
use super::meta::Anchor;
use crate::enrich::{SpanKind, squeeze};

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
    /// Fold ranges overlap or are out of order.
    Unordered,
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
            Self::Unordered => write!(f, "line ranges overlap or are out of order"),
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
    let classes = blocks::generated(raw).classes;
    let mut i = 0;
    while i < raw.len() {
        if let Some(b) = folds.iter().position(|f| f.open == i) {
            segs.push((Cls::Fold(b), String::new()));
            i = folds[b].close + 1;
            continue;
        }
        let cls = if classes[i].is_generated() {
            Cls::Hidden
        } else {
            Cls::Plain
        };
        segs.push((cls, raw[i].to_owned()));
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
