//! The frontmatter/body split of a note: the one split the editor's Ctrl+A and the removals trim
//! share with the daemon's fold operations (`cv_note::span`: a BOM, CRLF fences and the empty
//! `---\n---` block count; an unclosed block is no frontmatter).

use std::ops::Range;

/// The byte offset where `text`'s body begins: past the closing fence line, else 0.
fn start(text: &str) -> usize {
    cv_note::span(text).map_or(0, |s| s.end)
}

/// The note body of `text`: everything after the frontmatter block.
#[must_use]
pub fn of(text: &str) -> &str {
    &text[start(text)..]
}

/// The byte range of `text`'s body; `None` when there is no closed frontmatter block or the body
/// is empty.
#[must_use]
pub fn range(text: &str) -> Option<Range<usize>> {
    let start = cv_note::span(text)?.end;
    (start < text.len()).then_some(start..text.len())
}

/// How many lines the frontmatter block (fences included) takes off the top of `text`: body line
/// 0 is full-note line `line_base(text)`.
#[must_use]
pub fn line_base(text: &str) -> usize {
    text[..start(text)].matches('\n').count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_frontmatter_from_body() {
        let lf = "---\na: 1\n---\nbody\n";
        assert_eq!(range(lf), Some(13..lf.len()));
        assert_eq!(of(lf), "body\n");
        assert_eq!(line_base(lf), 3);
    }

    #[test]
    fn no_frontmatter_is_all_body() {
        assert_eq!(range("plain\ntext"), None);
        assert_eq!(of("plain\ntext"), "plain\ntext");
        assert_eq!(line_base("plain\ntext"), 0);
    }

    #[test]
    fn unclosed_block_is_no_frontmatter() {
        let t = "---\na: 1\nbody\n";
        assert_eq!(range(t), None);
        assert_eq!(of(t), t);
        assert_eq!(line_base(t), 0);
    }

    #[test]
    fn crlf_and_bom_fences_split() {
        let crlf = "---\r\na: 1\r\n---\r\nbody\r\n";
        assert_eq!(&crlf[range(crlf).unwrap()], "body\r\n");
        assert_eq!(of(crlf), "body\r\n");
        assert_eq!(line_base(crlf), 3);
        let bom = "\u{feff}---\na: 1\n---\nbody";
        assert_eq!(&bom[range(bom).unwrap()], "body");
        assert_eq!(of(bom), "body");
    }

    #[test]
    fn body_less_note_has_no_range() {
        assert_eq!(range("---\na: 1\n---\n"), None);
        assert_eq!(range("---\na: 1\n---"), None);
        assert_eq!(of("---\na: 1\n---\n"), "");
        assert_eq!(line_base("---\na: 1\n---\n"), 3);
    }
}
