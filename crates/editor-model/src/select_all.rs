//! Round-25 WP-2: the Ctrl+A toggle (pure). The first press selects the note body (everything
//! after the frontmatter); pressed while exactly the body is selected (either direction), it
//! selects the whole note; pressed again, the body. No frontmatter / no body → everything.

use crate::body;
use crate::reading::{SelPos, Selection};
use crate::visible::{Part, VisRun};

fn whole(runs: &[VisRun]) -> Option<Selection> {
    let last = runs.len().checked_sub(1)?;
    Some(Selection::new(
        SelPos::new(0, 0),
        SelPos::new(last, runs[last].text.len()),
    ))
}

/// The next Reading Ctrl+A selection over `runs` given the view's `current` selection.
/// `None` only when nothing is rendered.
pub fn select_all_next(runs: &[VisRun], current: Option<Selection>) -> Option<Selection> {
    let all = whole(runs)?;
    let Some(first_body) = runs.iter().position(|r| r.part != Part::Properties) else {
        return Some(all);
    };
    if first_body == 0 {
        return Some(all);
    }
    let body = Selection::new(SelPos::new(first_body, 0), all.focus);
    // Ordered endpoints, so a backwards selection of exactly the body also toggles (audit E1).
    if current.is_some_and(|c| c.ordered() == body.ordered()) {
        Some(all)
    } else {
        Some(body)
    }
}

/// The next Source Ctrl+A selection as `(anchor, cursor)` byte offsets, given the current
/// selection endpoints (`None` anchor = no selection). Endpoints are normalised, so a backwards
/// selection of exactly the body also toggles to the whole note.
pub fn source_select_all_next(text: &str, anchor: Option<usize>, cursor: usize) -> (usize, usize) {
    let all = (0, text.len());
    let Some(body) = body::range(text) else {
        return all;
    };
    let cur = anchor.map(|a| (a.min(cursor), a.max(cursor)));
    if cur == Some((body.start, body.end)) {
        all
    } else {
        (body.start, body.end)
    }
}

/// `(line, column)` (column in bytes) of byte `offset` in `text`.
pub fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let head = &text[..offset.min(text.len())];
    let line = head.matches('\n').count();
    let col = head.len() - head.rfind('\n').map_or(0, |i| i + 1);
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typography::Display;
    use crate::visible::Sep;

    fn run(text: &str, part: Part) -> VisRun {
        VisRun {
            display: Display::plain(text.into()),
            sep: Sep::Block,
            prefix: String::new(),
            image: false,
            segments: Vec::new(),
            part,
        }
    }

    fn sel(a: (usize, usize), f: (usize, usize)) -> Selection {
        Selection::new(SelPos::new(a.0, a.1), SelPos::new(f.0, f.1))
    }

    #[test]
    fn reading_toggle() {
        assert_eq!(select_all_next(&[], None), None);
        // No properties: everything.
        let body_only = [run("ab", Part::Block(0)), run("cd", Part::Block(1))];
        assert_eq!(select_all_next(&body_only, None), Some(sel((0, 0), (1, 2))));
        // Properties only: everything.
        let props = [run("k", Part::Properties), run("v", Part::Properties)];
        assert_eq!(select_all_next(&props, None), Some(sel((0, 0), (1, 1))));
        // Properties + body: body, then whole, then body.
        let both = [
            run("k", Part::Properties),
            run("v", Part::Properties),
            run("ab", Part::Block(0)),
            run("cd", Part::Block(1)),
        ];
        let body = sel((2, 0), (3, 2));
        let whole = sel((0, 0), (3, 2));
        assert_eq!(select_all_next(&both, None), Some(body));
        assert_eq!(select_all_next(&both, Some(body)), Some(whole));
        assert_eq!(select_all_next(&both, Some(whole)), Some(body));
        // A backwards selection of exactly the body also toggles to the whole note.
        assert_eq!(
            select_all_next(&both, Some(sel((3, 2), (2, 0)))),
            Some(whole)
        );
        // A partial selection selects the body.
        assert_eq!(
            select_all_next(&both, Some(sel((2, 1), (3, 2)))),
            Some(body)
        );
    }

    #[test]
    fn source_toggle_both_directions() {
        let t = "---\na: 1\n---\nbody\n";
        let all = (0, t.len());
        let body = (13, t.len());
        assert_eq!(source_select_all_next(t, None, 3), body);
        assert_eq!(source_select_all_next(t, Some(13), t.len()), all);
        // Backwards: anchor at the end, cursor at the body start.
        assert_eq!(source_select_all_next(t, Some(t.len()), 13), all);
        assert_eq!(source_select_all_next(t, Some(0), t.len()), body);
        assert_eq!(source_select_all_next("no fm", None, 0), (0, 5));
        assert_eq!(source_select_all_next("---\na: 1\n---\n", None, 0), (0, 13));
    }

    #[test]
    fn line_col_maps_offsets() {
        assert_eq!(line_col("ab\ncd\n", 0), (0, 0));
        assert_eq!(line_col("ab\ncd\n", 4), (1, 1));
        assert_eq!(line_col("ab\ncd\n", 6), (2, 0));
    }
}
