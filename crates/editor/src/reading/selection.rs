//! Plan round-5 WP-7: the Reading selection's copy text (pure; no iced).
//!
//! The run table is [`editor_model::visible::visible_runs`] (the one walk the view
//! draws from and find counts over), so run `i` is the `i`-th
//! [`crate::widget::selectable::Run`] on screen.

use editor_model::visible::VisRun;

use crate::widget::selectable::Selection;

/// The largest char boundary of `s` at or below `i`.
fn floor(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// The copy text of `sel` over `runs` (empty when collapsed).
pub fn copy_text(runs: &[VisRun], sel: Selection) -> String {
    let (lo, hi) = sel.ordered();
    let mut out = String::new();
    if lo == hi || runs.is_empty() {
        return out;
    }
    let last = hi.run.min(runs.len() - 1);
    for (i, r) in runs.iter().enumerate().take(last + 1).skip(lo.run) {
        let a = if i == lo.run {
            floor(&r.text, lo.byte)
        } else {
            0
        };
        let b = if i == hi.run {
            floor(&r.text, hi.byte)
        } else {
            r.text.len()
        };
        if i > lo.run {
            out.push_str(r.sep.as_str());
        }
        if i != lo.run || (a == 0 && hi.run > lo.run) {
            out.push_str(&r.prefix);
        }
        // Round-25 WP-3: the selection speaks display bytes; copy translates once, to source.
        let (a, b) = (r.to_source(a), r.to_source(b));
        if a < b {
            out.push_str(&r.src()[a..b]);
        }
    }
    out
}

/// Ctrl+A: the body first, then the whole document, then the body (see
/// `editor_model::select_all`).
pub fn all(runs: &[VisRun], current: Option<Selection>) -> Option<Selection> {
    editor_model::select_all::select_all_next(runs, current)
}
