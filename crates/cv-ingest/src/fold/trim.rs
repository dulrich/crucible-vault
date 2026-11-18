//! The fold review operations that edit lines directly: trim body lines and fold body ranges.

use cv_text::blocks::{self, LineClass};

use super::FoldError;
use super::layout::{FOLD_HEADER, closing, parse_folds, quote, write_fold};
use super::meta::{anchor_of, render_meta};
use crate::enrich::{MARK_OPEN, SpanKind, squeeze};

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
    check_range(&blocks::generated(&raw).classes, first, last)?;
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

/// Refuse a line range [`trim_lines`] and [`fold_ranges`] cannot take (`classes` = one per line).
fn check_range(classes: &[LineClass], first: usize, last: usize) -> Result<(), FoldError> {
    if first > last {
        return Err(FoldError::EmptyRange);
    }
    if last >= classes.len() {
        return Err(FoldError::OutOfBounds {
            last,
            lines: classes.len(),
        });
    }
    // Only block lines and the heading marker are generated here; an inserted heading and its
    // blank are ordinary lines a user may remove.
    if classes[first..=last]
        .iter()
        .any(|c| matches!(c, LineClass::Block | LineClass::HeadingMarker))
    {
        return Err(FoldError::Generated);
    }
    Ok(())
}

/// One [`fold_ranges`] group: body lines `first..=last` (0-based, inclusive) and their kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoldRange {
    pub first: usize,
    pub last: usize,
    pub kind: SpanKind,
}

/// Move body lines into the note's `[!removed]` fold without a model call, one group per
/// [`FoldRange`] — ascending and disjoint, line indices split at `\n` as [`trim_lines`], of a
/// frontmatter-free body; adjacent ranges stay separate groups. The groups are written exactly as
/// [`enrich`] writes them (one `%% cv-fold … %%` metadata line each, `> `-quoted content),
/// appended to the body's last fold block (its existing lines kept byte-for-byte), or to a new
/// fold block at the end of the body; everything else stays byte-identical. [`fold_groups`]
/// lists them and [`resolve_groups`] Keep restores each at its anchor.
///
/// # Errors
/// As [`trim_lines`] for each range; [`FoldError::Unordered`] when ranges overlap or are out of
/// order.
pub fn fold_ranges(body: &str, ranges: &[FoldRange]) -> Result<String, FoldError> {
    let raw: Vec<&str> = body.split('\n').collect();
    let classes = blocks::generated(&raw).classes;
    let mut prev: Option<usize> = None;
    for r in ranges {
        check_range(&classes, r.first, r.last)?;
        if prev.is_some_and(|p| r.first <= p) {
            return Err(FoldError::Unordered);
        }
        prev = Some(r.last);
    }
    if ranges.is_empty() {
        return Ok(body.to_owned());
    }
    let folds = parse_folds(&raw);
    // Generated: top block, heading marker + heading + blank, fold blocks.
    let hidden = |k: usize| classes[k].is_generated();
    let sq: Vec<String> = raw.iter().map(|l| squeeze(l)).collect();
    let blank = |k: usize| sq[k].is_empty();
    let in_fold = |k: usize| folds.iter().any(|f| (f.open..=f.close).contains(&k));
    let mut out: Vec<usize> = Vec::with_capacity(raw.len());
    let mut metas: Vec<String> = Vec::with_capacity(ranges.len());
    let mut chunks: Vec<Vec<String>> = Vec::with_capacity(ranges.len());
    let mut i = 0;
    for r in ranges {
        out.extend(i..r.first);
        i = r.last + 1;
        // As `plan_groups` over `strip_generated`'s lines: the blank run in front of a fold block
        // (or the end) is layout, so a group followed only by it never joins.
        let join = out.last().is_none_or(|&k| blank(k))
            && raw.get(i).is_some_and(|x| x.trim().is_empty())
            && (i + 1..raw.len()).any(|k| !blank(k) && !in_fold(k));
        if join {
            i += 1;
        }
        let anchor = anchor_of(&out, &sq, hidden, join);
        metas.push(render_meta(r.last - r.first + 1, &r.kind, &anchor));
        chunks.push(raw[r.first..=r.last].iter().map(|x| quote(x)).collect());
    }
    out.extend(i..raw.len());
    let owned = |ks: &[&str]| ks.iter().map(|l| (*l).to_owned()).collect::<Vec<_>>();
    let mut res: Vec<String> = Vec::with_capacity(out.len() + metas.len() * 2 + 8);
    let Some(fold) = folds.last() else {
        res.extend(out.iter().map(|&k| raw[k].to_owned()));
        while res.last().is_some_and(|l| l.trim().is_empty()) {
            res.pop();
        }
        res.push(String::new());
        let head = std::iter::once(MARK_OPEN.to_owned()).chain(metas);
        res.extend(write_fold(head, FOLD_HEADER, chunks, closing()));
        res.push(String::new());
        return Ok(res.join("\n"));
    };
    let header = (fold.open + 1..fold.close)
        .find(|&k| raw[k].trim_end() == FOLD_HEADER)
        .unwrap_or(fold.open + 1);
    let last = (header..fold.close)
        .rev()
        .find(|&k| !raw[k].trim().is_empty())
        .unwrap_or(header);
    let mut chunks = Some(chunks);
    for k in out {
        if k == fold.open {
            // A group taken from just above the fold leaves one blank line, as `enrich`.
            while res.len() >= 2 && res[res.len() - 2..].iter().all(|l| l.trim().is_empty()) {
                res.pop();
            }
            let head = owned(&raw[fold.open..header])
                .into_iter()
                .chain(metas.drain(..));
            let stored = (last != header).then(|| owned(&raw[header + 1..=last]));
            let all = stored.into_iter().chain(chunks.take().unwrap_or_default());
            res.extend(write_fold(
                head,
                raw[header],
                all,
                owned(&raw[last + 1..=fold.close]),
            ));
        } else if !(fold.open..=fold.close).contains(&k) {
            res.push(raw[k].to_owned());
        }
    }
    Ok(res.join("\n"))
}
