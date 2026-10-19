//! Selectable rich text for the Reading view (plan round-5 WP-7), grown from the spike widget
//! `docs/spikes/reading-select/src/selectable.rs` (verdict GO with caveat).
//!
//! Two widgets:
//! - [`Run`] — one text run (a paragraph, heading, table cell, code block, property row,
//!   callout title or transclusion head): styled spans, find highlights, link hover underline,
//!   and the selected byte range painted under the text. It owns no gesture logic; it exposes
//!   its shaped state to the root through `Operation::custom`.
//! - [`Root`] — wraps the Reading scrollable and owns every gesture: press/drag (through gaps
//!   between runs and past the last one), auto-scroll at the viewport edge, double-click word,
//!   triple-click run, Shift+click, Shift+Arrows, Esc, and link click vs drag (4 px threshold,
//!   the spike's). The selection itself lives on the Reading tab; the root reads it and
//!   publishes changes.
//!
//! **Hard newlines (audit E3):** iced's `hit_test` byte offsets are per hard line
//! (`iced_graphics-0.14.0/src/text/paragraph.rs:262-265`), so a run whose text contains `\n`
//! (code blocks, paragraphs with line breaks) is shaped as one newline-free paragraph per hard
//! line; each keeps its byte offset into the run text, so every position the widget reports is a
//! byte offset into the run's full text (= its copy text).
//!
//! **Caveat (spike):** the `Paragraph` trait exposes no visual-line byte ranges; they are
//! recovered by probing `hit_test` at each line's left and right edge ([`line_ranges`]).

mod ops;
mod root;
mod run;

pub use ops::{RunBox, probe_caret, probe_runs};
pub use root::{Root, root};
pub use run::{Run, RunState, line_ranges, run};

use unicode_segmentation::UnicodeSegmentation;

/// A position in the Reading selection: run index (render order) and byte offset in its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct SelPos {
    pub run: usize,
    pub byte: usize,
}

impl SelPos {
    pub const fn new(run: usize, byte: usize) -> Self {
        Self { run, byte }
    }
}

/// A Reading selection: where it started (`anchor`) and where it is extended to (`focus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Selection {
    pub anchor: SelPos,
    pub focus: SelPos,
}

impl Selection {
    pub const fn new(anchor: SelPos, focus: SelPos) -> Self {
        Self { anchor, focus }
    }

    /// `(start, end)` in document order.
    pub fn ordered(self) -> (SelPos, SelPos) {
        if self.anchor <= self.focus {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    /// Anchor and focus coincide (nothing selected).
    pub fn is_empty(self) -> bool {
        self.anchor == self.focus
    }

    /// `pos` lies inside a non-empty selection (ends included).
    pub fn contains(self, pos: SelPos) -> bool {
        let (lo, hi) = self.ordered();
        !self.is_empty() && lo <= pos && pos <= hi
    }

    /// The byte range of run `run` (text length `len`) inside the selection, if any part is.
    pub fn run_range(self, run: usize, len: usize) -> Option<(usize, usize)> {
        let (lo, hi) = self.ordered();
        if lo == hi || run < lo.run || run > hi.run {
            return None;
        }
        let a = if run == lo.run { lo.byte.min(len) } else { 0 };
        let b = if run == hi.run { hi.byte.min(len) } else { len };
        (a < b).then_some((a, b))
    }
}

/// The word (unicode word bounds) around `byte` in `text`.
pub fn word_at(text: &str, byte: usize) -> (usize, usize) {
    let byte = byte.min(text.len());
    let mut last = (text.len(), text.len());
    for (i, w) in text.split_word_bound_indices() {
        let end = i + w.len();
        if byte < end {
            // Whitespace runs are words of their own; prefer the word before a trailing space.
            if w.chars().all(char::is_whitespace) && byte == i && i > 0 {
                return last;
            }
            return (i, end);
        }
        last = (i, end);
    }
    last
}

/// The start of the grapheme before `byte` (0 at the start).
pub(super) fn prev_grapheme(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(i, _)| i)
}

/// The end of the grapheme at `byte` (`text.len()` at the end).
pub(super) fn next_grapheme(text: &str, byte: usize) -> usize {
    let byte = byte.min(text.len());
    text[byte..]
        .graphemes(true)
        .next()
        .map_or(text.len(), |g| byte + g.len())
}

/// Hard-line start offsets of `text` (always starts with 0).
pub fn hard_lines(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i + 1))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::widget::text::Span;

    #[test]
    fn split_lines_maps_hard_lines_to_run_offsets() {
        let spans: Vec<Span<'_, ()>> =
            vec![Span::new("ab\ncd"), Span::new("e\n"), Span::new("\nf")];
        let lines = run::split_lines(&spans);
        let starts: Vec<usize> = lines.iter().map(|l| l.0).collect();
        assert_eq!(starts, hard_lines("ab\ncde\n\nf"));
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.1.iter().map(|s| s.text.as_ref()).collect())
            .collect();
        assert_eq!(texts, ["ab", "cde", "", "f"]);
    }

    #[test]
    fn word_and_run_ranges() {
        let t = "hello brave world";
        assert_eq!(word_at(t, 7), (6, 11));
        assert_eq!(word_at(t, 6), (6, 11));
        assert_eq!(word_at(t, t.len()), (12, 17));
        let sel = Selection::new(SelPos::new(2, 3), SelPos::new(0, 1));
        assert_eq!(sel.run_range(0, 5), Some((1, 5)));
        assert_eq!(sel.run_range(1, 4), Some((0, 4)));
        assert_eq!(sel.run_range(2, 9), Some((0, 3)));
        assert_eq!(sel.run_range(3, 9), None);
        assert!(sel.contains(SelPos::new(1, 0)) && !sel.contains(SelPos::new(2, 4)));
        assert_eq!(sel.ordered(), (SelPos::new(0, 1), SelPos::new(2, 3)));
    }
}
