//! The enrich pass: its phases in order, with [`assemble`] beside phase 5. Decisions recorded
//! here:
//! - **Anchors** compare whitespace-squeezed text ([`squeeze`]), never fuzzy. A span's
//!   `first_line` matches a whole line not already claimed by an earlier span (so a repeated chrome
//!   block resolves to its successive occurrences); `last_line` is the first whole-line match at or
//!   after it. `starts_at` is the first line whose squeezed text starts with the anchor, else the
//!   first line containing it. `content_ends_after` is the *last* whole-line match (else
//!   the last line starting with it); every non-blank line after it becomes one `footer` span.
//! - **Banners** ([`BannerRef`]) are the exact embed text; every line consisting solely of that
//!   embed (squeezed equality) folds. Line indices were rejected: they drift across edits.
//! - **Headings**: when the body (outside code fences and outside folded lines) has markdown
//!   headings, chapters are ignored; otherwise resolved chapters become `## <title>` headings.
//!   Chapters with unresolved anchors or duplicate lines are dropped; with existing headings the
//!   chapters are neither resolved nor counted. No top block is rendered (plan
//!   abstract-removal-confidence-deletion WP-2, Decision 1: the `[!abstract]-` block, its ToC and
//!   the claim-quote anchors are gone; a legacy one is removed by the re-run's strip).
//! - **Inserted headings** are preceded by a `%% cv-enrich heading %%` marker line and followed by
//!   one blank line; a re-run strips marker + heading + blank before deciding "has headings".
//! - **Deletion tier** (plan abstract-removal-confidence-deletion WP-3, Decision 3): a fold run
//!   whose every line only model spans claim, each choosing deletion ([`deletes`]), outside code
//!   fences, without a Keep label and with no line whose squeezed text equals a prior fold
//!   group's `after` anchor text, leaves the body with no fold copy ([`plan_deletion`]); anything
//!   else folds as before. Deleted lines count toward the half-body guard.

use std::collections::BTreeSet;

use cv_text::blocks;
use cv_text::sections::heading_text;

use super::body::link_safe;
use super::{
    BannerRef, EnrichOpts, EnrichOutput, EnrichReport, MARK_HEADING, MARK_OPEN, SpanKind, deletes,
    squeeze,
};
use crate::fold::{Anchor, FOLD_HEADER, anchor_of, closing, quote, render_meta, write_fold};

/// Folded line index → the kind of the span that claimed it first.
type Kinds = std::collections::BTreeMap<usize, SpanKind>;

/// Claimed line index → whether every claim on it chose deletion ([`deletes`]); a
/// `content_ends_after` or banner claim is always `false`.
pub(super) type Sure = std::collections::BTreeMap<usize, bool>;

/// Phase 1: the line indices the model's spans and `content_ends_after` fold, and each folded
/// line's span kind (the first span claiming a line wins). A span that does not resolve against
/// the body but whose both anchors are lines of the prior fold (`prior`, squeezed and unquoted)
/// is counted as `anchors_already_folded`, not dropped: an earlier run already applied it.
/// `sure` records, per claimed line, whether all its claims chose deletion under `threshold`.
pub(super) fn resolve_fold(
    output: &EnrichOutput,
    sq: &[String],
    prior: &[String],
    threshold: f64,
    sure: &mut Sure,
) -> (BTreeSet<usize>, Kinds, EnrichReport) {
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
                let del = deletes(span.confidence, threshold);
                for i in s..=e {
                    kinds.entry(i).or_insert_with(|| span.kind.clone());
                    *sure.entry(i).or_insert(true) &= del;
                }
            }
            None if !a.is_empty() && prior.contains(&a) && prior.contains(&b) => {
                rep.anchors_already_folded += 1;
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
                        sure.insert(i, false);
                    }
                }
            }
            None => rep.anchors_dropped += 1,
        }
    }
    (fold, kinds, rep)
}

/// Phase 2: add the banner lines to `fold`, then drop the whole fold when it would take more than
/// half of the non-blank lines (`fold` holds the lines to delete as well, so the guard counts
/// both and refuses both). Banner lines are never deleted (`sure` = `false`).
pub(super) fn banners_and_cap(
    mut fold: BTreeSet<usize>,
    kinds: &mut Kinds,
    sure: &mut Sure,
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
        kinds.entry(i).or_insert(SpanKind::Banner);
        sure.insert(i, false);
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

/// Phase 2b: the subset of `fold` deleted instead of folded. The unit is a maximal run of `fold`
/// (exactly a fold group of [`plan_groups`]): a run is deleted only when every line in it is
/// `sure`, outside a code fence, not Keep-labelled (`kept`), and its squeezed text (`sq`) equals
/// no prior group's `after` anchor text (`prior_meta`, any occurrence, earlier or later: deleting
/// it would orphan or retarget that group for good, where a fold stays reversible through Keep);
/// otherwise the whole run folds. The groups that stay are cut from the same runs.
pub(super) fn plan_deletion(
    lines: &[String],
    sq: &[String],
    fold: &BTreeSet<usize>,
    sure: &Sure,
    prior_meta: &[String],
    kept: impl Fn(&str) -> bool,
) -> BTreeSet<usize> {
    let fence = blocks::fenced_lines(lines);
    let anchors: BTreeSet<String> = prior_meta
        .iter()
        .filter_map(|m| crate::fold::meta_after_text(m))
        .map(|t| squeeze(&t))
        .collect();
    let ok = |i: usize| {
        sure.get(&i).copied().unwrap_or(false)
            && !fence[i]
            && !kept(&lines[i])
            && !anchors.contains(&sq[i])
    };
    let mut out = BTreeSet::new();
    let mut i = 0;
    while i < lines.len() {
        if !fold.contains(&i) {
            i += 1;
            continue;
        }
        let s = i;
        while i < lines.len() && fold.contains(&i) {
            i += 1;
        }
        if (s..i).all(ok) {
            out.extend(s..i);
        }
    }
    out
}

/// Phase 3's piece: the chapter headings to insert (line index, title).
pub(super) struct HeadingPlan {
    pub(super) inserts: Vec<(usize, String)>,
    pub(super) report: EnrichReport,
}

/// Phase 3: keep the existing headings, or resolve the model's chapters into inserted headings.
pub(super) fn plan_headings(
    output: &EnrichOutput,
    opts: EnrichOpts,
    lines: &[String],
    sq: &[String],
    fold: &BTreeSet<usize>,
) -> HeadingPlan {
    let mut rep = EnrichReport::default();
    let fence = blocks::fenced_lines(lines);
    let has_headings = (0..lines.len())
        .filter(|&i| !fence[i] && !fold.contains(&i))
        .any(|i| heading_text(&lines[i]).is_some());
    let mut inserts: Vec<(usize, String)> = Vec::new();
    if opts.headings && !has_headings {
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
    }
    rep.headings_inserted = inserts.len();
    HeadingPlan {
        inserts,
        report: rep,
    }
}

/// Phase 5's piece: one new fold group — its line indices, kind and position anchor.
pub(super) struct NewGroup {
    idx: Vec<usize>,
    kind: SpanKind,
    anchor: Anchor,
}

/// Phase 5's piece: the folded groups (maximal runs of folded lines) with the anchor each leaves
/// behind. Mirrors [`assemble`]'s walk (inserted headings never sit between a group and its anchor
/// line, so they are ignored) to learn which surviving line precedes each group, how many blank
/// lines sit between, and whether the doubled-blank collapse dropped the blank after the group.
pub(super) fn plan_groups(
    lines: &[String],
    sq: &[String],
    fold: &BTreeSet<usize>,
    kinds: &Kinds,
    deleted: &BTreeSet<usize>,
) -> (Vec<NewGroup>, usize) {
    let mut n_deleted = 0;
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
        // A deleted run leaves the body as its fold would, but no group (and no copy).
        if deleted.contains(&s) {
            n_deleted += 1;
            continue;
        }
        let anchor = anchor_of(&out, sq, |_| false, join);
        let kind = kinds
            .get(&s)
            .cloned()
            .unwrap_or_else(|| SpanKind::Other(String::new()));
        groups.push(NewGroup { idx, kind, anchor });
    }
    (groups, n_deleted)
}

/// Phase 5: the trailing `> [!removed]-` fold block lines (empty when nothing is folded): prior
/// metadata, then one `%% cv-fold … %%` line per new group, the header, the prior content carried
/// verbatim, then the new groups.
pub(super) fn render_fold(
    groups: &[NewGroup],
    prior_meta: &[String],
    prior_fold: &[String],
    lines: &[String],
) -> (Vec<String>, EnrichReport) {
    let rep = EnrichReport {
        spans_folded: groups.len(),
        ..EnrichReport::default()
    };
    if groups.is_empty() && prior_fold.is_empty() {
        return (Vec::new(), rep);
    }
    let head = std::iter::once(MARK_OPEN.to_owned())
        .chain(prior_meta.iter().cloned())
        .chain(
            groups
                .iter()
                .map(|g| render_meta(g.idx.len(), &g.kind, &g.anchor)),
        );
    let chunks = (!prior_fold.is_empty())
        .then(|| prior_fold.to_vec())
        .into_iter()
        .chain(
            groups
                .iter()
                .map(|g| g.idx.iter().map(|&i| quote(&lines[i])).collect()),
        );
    (write_fold(head, FOLD_HEADER, chunks, closing()), rep)
}

/// Phase 6: the body without folded lines, with the inserted headings and the fold block last.
pub(super) fn assemble(
    lines: &[String],
    fold: &BTreeSet<usize>,
    inserts: &[(usize, String)],
    fold_block: Vec<String>,
) -> String {
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + fold_block.len());
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
