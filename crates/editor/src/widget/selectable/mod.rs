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

/// Plan round-22 WP-2: what the widgets need to know about a span's link type — whether it is a
/// copy payload (inline code: hover shows a copy glyph, no underline, no link menu) rather than
/// a navigation link.
pub trait SpanLink {
    /// Whether this link is a copy payload.
    fn is_copy(&self) -> bool {
        false
    }
}

impl SpanLink for () {}

mod ops;
mod root;
mod run;

pub use ops::{RunBox, probe_caret, probe_copy_glyph, probe_runs};
pub use root::{Root, root};
pub use run::{Run, RunState, line_ranges, run};

pub use editor_model::reading::{
    SelPos, Selection, hard_lines, line_containing, next_grapheme, next_word_boundary,
    prev_grapheme, prev_word_boundary, word_at,
};

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
}
