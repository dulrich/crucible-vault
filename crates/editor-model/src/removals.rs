//! Plan `intake-removals-sources-rescan` WP-6: the pure line arithmetic behind "Trim selection as
//! removed". The daemon's `trim` form counts **body** lines (the frontmatter block removed, split
//! at `\n`); the editor's buffer and the render model count lines of the **full** note. This
//! module converts one to the other and turns a Source or Reading selection into whole lines.

use crate::render::RenderModel;
use crate::visible::{Part, VisRun};

/// Why a selection could not become a body line range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimError {
    /// Nothing is selected.
    NoSelection,
    /// The selected lines lie (partly) in the frontmatter, or outside the note body.
    NotInBody,
}

impl TrimError {
    /// The status-line text.
    pub fn text(self) -> &'static str {
        match self {
            TrimError::NoSelection => "Trim selection as removed: select the lines to remove first",
            TrimError::NotInBody => {
                "Trim selection as removed: the selection must be in the note body"
            }
        }
    }
}

/// The note body of `text`: everything after the frontmatter block, the split the daemon applies
/// before its fold operations (`cv-note`).
#[must_use]
pub fn body_of(text: &str) -> &str {
    let body = cv_note::FrontmatterDoc::new(text).body().len();
    &text[text.len() - body..]
}

/// How many lines the frontmatter block (fences included) takes off the top of `text`: body line
/// 0 is full-note line `body_line_base(text)`. Uses the daemon's own split (`cv-note`).
#[must_use]
pub fn body_line_base(text: &str) -> usize {
    let body = body_of(text).len();
    text[..text.len() - body].matches('\n').count()
}

/// The 0-based inclusive **body** range of full-note lines `first..=last` of `text`; an error
/// when any of them is a frontmatter line.
pub fn body_range(text: &str, first: usize, last: usize) -> Result<(usize, usize), TrimError> {
    let base = body_line_base(text);
    let total = text.matches('\n').count() + 1;
    if first > last || first < base || last >= total {
        return Err(TrimError::NotInBody);
    }
    Ok((first - base, last - base))
}

/// Whole lines of a Source selection: iced-style `(line, column)` positions of the cursor and
/// the other end of the selection (`None` = no selection). A selection that ends at column 0 of a
/// later line does not take that line (the usual "select whole lines" gesture).
pub fn source_lines(
    cursor: (usize, usize),
    selection: Option<(usize, usize)>,
) -> Result<(usize, usize), TrimError> {
    let other = selection.ok_or(TrimError::NoSelection)?;
    if other == cursor {
        return Err(TrimError::NoSelection);
    }
    let (lo, hi) = if other < cursor {
        (other, cursor)
    } else {
        (cursor, other)
    };
    let last = if hi.1 == 0 && hi.0 > lo.0 {
        hi.0 - 1
    } else {
        hi.0
    };
    Ok((lo.0, last))
}

/// Whole full-note lines of a Reading selection spanning visible runs `lo_run..=hi_run` with copy
/// text `sel_text`. A run knows only its top-level block, and a block knows its source line range
/// (`RenderModel::block_lines`), so the range starts as the blocks' lines; inside the first and
/// last block it narrows to the first and last line whose text contains the selection's first
/// and last non-blank line (a block drawn from several source lines). When the text cannot be
/// found (markup differs) the block bound stays: it never narrows wrongly.
pub fn reading_lines(
    model: &RenderModel,
    runs: &[VisRun],
    lo_run: usize,
    hi_run: usize,
    sel_text: &str,
    full_text: &str,
) -> Result<(usize, usize), TrimError> {
    if sel_text.trim().is_empty() {
        return Err(TrimError::NoSelection);
    }
    let block_of = |i: usize| match runs.get(i).map(|r| r.part) {
        Some(Part::Block(b)) => Some(b),
        _ => None,
    };
    let (Some(b0), Some(b1)) = (block_of(lo_run), block_of(hi_run)) else {
        return Err(TrimError::NotInBody);
    };
    let (Some(r0), Some(r1)) = (model.block_lines.get(b0), model.block_lines.get(b1)) else {
        return Err(TrimError::NotInBody);
    };
    let lines: Vec<&str> = full_text.split('\n').collect();
    let (s, e) = (r0.start, r1.end.min(lines.len()));
    if s >= e {
        return Err(TrimError::NotInBody);
    }
    let sel: Vec<&str> = sel_text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let (head, tail) = (sel[0], sel[sel.len() - 1]);
    let first = (r0.start..r0.end.min(lines.len()))
        .find(|&l| lines[l].contains(head))
        .unwrap_or(s);
    let last = (r1.start..e)
        .rev()
        .find(|&l| lines[l].contains(tail))
        .unwrap_or(e - 1);
    Ok(if first <= last {
        (first, last)
    } else {
        (s, e - 1)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::lower;

    const NOTE: &str = "---\ntitle: T\n---\nintro\n\nfirst line\nsecond line\n\nend\n";

    #[test]
    fn body_range_drops_the_frontmatter_lines() {
        assert_eq!(body_line_base(NOTE), 3);
        // Full-note line 5 is "first line" = body line 2 (0: "intro", 1: "", 2: "first line").
        assert_eq!(body_range(NOTE, 5, 6), Ok((2, 3)));
        assert_eq!(body_line_base("no frontmatter\nx"), 0);
        assert_eq!(body_range("a\nb\nc", 1, 2), Ok((1, 2)));

        // A frontmatter or out-of-range line is refused.
        assert_eq!(body_range(NOTE, 1, 4), Err(TrimError::NotInBody));
        assert_eq!(body_range(NOTE, 4, 99), Err(TrimError::NotInBody));
        assert_eq!(body_range(NOTE, 6, 5), Err(TrimError::NotInBody));
    }

    #[test]
    fn selections_expand_to_whole_lines() {
        assert_eq!(source_lines((2, 3), None), Err(TrimError::NoSelection));
        assert_eq!(
            source_lines((2, 3), Some((2, 3))),
            Err(TrimError::NoSelection)
        );
        assert_eq!(source_lines((4, 2), Some((2, 5))), Ok((2, 4)));
        assert_eq!(source_lines((2, 5), Some((4, 2))), Ok((2, 4)));
        // Ending at column 0 of a later line leaves that line out.
        assert_eq!(source_lines((5, 0), Some((3, 4))), Ok((3, 4)));
        assert_eq!(source_lines((3, 0), Some((3, 4))), Ok((3, 3)));

        // Reading: a selection maps through the blocks' source lines.
        let model = lower(NOTE);
        let runs = crate::visible::visible_runs(&model);
        let run_of = |needle: &str| {
            runs.iter()
                .position(|r| r.text.contains(needle))
                .expect("run")
        };
        // The "first line / second line" paragraph is source lines 5..7; selecting from the
        // second line narrows to it.
        let i = run_of("first line");
        assert_eq!(
            reading_lines(&model, &runs, i, i, "second line", NOTE),
            Ok((6, 6))
        );
        assert_eq!(
            reading_lines(&model, &runs, i, i, "", NOTE),
            Err(TrimError::NoSelection)
        );
        // Properties runs have no block.
        assert_eq!(
            reading_lines(&model, &runs, 0, 0, "title", NOTE),
            Err(TrimError::NotInBody)
        );
        let j = run_of("end");
        assert_eq!(
            reading_lines(&model, &runs, run_of("intro"), j, "intro\n\nend", NOTE),
            Ok((3, 8))
        );
    }
}
