//! The enrich pass: its phases in order, with [`assemble`] beside phase 5. Decisions recorded
//! here:
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

use std::collections::BTreeSet;

use cv_text::blocks;
use cv_text::sections::heading_text;

use super::body::link_safe;
use super::{
    BannerRef, EnrichOpts, EnrichOutput, EnrichReport, MARK_CLOSE, MARK_HEADING, MARK_OPEN,
    SpanKind, squeeze,
};
use crate::fold::{Anchor, FOLD_HEADER, anchor_of, closing, quote, render_meta, write_fold};

const TOP_HEADER: &str = "> [!abstract]-";

/// Folded line index → the kind of the span that claimed it first.
type Kinds = std::collections::BTreeMap<usize, SpanKind>;

/// Phase 1: the line indices the model's spans and `content_ends_after` fold, and each folded
/// line's span kind (the first span claiming a line wins).
pub(super) fn resolve_fold(
    output: &EnrichOutput,
    sq: &[String],
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
pub(super) fn banners_and_cap(
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
        kinds.entry(i).or_insert(SpanKind::Banner);
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
pub(super) struct HeadingPlan {
    pub(super) toc: Vec<String>,
    pub(super) inserts: Vec<(usize, String)>,
    pub(super) report: EnrichReport,
}

/// Phase 3: link the existing headings, or resolve the model's chapters into inserted headings.
pub(super) fn plan_headings(
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
pub(super) fn render_top(
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
        let anchor = anchor_of(&out, sq, |_| false, join);
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

/// Phase 6: the body without folded lines, with the inserted headings, the top block first and
/// the fold block last.
pub(super) fn assemble(
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
